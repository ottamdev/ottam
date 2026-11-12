//! Installing the managed Node runtime, on request and with progress.
//!
//! Shaped like `AgentSignInService`: the request returns as soon as the work is
//! accepted, an id addresses it, and everything the user needs to watch arrives
//! as notifications. A download is minutes of work on a slow link — far too
//! long to hold a JSON-RPC response open.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use ottam_protocol::{RuntimeInstallEventParams, RuntimeInstallID, RuntimeInstallPhase};
use tokio_util::sync::CancellationToken;

use super::agent_install_service::{InstallOutcome, RecordedOutcome};
use super::managed_node::{InstallError, InstallProgress, ManagedNode};
use super::node_resolver::{NodeResolution, NodeSource};
use super::registry::AgentRegistry;
use super::types::{HarnessError, HarnessErrorKind};
use crate::net::{DownloadProgress, HttpClient};

pub(crate) type RuntimeInstallEventSink =
    Arc<dyn Fn(RuntimeInstallEventParams) + Send + Sync + 'static>;

pub(crate) struct RuntimeInstallService {
    registry: Arc<AgentRegistry>,
    http: HttpClient,
    events: RuntimeInstallEventSink,
    /// At most one install at a time: there is a single managed runtime, and a
    /// second concurrent download of the same tarball is pure waste.
    active: StdMutex<Option<ActiveInstall>>,
    /// How the last install ended short of `ready`, until the next starts.
    outcome: StdMutex<Option<RecordedOutcome>>,
}

struct ActiveInstall {
    install_id: RuntimeInstallID,
    phase: RuntimeInstallPhase,
    cancel: CancellationToken,
}

/// What a caller learns when an install is accepted.
pub(crate) struct AcceptedInstall {
    pub install_id: RuntimeInstallID,
    pub version: String,
    pub phase: RuntimeInstallPhase,
}

