//! Installing an agent's pinned ACP adapter, on request and with progress.
//!
//! Shaped like `RuntimeInstallService`: the request returns as soon as the
//! work is accepted, an id addresses it, and progress arrives as
//! notifications. Per-agent rather than app-scoped — each agent's adapter
//! installs independently, and a second `agent.install` for the same agent
//! joins the one in flight.
//!
//! The phase sequence is the product claim: `installing` is npm fetching the
//! pinned tree, `verifying` is a throwaway adapter completing its first ACP
//! handshake (the model-catalog probe), and `ready` is only emitted after
//! both. A handshake failure rolls the published version back, so "installed"
//! always means the adapter actually answered.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use ottam_protocol::{AgentID, AgentInstallEventParams, AgentInstallID, AgentInstallPhase};
use tokio_util::sync::CancellationToken;

use super::adapter_install::{
    binary_install_into, npm_install_into, AdapterInstallError, AdapterManifest,
    ManagedAdapterInstalls,
};
use super::model_catalog::ModelCatalogService;
use super::node_resolver::ResolvedNodeRuntime;
use super::registry::AgentRegistry;
use super::types::{
    AdapterPackage, AgentId, BinaryAdapter, HarnessError, HarnessErrorKind, RuntimeAvailability,
};
use crate::net::HttpClient;

pub(crate) type AgentInstallEventSink =
    Arc<dyn Fn(AgentInstallEventParams) + Send + Sync + 'static>;

pub(crate) struct AgentInstallService {
    registry: Arc<AgentRegistry>,
    catalog: Arc<ModelCatalogService>,
    events: AgentInstallEventSink,
    /// One slot per agent. A second start for the same agent joins the
    /// running install; different agents install concurrently.
    operations: StdMutex<BTreeMap<AgentId, AgentLifecycleOperation>>,
    /// How each agent's last install ended short of `ready`, until the next
    /// one starts. An update that failed is retried on a later run start;
    /// one the user cancelled waits for the next launch or an explicit click.
    outcomes: StdMutex<BTreeMap<AgentId, RecordedOutcome>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    Failed(String),
    Cancelled,
}

/// How long a failed background update waits before a run start retries it.
/// Offline, or failing the same way every time, an update would otherwise
/// re-download and fail with every message the user sends.
const RUN_START_RETRY_AFTER: Duration = Duration::from_secs(60);

/// An install's [`InstallOutcome`] and when it ended.
#[derive(Debug, Clone)]
pub(crate) struct RecordedOutcome {
    outcome: InstallOutcome,
    at: Instant,
}

impl RecordedOutcome {
    pub(crate) fn new(outcome: InstallOutcome) -> Self {
        Self {
            outcome,
            at: Instant::now(),
        }
    }

    pub(crate) fn outcome(&self) -> &InstallOutcome {
        &self.outcome
    }

    /// Whether a run start should retry it: it failed, a while ago.
    pub(crate) fn retry_due(&self) -> bool {
        matches!(self.outcome, InstallOutcome::Failed(_))
            && self.at.elapsed() >= RUN_START_RETRY_AFTER
    }
}

struct ActiveInstall {
    install_id: AgentInstallID,
    phase: AgentInstallPhase,
    cancel: CancellationToken,
}

enum AgentLifecycleOperation {
    Installing(ActiveInstall),
    Removing,
}

pub(crate) struct RemovalOperationGuard<'a> {
    operations: &'a StdMutex<BTreeMap<AgentId, AgentLifecycleOperation>>,
    agent_id: AgentId,
}

impl Drop for RemovalOperationGuard<'_> {
    fn drop(&mut self) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(
            operations.get(&self.agent_id),
            Some(AgentLifecycleOperation::Removing)
        ) {
            operations.remove(&self.agent_id);
        }
    }
}

/// What a caller learns when an install is accepted (or joined).
pub(crate) struct AcceptedAgentInstall {
    pub install_id: AgentInstallID,
    pub agent_id: AgentID,
    pub name: String,
    pub version: String,
    pub phase: AgentInstallPhase,
}

/// Where the install step gets the adapter from.
enum InstallSource<'a> {
    Npm {
        runtime: ResolvedNodeRuntime,
        node_version: Option<String>,
    },
    Binary {
        binary: &'a BinaryAdapter,
        http: HttpClient,
    },
}

