//! The model-catalog probe: the one place core opens a session nobody asked
//! for.
//!
//! ACP reports models, modes, and thought levels only on `session/new`, so the
//! catalog starts a throwaway adapter, opens one session in a scratch
//! directory, reads what it advertised, and shuts the adapter down. No prompt
//! is ever sent.
//!
//! The probe must leave no vendor history: nothing in the agent's session
//! list, nothing on disk the user would find later. Agents differ in what a
//! bare `session/new` stores, so each definition declares a
//! [`ProbeCleanup`] beside its adapter pin, and this module applies it. That
//! declaration rests on adapter internals, so recheck it at every pin bump with
//! `scripts/check-probe-leftovers.py` (see `docs/architecture/backend/acp.md`
//! § Model Catalog Freshness). A cleanup that fails logs
//! "model catalog probe left a session behind".
//!
//! CLI-setup homes are probed the same way. The throwaway process shares the
//! user's credentials, so it is never stopped while that home's sign-in
//! refresh is held; and it runs the user's own settings and hooks.
//!
//! ACP's unstable `session/options` answers the same question without a
//! session. Once an agent advertises `sessionCapabilities.options`, it should
//! replace this probe for that agent.

use super::registry::AgentRegistry;
use super::types::{
    AdapterStatus, AgentId, AgentModelOption, AgentSessionSelects,
    HarnessAdapter, HarnessAuthMethod, HarnessAuthMethodKind, HarnessAuthState, HarnessError,
    HarnessErrorKind, HarnessSessionId, RuntimeAvailability, SessionConfigRequest,
};
use super::AcpFeature;
use ottam_protocol::AgentAuthMethod;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Budget for the adapter start and `session/new`; a cold start can be slow.
/// The auth grace period follows it, bounded on its own.
pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Grace period for an adapter's asynchronous first identity notification.
const AUTH_STATUS_GRACE_PERIOD: Duration = Duration::from_secs(6);
/// How long the cleanup request may take before the probe gives up on it.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Where every probe session's working directory points.
const SCRATCH_CWD: &str = "ottam-model-probe";
/// Parent of the per-probe session stores [`ProbeCleanup::ScratchSessionDir`]
/// points at.
const SCRATCH_SESSIONS: &str = "ottam-model-probe-sessions";

/// The `_meta` object a [`ProbeCleanup::SessionMeta`] sends.
pub type ProbeSessionMeta = fn() -> serde_json::Map<String, serde_json::Value>;