impl RuntimeInstallService {
    pub fn new(
        registry: Arc<AgentRegistry>,
        events: RuntimeInstallEventSink,
    ) -> Result<Self, HarnessError> {
        let http = HttpClient::new().map_err(|error| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                format!("could not start the downloader: {error}"),
            )
        })?;
        Ok(Self {
            registry,
            http,
            events,
            active: StdMutex::new(None),
            outcome: StdMutex::new(None),
        })
    }

    /// Begin an install, or join the one already running.
    ///
    /// Joining rather than refusing is deliberate: two windows both showing the
    /// setup UI is ordinary, and the second click should watch the first
    /// download rather than fail.
    pub fn start(self: &Arc<Self>) -> Result<AcceptedInstall, HarnessError> {
        let managed = self.registry.managed_node().ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                "the managed Node runtime is unavailable on this machine",
            )
        })?;
        let version = managed.version().to_string();

        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = active.as_ref() {
            return Ok(AcceptedInstall {
                install_id: running.install_id.clone(),
                version,
                phase: running.phase,
            });
        }

        let install_id = RuntimeInstallID(format!("runtime_install_{}", uuid::Uuid::now_v7()));
        let cancel = CancellationToken::new();
        self.set_outcome(None);
        *active = Some(ActiveInstall {
            install_id: install_id.clone(),
            phase: RuntimeInstallPhase::Starting,
            cancel: cancel.clone(),
        });
        drop(active);

        self.emit(
            &install_id,
            &version,
            RuntimeInstallPhase::Starting,
            None,
            None,
        );
        self.spawn(managed, install_id.clone(), version.clone(), cancel);

        Ok(AcceptedInstall {
            install_id,
            version,
            phase: RuntimeInstallPhase::Starting,
        })
    }

    /// Upgrade the managed runtime when an app update moved its pin past the
    /// one on disk — the older install is the standing consent, as for
    /// adapters. Agents keep running on the older runtime until this lands.
    /// With the pin installed, prune what it superseded instead.
    ///
    /// Only a runtime the agents run on is worth the download: when the
    /// resolver settles on a sufficient system Node, the managed one waits
    /// until it is needed again. Resolving can block on a login-shell probe,
    /// so the decision runs off the request path.
    pub fn reconcile(self: &Arc<Self>) {
        let Some(managed) = self.registry.managed_node() else {
            return;
        };
        if let Some(installed) = managed.superseded_version() {
            let service = Arc::clone(self);
            let version = managed.version().to_string();
            tokio::spawn(async move {
                let registry = Arc::clone(&service.registry);
                let resolution = tokio::task::spawn_blocking(move || registry.resolve_node())
                    .await
                    .unwrap_or(NodeResolution::Unresolved);
                if let NodeResolution::Resolved(runtime) = &resolution {
                    if runtime.source != NodeSource::Managed {
                        tracing::info!(
                            installed = %installed,
                            version = %version,
                            "managed Node pin moved, but agents run on another Node; not upgrading"
                        );
                        return;
                    }
                }
                tracing::info!(
                    installed = %installed,
                    version = %version,
                    "managed Node pin moved; upgrading in the background"
                );
                let _ = service.start();
            });
        } else if !self.is_installing() {
            self.registry
                .prune_unused(|in_use| managed.prune_superseded(in_use));
        }
    }

    /// Retry an upgrade whose last attempt failed at least a minute ago; see
    /// `reconcile`. A cancelled one waits for the next launch or an explicit
    /// install.
    pub fn retry_failed_upgrade(self: &Arc<Self>) {
        let failed = self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(RecordedOutcome::retry_due);
        let superseded = self
            .registry
            .managed_node()
            .is_some_and(|managed| managed.superseded_version().is_some());
        if failed && superseded {
            tracing::info!("retrying a failed managed Node upgrade on run start");
            let _ = self.start();
        }
    }

    pub fn is_installing(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// How the last install ended, if short of `ready` and nothing has
    /// started since.
    pub fn outcome(&self) -> Option<InstallOutcome> {
        self.outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|recorded| recorded.outcome().clone())
    }

    fn set_outcome(&self, outcome: Option<InstallOutcome>) {
        *self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = outcome.map(RecordedOutcome::new);
    }

    /// Stop an install in flight. A stale id is a no-op: a client racing the
    /// terminal event should not get an error for arriving second.
    pub fn cancel(&self, install_id: &RuntimeInstallID) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = active.as_ref() {
            if &running.install_id == install_id {
                running.cancel.cancel();
            }
        }
    }

    /// Stop the install in flight, if any, for core shutdown.
    pub fn cancel_all(&self) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = active.as_ref() {
            running.cancel.cancel();
        }
    }

    fn spawn(
        self: &Arc<Self>,
        managed: ManagedNode,
        install_id: RuntimeInstallID,
        version: String,
        cancel: CancellationToken,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let progress_service = Arc::clone(&service);
            let progress_id = install_id.clone();
            let progress_version = version.clone();
            let result = managed
                .install(&service.http, &cancel, move |progress| {
                    let (phase, received, total) = match progress {
                        InstallProgress::Download(DownloadProgress::Started { total }) => {
                            (RuntimeInstallPhase::Downloading, Some(0), total)
                        }
                        InstallProgress::Download(DownloadProgress::Received {
                            received,
                            total,
                        }) => (RuntimeInstallPhase::Downloading, Some(received), total),
                        InstallProgress::Download(DownloadProgress::Verifying) => {
                            (RuntimeInstallPhase::Verifying, None, None)
                        }
                        InstallProgress::Extracting => {
                            (RuntimeInstallPhase::Extracting, None, None)
                        }
                    };
                    progress_service.emit(&progress_id, &progress_version, phase, received, total);
                })
                .await;

            match result {
                Ok(node) => {
                    // The resolver caches a found toolchain for the process
                    // lifetime and the probe caches Ready for five minutes, so
                    // without this the freshly installed runtime stays invisible
                    // until something else invalidates them.
                    service.registry.invalidate_runtime_cache();
                    tracing::info!(node = %node.display(), "managed Node runtime installed");
                    // Runtimes a live adapter still runs on stay until a
                    // later reconcile; the idle ones go now.
                    service
                        .registry
                        .prune_unused(|in_use| managed.prune_superseded(in_use));
                    service.emit(
                        &install_id,
                        &version,
                        RuntimeInstallPhase::Ready,
                        None,
                        None,
                    );
                }
                Err(InstallError::Cancelled) => {
                    service.set_outcome(Some(InstallOutcome::Cancelled));
                    service.emit(
                        &install_id,
                        &version,
                        RuntimeInstallPhase::Cancelled,
                        None,
                        None,
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "managed Node install failed");
                    service.set_outcome(Some(InstallOutcome::Failed(error.to_string())));
                    service.emit_failure(&install_id, &version, error.to_string());
                }
            }
            service.finish(&install_id);
        });
    }

    fn emit(
        &self,
        install_id: &RuntimeInstallID,
        version: &str,
        phase: RuntimeInstallPhase,
        bytes_received: Option<u64>,
        bytes_total: Option<u64>,
    ) {
        self.record_phase(install_id, phase);
        (self.events)(RuntimeInstallEventParams {
            install_id: install_id.clone(),
            version: version.to_string(),
            phase,
            bytes_received,
            bytes_total,
            error: None,
        });
    }

    fn emit_failure(&self, install_id: &RuntimeInstallID, version: &str, error: String) {
        self.record_phase(install_id, RuntimeInstallPhase::Failed);
        (self.events)(RuntimeInstallEventParams {
            install_id: install_id.clone(),
            version: version.to_string(),
            phase: RuntimeInstallPhase::Failed,
            bytes_received: None,
            bytes_total: None,
            error: Some(error),
        });
    }

    fn record_phase(&self, install_id: &RuntimeInstallID, phase: RuntimeInstallPhase) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = active.as_mut() {
            if &running.install_id == install_id {
                running.phase = phase;
            }
        }
    }

    fn finish(&self, install_id: &RuntimeInstallID) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .as_ref()
            .is_some_and(|running| &running.install_id == install_id)
        {
            *active = None;
        }
    }
}