/// Why an install stopped short of `ready`.
enum InstallFailure {
    Cancelled,
    Message(String),
}

impl From<HarnessError> for InstallFailure {
    fn from(error: HarnessError) -> Self {
        Self::Message(error.message)
    }
}

impl From<AdapterInstallError> for InstallFailure {
    fn from(error: AdapterInstallError) -> Self {
        match error {
            AdapterInstallError::Cancelled => Self::Cancelled,
            other => Self::Message(other.to_string()),
        }
    }
}

impl AgentInstallService {
    pub(crate) fn guard_profile_delete(
        &self,
        id: &AgentId,
    ) -> Result<RemovalOperationGuard<'_>, HarnessError> {
        let harness = AgentId::new(self.registry.definition(id)?.harness_id());
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if operations.contains_key(&harness) {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                "Wait for this provider's installation or removal to finish.",
            ));
        }
        operations.insert(harness.clone(), AgentLifecycleOperation::Removing);
        Ok(RemovalOperationGuard {
            operations: &self.operations,
            agent_id: harness,
        })
    }

    pub fn new(
        registry: Arc<AgentRegistry>,
        catalog: Arc<ModelCatalogService>,
        events: AgentInstallEventSink,
    ) -> Self {
        Self {
            registry,
            catalog,
            events,
            operations: StdMutex::new(BTreeMap::new()),
            outcomes: StdMutex::new(BTreeMap::new()),
        }
    }

    /// Begin an install for one agent, or join the one already running.
    pub fn start(
        self: &Arc<Self>,
        agent_id: &AgentId,
    ) -> Result<AcceptedAgentInstall, HarnessError> {
        let harness_agent = AgentId::new(self.registry.install_definition(agent_id)?.harness_id());
        let agent_id = &harness_agent;
        let definition = self.registry.install_definition(agent_id)?;
        let Some(package) = definition.adapter.clone() else {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                format!("{agent_id} has no installable adapter"),
            ));
        };
        let Some(installs) = self.registry.adapter_installs() else {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                "managed adapter installs are disabled on this machine",
            ));
        };

        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(operation) = operations.get(agent_id) {
            return match operation {
                AgentLifecycleOperation::Installing(running) => Ok(AcceptedAgentInstall {
                    install_id: running.install_id.clone(),
                    agent_id: AgentID(agent_id.0.clone()),
                    name: package.name,
                    version: package.version,
                    phase: running.phase,
                }),
                AgentLifecycleOperation::Removing => Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!("{agent_id}'s adapter is being removed; wait for it to finish"),
                )),
            };
        }

        let install_id = AgentInstallID(format!("agent_install_{}", uuid::Uuid::now_v7()));
        let cancel = CancellationToken::new();
        self.set_outcome(agent_id, None);
        operations.insert(
            agent_id.clone(),
            AgentLifecycleOperation::Installing(ActiveInstall {
                install_id: install_id.clone(),
                phase: AgentInstallPhase::Starting,
                cancel: cancel.clone(),
            }),
        );
        drop(operations);

        self.emit(
            agent_id,
            &install_id,
            &package,
            AgentInstallPhase::Starting,
            None,
        );
        self.spawn(
            agent_id.clone(),
            package.clone(),
            installs,
            install_id.clone(),
            cancel,
        );

        Ok(AcceptedAgentInstall {
            install_id,
            agent_id: AgentID(agent_id.0.clone()),
            name: package.name,
            version: package.version,
            phase: AgentInstallPhase::Starting,
        })
    }

    /// Remove the agent's installed adapter (every version), and optionally
    /// its isolated home — sessions, sign-in, and all. The running adapter is
    /// shut down first so nothing keeps executing out of a directory that is
    /// about to disappear. Refused while an install is in flight: the user
    /// cancels that, then removes.
    pub async fn uninstall(
        &self,
        agent_id: &AgentId,
        remove_home: bool,
    ) -> Result<(), HarnessError> {
        let requested = self.registry.definition(agent_id)?;
        if remove_home && requested.existing_home().is_some() {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                "Your CLI setup cannot be removed by Ottam.",
            ));
        }
        let requested_id = agent_id.clone();
        let harness_agent = AgentId::new(requested.harness_id());
        let agent_id = &harness_agent;
        let definition = self.registry.install_definition(agent_id)?;
        if definition.adapter.is_none() {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                format!("{agent_id} has no removable adapter"),
            ));
        }
        let Some(installs) = self.registry.adapter_installs() else {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                "managed adapter installs are disabled on this machine",
            ));
        };
        {
            let mut operations = self
                .operations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if operations.contains_key(agent_id) {
                return Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!("{agent_id}'s adapter has another operation in progress"),
                ));
            }
            operations.insert(agent_id.clone(), AgentLifecycleOperation::Removing);
        }
        let _operation = RemovalOperationGuard {
            operations: &self.operations,
            agent_id: agent_id.clone(),
        };
        let instances = self.registry.harness_instances(&harness_agent.0);
        let mut admissions = Vec::new();
        for instance in &instances {
            admissions.push(
                self.registry
                    .instance_admission(instance)
                    .lock_owned()
                    .await,
            );
        }
        let _instances = self.registry.instance_changes.lock().await;
        for instance in instances {
            loop {
                match self.registry.shutdown(&instance).await {
                    Err(error) if error.kind == HarnessErrorKind::AuthRefreshBusy => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    result => {
                        result?;
                        break;
                    }
                }
            }
            self.catalog.forget_agent(&instance);
            self.registry.forget_probed_auth(&instance);
        }
        installs.remove_all(&agent_id.0).map_err(|error| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                format!("failed to remove {agent_id}'s adapter: {error}"),
            )
        })?;
        // What the removed adapter advertised, and what its probe concluded
        // about sign-in, describe a binary that no longer exists.
        self.catalog.forget_agent(agent_id);
        self.registry.forget_probed_auth(agent_id);
        if remove_home {
            if let Some(home) = self.registry.agent_home(&requested_id) {
                let removal =
                    tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(home) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(error),
                    })
                    .await
                    .map_err(|error| {
                        HarnessError::new(
                            HarnessErrorKind::Internal,
                            format!("failed to join {agent_id}'s home removal: {error}"),
                        )
                    })?;
                removal.map_err(|error| {
                    HarnessError::new(
                        HarnessErrorKind::Internal,
                        format!("failed to remove {agent_id}'s home: {error}"),
                    )
                })?;
            }
        }
        tracing::info!(
            target: "ottam_core::agent",
            agent_id = %agent_id,
            remove_home,
            "adapter removed"
        );
        Ok(())
    }

    /// Stop an install in flight. A stale id is a no-op: a client racing the
    /// terminal event should not get an error for arriving second.
    pub fn cancel(&self, install_id: &AgentInstallID) {
        let operations = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for operation in operations.values() {
            if let AgentLifecycleOperation::Installing(running) = operation {
                if &running.install_id == install_id {
                    running.cancel.cancel();
                }
            }
        }
    }

    /// Stop every install in flight, for core shutdown: npm's process group
    /// dies instead of holding the process open until it finishes.
    pub fn cancel_all(&self) {
        let operations = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for operation in operations.values() {
            if let AgentLifecycleOperation::Installing(running) = operation {
                running.cancel.cancel();
            }
        }
    }

    /// Whether an install for this agent is currently running, for the
    /// `installState: installing` surface on `agent.list`.
    pub fn is_installing(&self, agent_id: &AgentId) -> bool {
        let harness = self
            .registry
            .definition(agent_id)
            .map(|definition| AgentId::new(definition.harness_id()))
            .unwrap_or_else(|_| agent_id.clone());
        matches!(
            self.operations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&harness),
            Some(AgentLifecycleOperation::Installing(_))
        )
    }

    /// Start background installs for every agent whose pin moved out from
    /// under an existing install. First install is user-consented via the Get
    /// button; an older version of the same package on disk is the standing
    /// consent that lets an app upgrade install the new pin silently — and is
    /// what keeps launching until it lands. Fresh machines (no version at all)
    /// still wait for Get. Versions a verified pin superseded, but that a live
    /// adapter still ran from when it landed, are pruned here too.
    pub fn spawn_auto_reinstalls(self: &Arc<Self>) {
        let Some(installs) = self.registry.adapter_installs() else {
            return;
        };
        for definition in self.registry.definitions() {
            let Some(package) = &definition.adapter else {
                continue;
            };
            if !self.registry.adapter_pin_installed(&definition)
                && installs.installed_other_version(definition.harness_id(), package)
            {
                tracing::info!(
                    target: "ottam_core::agent",
                    agent_id = %definition.agent_id,
                    version = %package.version,
                    "adapter pin moved; reinstalling in the background"
                );
                let _ = self.start(&definition.agent_id);
            } else if self.registry.adapter_pin_installed(&definition)
                && !self.is_installing(&definition.agent_id)
            {
                self.registry.prune_unused(|in_use| {
                    installs.prune_superseded(definition.harness_id(), &package.version, in_use);
                });
            }
        }
    }

    /// Retry a pin upgrade whose last attempt failed at least a minute ago —
    /// the run-start half of the retry policy, so an offline launch recovers
    /// without a restart or a click once the network is back. A cancelled
    /// update is not retried.
    pub fn retry_failed_update(self: &Arc<Self>, agent_id: &AgentId) {
        let Ok(definition) = self.registry.install_definition(agent_id) else {
            return;
        };
        let harness = AgentId::new(definition.harness_id());
        let consented = self.registry.adapter_installs().is_some_and(|installs| {
            definition
                .adapter
                .as_ref()
                .is_some_and(|package| installs.installed_other_version(&harness.0, package))
        });
        let due = self
            .outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&harness)
            .is_some_and(RecordedOutcome::retry_due);
        if due && !self.registry.adapter_pin_installed(&definition) && consented {
            tracing::info!(
                target: "ottam_core::agent",
                agent_id = %harness,
                "retrying a failed adapter update on run start"
            );
            let _ = self.start(&harness);
        }
    }

    /// How the agent's last install ended, if it did not reach `ready` and
    /// nothing has started since.
    pub fn outcome(&self, agent_id: &AgentId) -> Option<InstallOutcome> {
        let harness = self
            .registry
            .install_definition(agent_id)
            .map(|definition| AgentId::new(definition.harness_id()))
            .unwrap_or_else(|_| agent_id.clone());
        self.outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&harness)
            .map(|recorded| recorded.outcome().clone())
    }

    fn set_outcome(&self, agent_id: &AgentId, outcome: Option<InstallOutcome>) {
        let mut outcomes = self
            .outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match outcome {
            Some(outcome) => outcomes.insert(agent_id.clone(), RecordedOutcome::new(outcome)),
            None => outcomes.remove(agent_id),
        };
    }

    fn spawn(
        self: &Arc<Self>,
        agent_id: AgentId,
        package: AdapterPackage,
        installs: ManagedAdapterInstalls,
        install_id: AgentInstallID,
        cancel: CancellationToken,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let result = service
                .run_install(&agent_id, &package, &installs, &install_id, &cancel)
                .await;
            match result {
                Ok(()) => {
                    tracing::info!(
                        target: "ottam_core::agent",
                        agent_id = %agent_id,
                        version = %package.version,
                        "adapter installed and handshake-verified"
                    );
                    service.emit(
                        &agent_id,
                        &install_id,
                        &package,
                        AgentInstallPhase::Ready,
                        None,
                    );
                }
                Err(InstallFailure::Cancelled) => {
                    service.set_outcome(&agent_id, Some(InstallOutcome::Cancelled));
                    service.emit(
                        &agent_id,
                        &install_id,
                        &package,
                        AgentInstallPhase::Cancelled,
                        None,
                    );
                }
                Err(InstallFailure::Message(error)) => {
                    tracing::warn!(
                        target: "ottam_core::agent",
                        agent_id = %agent_id,
                        version = %package.version,
                        error = %error,
                        "adapter install failed"
                    );
                    service.set_outcome(&agent_id, Some(InstallOutcome::Failed(error.clone())));
                    service.emit(
                        &agent_id,
                        &install_id,
                        &package,
                        AgentInstallPhase::Failed,
                        Some(error),
                    );
                }
            }
            service.finish(&agent_id, &install_id);
        });
    }

    async fn run_install(
        self: &Arc<Self>,
        agent_id: &AgentId,
        package: &AdapterPackage,
        installs: &ManagedAdapterInstalls,
        install_id: &AgentInstallID,
        cancel: &CancellationToken,
    ) -> Result<(), InstallFailure> {
        // npm needs a working Node beside it; the runtime probe is also what
        // warms the resolver cache the npm path is read from. A missing
        // runtime fails here, in install terms — the runtime-problem UI
        // already tells the user what to do about Node itself. A binary
        // adapter downloads instead and needs neither.
        let source = match package.binary() {
            Some(binary) => InstallSource::Binary {
                binary,
                http: HttpClient::new().map_err(|error| {
                    InstallFailure::Message(format!("cannot start the download: {error}"))
                })?,
            },
            None => {
                let status = self
                    .registry
                    .install_runtime_status(agent_id)
                    .await
                    .map_err(|error| InstallFailure::Message(error.message))?;
                if !matches!(status.availability, RuntimeAvailability::Ready) {
                    return Err(InstallFailure::Message(
                        "a working Node runtime is required to install the adapter".to_string(),
                    ));
                }
                let Some(runtime) = self.registry.cached_node_runtime() else {
                    return Err(InstallFailure::Message(
                        "no Node toolchain was resolved for the install".to_string(),
                    ));
                };
                InstallSource::Npm {
                    runtime,
                    node_version: status.node_version,
                }
            }
        };

        // npm runs out of this Node's directory until the install ends; a
        // runtime upgrade landing meanwhile keeps it. A prune that won the
        // race to the lease has already removed it, which the check after
        // leasing catches rather than spawning a missing npm.
        let _toolchain = match &source {
            InstallSource::Npm { runtime, .. } => {
                let lease = self.registry.lease_toolchain(runtime.npm.clone());
                if runtime.npm.is_absolute() && !runtime.npm.exists() {
                    return Err(InstallFailure::Message(
                        "the Node runtime was replaced as the install started; try again"
                            .to_string(),
                    ));
                }
                Some(lease)
            }
            InstallSource::Binary { .. } => None,
        };

        self.emit(
            agent_id,
            install_id,
            package,
            AgentInstallPhase::Installing,
            None,
        );

        // A same-pin reinstall replaces the directory the running adapter was
        // launched from; stop it first rather than pulling the tree out from
        // under a live process. A first install has nothing to stop, and a pin
        // upgrade installs beside the running version: its adapters keep
        // working and move to the new one once idle.
        if self
            .registry
            .adapter_pin_installed(&self.registry.install_definition(agent_id)?)
        {
            for instance in self.registry.harness_instances(&agent_id.0) {
                let definition = self.registry.definition(&instance)?;
                let refresh_lock_path = definition.refresh_lock_path();
                loop {
                    tokio::select! {
                        () = cancel.cancelled() => return Err(InstallFailure::Cancelled),
                        () = super::refresh_lock::wait_for_release(refresh_lock_path.as_deref()) => {}
                    }
                    let result = tokio::select! {
                        () = cancel.cancelled() => return Err(InstallFailure::Cancelled),
                        result = self.registry.shutdown(&instance) => result,
                    };
                    match result {
                        Err(error) if error.kind == HarnessErrorKind::AuthRefreshBusy => continue,
                        result => {
                            result?;
                            break;
                        }
                    }
                }
                self.catalog.forget_agent(&instance);
                self.registry.forget_probed_auth(&instance);
            }
        }

        installs.remove_stale_staging(&agent_id.0);
        let staging = installs.staging_dir(&agent_id.0);
        let installed: Result<AdapterManifest, AdapterInstallError> = match &source {
            InstallSource::Npm {
                runtime,
                node_version,
            } => {
                npm_install_into(
                    &staging,
                    package,
                    &runtime.npm,
                    runtime.bin_dir(),
                    node_version.clone(),
                    cancel,
                )
                .await
            }
            InstallSource::Binary { binary, http } => {
                binary_install_into(&staging, package, binary, http, cancel).await
            }
        };
        if let Err(error) = installed {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error.into());
        }
        // npm and the download observe the token themselves, but either can
        // finish moments after Cancel lands. Publishing anyway would turn a
        // cancelled install into an installed one.
        if cancel.is_cancelled() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallFailure::Cancelled);
        }
        let published = installs.publish(&staging, &agent_id.0, &package.version)?;

        // The published directory is what flips `resolved_definition` to the
        // installed entrypoint, so the probe below launches exactly what a
        // real session will.
        self.emit(
            agent_id,
            install_id,
            package,
            AgentInstallPhase::Verifying,
            None,
        );
        // Verification is a full ACP handshake — up to a minute. Racing it
        // against the token is what makes Cancel mean something during
        // `verifying` instead of reporting `ready` after the wait. Dropping the
        // probe future here is safe: `probe_now` runs it as a detached task, so
        // the slot still settles.
        let verified = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                installs.rollback_publish(published)?;
                return Err(InstallFailure::Cancelled);
            }
            verified = self.verify_published_adapter(agent_id) => verified,
        };
        if let Err(error) = verified {
            // Roll back: "installed" must always mean handshake-verified. A
            // same-pin reinstall restores its previous verified tree; a pin
            // bump leaves the older version untouched beside this candidate.
            installs.rollback_publish(published)?;
            return Err(InstallFailure::Message(format!(
                "the adapter installed but failed its first handshake: {}",
                error.message
            )));
        }
        // Verified — now the old version is genuinely superseded.
        installs.commit_publish(published)?;
        self.registry.prune_unused(|in_use| {
            installs.prune_superseded(&agent_id.0, &package.version, in_use);
        });
        Ok(())
    }

    async fn verify_published_adapter(&self, agent_id: &AgentId) -> Result<(), HarnessError> {
        let instance = self
            .registry
            .harness_instances(&agent_id.0)
            .into_iter()
            .next()
            .unwrap_or_else(|| agent_id.clone());
        let agent_id = &instance;
        if self
            .registry
            .definition(agent_id)?
            .existing_home()
            .is_none()
        {
            return self.catalog.probe_now_fresh(&self.registry, agent_id).await;
        }
        let adapter = self.registry.create_install_verifier(agent_id)?;
        // Cancellation of the caller detaches this cleanup task, rather than
        // abandoning a verification process during publication rollback.
        tokio::spawn(async move {
            let verified =
                tokio::time::timeout(std::time::Duration::from_secs(60), adapter.start())
                    .await
                    .map_err(|_| {
                        HarnessError::new(
                            HarnessErrorKind::Internal,
                            "Adapter verification timed out",
                        )
                    })
                    .and_then(|result| result.map(|_| ()));
            let shutdown = adapter.shutdown().await;
            verified.and(shutdown)
        })
        .await
        .map_err(|_| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                "Adapter verification task failed",
            )
        })?
    }

    fn emit(
        &self,
        agent_id: &AgentId,
        install_id: &AgentInstallID,
        package: &AdapterPackage,
        phase: AgentInstallPhase,
        error: Option<String>,
    ) {
        self.record_phase(agent_id, install_id, phase);
        (self.events)(AgentInstallEventParams {
            install_id: install_id.clone(),
            agent_id: AgentID(agent_id.0.clone()),
            name: package.name.clone(),
            version: package.version.clone(),
            phase,
            error,
        });
    }

    fn record_phase(
        &self,
        agent_id: &AgentId,
        install_id: &AgentInstallID,
        phase: AgentInstallPhase,
    ) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(AgentLifecycleOperation::Installing(running)) = operations.get_mut(agent_id) {
            if &running.install_id == install_id {
                running.phase = phase;
            }
        }
    }

    fn finish(&self, agent_id: &AgentId, install_id: &AgentInstallID) {
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(
            operations.get(agent_id),
            Some(AgentLifecycleOperation::Installing(running))
                if &running.install_id == install_id
        ) {
            operations.remove(agent_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure is retried on run start only once it is a minute old, and a
    /// cancel never is.
    #[test]
    fn a_run_start_retries_only_an_aged_failure() {
        let fresh = RecordedOutcome::new(InstallOutcome::Failed("offline".to_string()));
        assert!(!fresh.retry_due());
        let aged = RecordedOutcome {
            outcome: InstallOutcome::Failed("offline".to_string()),
            at: Instant::now() - RUN_START_RETRY_AFTER,
        };
        assert!(aged.retry_due());
        let cancelled = RecordedOutcome {
            outcome: InstallOutcome::Cancelled,
            at: Instant::now() - RUN_START_RETRY_AFTER,
        };
        assert!(!cancelled.retry_due());
    }
}