/// What a probe's `session/new` would otherwise leave in the agent's history,
/// and how the probe avoids it. Declared on each definition beside its pin.
#[derive(Debug, Clone, Copy, Default)]
pub enum ProbeCleanup {
    /// The agent stores nothing until a session's first prompt.
    #[default]
    Nothing,
    /// `_meta` for the probe's `session/new` that tells the agent not to
    /// store the session.
    SessionMeta(ProbeSessionMeta),
    /// `session/new` stores the session; `session/delete` removes it.
    DeleteSession,
    /// `session/new` stores the session and the agent cannot delete it. This
    /// launch flag points its session store at a per-probe directory that the
    /// probe removes.
    ScratchSessionDir(&'static str),
}

/// Equal when they apply the same cleanup: a meta compares by the object it
/// sends, since function addresses are not stable.
impl PartialEq for ProbeCleanup {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nothing, Self::Nothing) | (Self::DeleteSession, Self::DeleteSession) => true,
            (Self::SessionMeta(left), Self::SessionMeta(right)) => left() == right(),
            (Self::ScratchSessionDir(left), Self::ScratchSessionDir(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for ProbeCleanup {}

impl ProbeCleanup {
    fn session_meta(self) -> Option<serde_json::Map<String, serde_json::Value>> {
        match self {
            Self::SessionMeta(meta) => Some(meta()),
            _ => None,
        }
    }
}

pub(super) struct CatalogProbe {
    pub(super) models: Vec<AgentModelOption>,
    pub(super) default_model_id: Option<String>,
    pub(super) prompt_image: bool,
    pub(super) auth_methods: Vec<AgentAuthMethod>,
    /// `None` when the probe's `session/new` failed — nothing was observed.
    pub(super) session_selects: Option<AgentSessionSelects>,
    /// Everything `initialize` reported, keyed by ACP surface.
    pub(super) agent_advertises: BTreeMap<AcpFeature, bool>,
    /// The sign-in verdict observed through session validation or an explicit
    /// auth-status notification. Claude session creation alone leaves Unknown.
    /// Persisted with the adapter pin and reported to the registry so a cold
    /// launch can offer Sign In without waiting for a prompt to fail.
    pub(super) auth_state: HarnessAuthState,
}

/// Probe one agent with a throwaway adapter. Never touches the registry's
/// shared adapter. Returns once the catalog can be recorded; on a CLI-setup
/// home whose sign-in refresh is held, the adapter's shutdown finishes in the
/// background so a waiting install never stalls behind it.
pub(super) async fn probe(
    registry: &Arc<AgentRegistry>,
    agent_id: &AgentId,
) -> Result<CatalogProbe, HarnessError> {
    // Never launch an adapter whose pin is not installed: `resolved_definition`
    // falls back to `npx`, which downloads and executes the package — the
    // un-consented fetch the install flow exists to prevent. This is the choke
    // point for both the background sweep and `probe_now`, which is also what
    // keeps install verification from ever attesting an npx tree: a probe can
    // only exist once the published directory is the thing that launches.
    let definition = registry.definition(agent_id)?;
    if registry.adapter_needs_install(&definition) {
        return Err(HarnessError::new(
            HarnessErrorKind::AdapterNotInstalled,
            format!("{agent_id}'s adapter is not installed"),
        ));
    }
    // Skip the expensive spawn for an agent whose runtime is missing or
    // unsupported; the cheap version probe would fail anyway and the backoff
    // keeps us from retrying it every poll.
    let runtime = registry.runtime_status(agent_id).await?;
    if !matches!(runtime.availability, RuntimeAvailability::Ready) {
        return Err(HarnessError::new(
            HarnessErrorKind::RuntimeMissing,
            format!("{agent_id} runtime is not ready for a model catalog probe"),
        ));
    }
    let refresh_lock = definition.refresh_lock_path();
    // A CLI home's last probe may still be waiting out a refresh; starting
    // another would stack a process per probe behind the same lock.
    if refresh_lock.is_some() && registry.probe_teardown_pending(agent_id) {
        return Err(HarnessError::new(
            HarnessErrorKind::Internal,
            format!("{agent_id}'s previous model catalog probe is still stopping"),
        ));
    }
    let cleanup = definition.catalog_probe;
    let session_store = match cleanup {
        ProbeCleanup::ScratchSessionDir(_) => Some(scratch_session_store(agent_id)?),
        _ => None,
    };
    let extra_arguments = match (cleanup, &session_store) {
        (ProbeCleanup::ScratchSessionDir(flag), Some(store)) => {
            vec![flag.to_string(), store.to_string_lossy().into_owned()]
        }
        _ => Vec::new(),
    };
    let adapter = match registry.create_probe_adapter(agent_id, extra_arguments) {
        Ok(adapter) => adapter,
        Err(error) => {
            if let Some(store) = &session_store {
                let _ = remove_dir(store);
            }
            return Err(error);
        }
    };
    let cwd = std::env::temp_dir().join(SCRATCH_CWD);
    let _ = std::fs::create_dir_all(&cwd);
    registry.begin_catalog_probe(agent_id);
    let config = SessionConfigRequest {
        meta: cleanup.session_meta(),
        ..SessionConfigRequest::default()
    };
    // The session lands outside the timed future, so a timeout never loses
    // the id its cleanup needs.
    let mut requested = false;
    let mut session = None;
    // Bound the start and `session/new` here rather than at the call site:
    // on timeout the future is dropped but the teardown below still runs,
    // stopping the child process instead of detaching it.
    let opened = tokio::time::timeout(PROBE_TIMEOUT, async {
        adapter.start().await?;
        // `session/new` is where the models live, but it is also the step that
        // fails for a signed-out agent — which is exactly when the sign-ins
        // read off `initialize` matter most. So the session is attempted for
        // what it adds, not required for the probe to be worth recording.
        requested = true;
        session = Some(adapter.new_session(cwd, config).await);
        Ok::<(), HarnessError>(())
    })
    .await;
    let result = match opened {
        Ok(Ok(())) => {
            let status = wait_for_auth_status(adapter.status(), AUTH_STATUS_GRACE_PERIOD).await;
            if let Some(Err(error)) = &session {
                tracing::info!(
                    target: "ottam_core::agent",
                    agent_id = %agent_id,
                    error = %error,
                    auth_method_count = status.auth_methods.len(),
                    "model catalog probe could not open a session; keeping what initialize reported"
                );
            }
            Ok(catalog_probe(status))
        }
        Ok(Err(error)) => Err(error),
        Err(_) => {
            // The agent may have stored a session whose id never arrived.
            if requested && session.is_none() && cleanup == ProbeCleanup::DeleteSession {
                left_behind(agent_id, "session/new timed out before returning its id");
            }
            Err(HarnessError::new(
                HarnessErrorKind::Internal,
                format!("model catalog probe for {agent_id} timed out"),
            ))
        }
    };
    if let (ProbeCleanup::DeleteSession, Some(Ok(session))) = (cleanup, session) {
        delete_probe_session(adapter.as_ref(), agent_id, session).await;
    }
    let teardown = Teardown {
        registry: Arc::clone(registry),
        agent_id: agent_id.clone(),
        adapter,
        refresh_lock,
        session_store,
    };
    // A home with a refresh lock always tears down in the background: the
    // probe's own process may take the lock at any moment, and the wait for
    // it has no bound. The adapter stays a tracked child meanwhile, so Quit
    // waits for it too.
    if teardown.refresh_lock.is_some() && registry.begin_probe_teardown(agent_id) {
        tokio::spawn(async move {
            let registry = Arc::clone(&teardown.registry);
            let agent_id = teardown.agent_id.clone();
            teardown.run().await;
            registry.finish_probe_teardown(&agent_id);
        });
    } else {
        teardown.run().await;
    }
    result
}

fn catalog_probe(status: AdapterStatus) -> CatalogProbe {
    CatalogProbe {
        models: status.models,
        default_model_id: status.default_model_id,
        // Read from `initialize`, so both are settled whether or not
        // `session/new` succeeded.
        prompt_image: status.capabilities.prompt_image,
        agent_advertises: status.capabilities.agent_advertises,
        auth_methods: status.auth_methods.iter().map(wire_auth_method).collect(),
        session_selects: status.session_selects,
        auth_state: status.auth_state,
    }
}

async fn delete_probe_session(
    adapter: &dyn HarnessAdapter,
    agent_id: &AgentId,
    session: HarnessSessionId,
) {
    let error = match tokio::time::timeout(CLEANUP_TIMEOUT, adapter.delete_session(session)).await
    {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error.to_string(),
        Err(_) => "session/delete timed out".to_string(),
    };
    left_behind(agent_id, &error);
}

/// Stopping the throwaway adapter, and removing what it was pointed at.
struct Teardown {
    registry: Arc<AgentRegistry>,
    agent_id: AgentId,
    adapter: Arc<dyn HarnessAdapter>,
    /// The CLI-setup home's sign-in refresh lock, if it has one.
    refresh_lock: Option<PathBuf>,
    session_store: Option<PathBuf>,
}

impl Teardown {
    async fn run(self) {
        // A transport stopped mid-refresh can strand the user's own
        // credentials, so wait for the lock however long it is held. A
        // shared home's shutdown refuses while a refresh holds it, and one can
        // start between the wait and the call, so wait again and retry.
        let stopped = loop {
            super::refresh_lock::wait_for_release(self.refresh_lock.as_deref()).await;
            match self.adapter.shutdown().await {
                Err(error) if error.kind == HarnessErrorKind::AuthRefreshBusy => continue,
                result => break result.is_ok(),
            }
        };
        self.registry.finish_catalog_probe(&self.agent_id, stopped);
        if let Some(store) = &self.session_store {
            if let Err(error) = remove_dir(store) {
                left_behind(&self.agent_id, &error.to_string());
            }
        }
    }
}

fn remove_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// The one warning a broken cleanup produces. The catalog is still recorded:
/// a leftover session is worth fixing, not worth an empty model list.
fn left_behind(agent_id: &AgentId, error: &str) {
    tracing::warn!(
        target: "ottam_core::agent",
        agent_id = %agent_id,
        error = %error,
        "model catalog probe left a session behind"
    );
}

/// A fresh, empty directory for one probe's session store.
fn scratch_session_store(agent_id: &AgentId) -> Result<PathBuf, HarnessError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "{}-{}-{}",
        agent_id.0.replace(['/', '.'], "-"),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(SCRATCH_SESSIONS).join(name);
    let _ = remove_dir(&path);
    std::fs::create_dir_all(&path).map_err(|error| {
        HarnessError::new(
            HarnessErrorKind::Internal,
            format!("Cannot prepare the model catalog probe: {error}"),
        )
    })?;
    Ok(path)
}

/// Give an advertised identity notification time to arrive before the probe
/// shuts down. Other adapters need no wait; silence stays Unknown.
async fn wait_for_auth_status(
    mut status_rx: tokio::sync::watch::Receiver<AdapterStatus>,
    grace_period: Duration,
) -> AdapterStatus {
    if status_rx.borrow().capabilities.auth_status {
        let _ = tokio::time::timeout(grace_period, async {
            loop {
                if status_rx.borrow_and_update().auth_state != HarnessAuthState::Unknown {
                    break;
                }
                if status_rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }
    let status = status_rx.borrow().clone();
    status
}

/// The catalog stores what a client can act on: an id, a name, and which of
/// the two mechanisms it uses. The command line a terminal method carries is
/// deliberately dropped — see `CatalogEntry::auth_methods`.
fn wire_auth_method(method: &HarnessAuthMethod) -> AgentAuthMethod {
    AgentAuthMethod {
        method_id: method.method_id.clone(),
        name: method.name.clone(),
        description: method.description.clone(),
        kind: match method.kind {
            HarnessAuthMethodKind::Agent => ottam_protocol::AgentAuthMethodKind::Agent,
            HarnessAuthMethodKind::Terminal => ottam_protocol::AgentAuthMethodKind::Terminal,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped_cleanups() -> BTreeMap<String, ProbeCleanup> {
        super::super::registry::built_in_definitions()
            .into_iter()
            .map(|definition| (definition.agent_id.0, definition.catalog_probe))
            .collect()
    }

    #[tokio::test]
    async fn advertised_auth_status_waits_for_delayed_verdict() {
        let mut status = AdapterStatus::stopped();
        status.capabilities.auth_status = true;
        let (sender, receiver) = tokio::sync::watch::channel(status);
        let (observed, ()) = tokio::join!(
            wait_for_auth_status(receiver, Duration::from_secs(1)),
            async {
                tokio::task::yield_now().await;
                sender.send_modify(|status| status.auth_state = HarnessAuthState::Missing);
            }
        );
        assert_eq!(observed.auth_state, HarnessAuthState::Missing);
        assert!(!observed.credentials_seen);
    }

    #[tokio::test]
    async fn auth_status_silence_and_unsupported_extension_stay_unknown() {
        for advertised in [false, true] {
            let mut status = AdapterStatus::stopped();
            status.capabilities.auth_status = advertised;
            let (_sender, receiver) = tokio::sync::watch::channel(status);
            let observed = wait_for_auth_status(receiver, Duration::ZERO).await;
            assert_eq!(observed.auth_state, HarnessAuthState::Unknown);
            assert!(!observed.credentials_seen);
        }
    }

    #[tokio::test]
    async fn unsupported_auth_status_does_not_wait_for_notifications() {
        let (_sender, receiver) = tokio::sync::watch::channel(AdapterStatus::stopped());
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_auth_status(receiver, Duration::from_secs(60)),
        )
        .await
        .expect("legacy adapter must not wait for an unsupported extension");
    }

    /// A pin bump that changes what `session/new` stores must revisit these.
    #[test]
    fn every_shipped_agent_declares_its_cleanup() {
        let cleanups = shipped_cleanups();
        assert_eq!(cleanups.get("codex"), Some(&ProbeCleanup::Nothing));
        assert_eq!(
            cleanups.get("claude").and_then(|cleanup| cleanup.session_meta()),
            Some(serde_json::Map::from_iter([(
                "claudeCode".to_string(),
                serde_json::json!({"options": {"persistSession": false}}),
            )]))
        );
        assert_eq!(cleanups.get("opencode"), Some(&ProbeCleanup::DeleteSession));
        assert_eq!(
            cleanups.get("omp"),
            Some(&ProbeCleanup::ScratchSessionDir("--session-dir"))
        );
    }

    #[test]
    fn scratch_session_stores_are_fresh_and_distinct() {
        let id = AgentId::new("omp.profile");
        let first = scratch_session_store(&id).unwrap();
        let second = scratch_session_store(&id).unwrap();
        assert_ne!(first, second);
        assert!(first.is_dir() && std::fs::read_dir(&first).unwrap().next().is_none());
        remove_dir(&first).unwrap();
        remove_dir(&second).unwrap();
        remove_dir(&first).expect("removing a missing store is not an error");
    }
}
