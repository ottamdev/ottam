//! Background discovery and caching of the models each ACP agent advertises.
//!
//! ACP exposes model choices only in the `session/new` response (as a
//! `category: model` config option), so the catalog is discovered by probing
//! (`catalog_probe`): this module decides when to probe and merges what each
//! probe and live session observed.
//!
//! Results are cached in memory and persisted to `~/.ottam/model-catalog.json`
//! so the composer has model choices immediately on relaunch.

use super::acp::AcpFeature;
use super::catalog_probe;
use super::registry::AgentRegistry;
use super::types::{
    AdapterLifecycleState, AdapterStatus, AgentDefinition, AgentId, AgentModelOption,
    AgentModelSelects, AgentSessionSelects, HarnessAuthState, HarnessError, HarnessErrorKind,
};
use ottam_protocol::AgentAuthMethod;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What a finished probe settles to. `Ok` means the handshake completed and
/// the catalog was recorded; the models themselves live in the catalog, not
/// here, so joiners re-read them off the service.
type ProbeOutcome = Result<(), HarnessError>;
/// `None` while the probe is still running, `Some` once it settles.
type ProbeWaiter = tokio::sync::watch::Receiver<Option<ProbeOutcome>>;
/// The owning half of a probe slot; whoever holds it must settle the probe.
type ProbeOwner = tokio::sync::watch::Sender<Option<ProbeOutcome>>;

/// Told when one agent's cached catalog content changes, so the server can
/// push `engine.changed`. Content only: refreshing an unchanged entry's TTL
/// clock is not a change, and firing on it would have every `engine.list` poll
/// ask its own caller to poll again.
pub type CatalogChangeSink = Arc<dyn Fn(&AgentId) + Send + Sync>;

const CATALOG_FILE_NAME: &str = "model-catalog.json";
const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Minimum spacing between probe attempts for the same agent. A probe that
/// yields no catalog (uninstalled runtime, unauthenticated, offline) records
/// nothing, so without this backoff `engine.list` would re-spawn a real `npx`
/// process on every poll. Applies to successes too, but those are short-circuited
/// by the cache first.
const PROBE_RETRY_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// A live catalog that hasn't changed still refreshes its TTL clock in memory
/// on every `engine.list`, but we only flush that to disk this often — enough
/// that a restart within the day doesn't re-probe, without a write per poll.
const TIMESTAMP_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogEntry {
    models: Vec<AgentModelOption>,
    #[serde(
        rename = "defaultModelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    default_model_id: Option<String>,
    /// Whether the agent advertised `promptCapabilities.image` during
    /// `initialize`. Cached because otherwise the composer can only learn it
    /// after a run has started an adapter — which is exactly when it is too
    /// late to attach an image to the first message. `None` means "never
    /// observed" (an entry written before this field existed) and triggers a
    /// re-probe; `Some(false)` is a real negative answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_image: Option<bool>,
    /// The sign-ins the agent advertised during `initialize`, so the engine
    /// picker can offer one before any adapter is running. Cached in the wire
    /// shape deliberately: the terminal command a `terminal` method carries is
    /// an absolute path into the adapter's installed package, which goes stale
    /// on upgrade — core re-derives it from the live adapter at sign-in time.
    ///
    /// `None` means "never observed" (an entry written before this field
    /// existed) and triggers a re-probe; `Some(vec![])` is a real answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_methods: Option<Vec<AgentAuthMethod>>,
    /// The authentication verdict reached while opening a throwaway session.
    /// Persisted with the catalog so a fresh Core can still offer Sign In
    /// without repeating a handshake that the catalog TTL considers fresh.
    /// `None` marks an older entry and requires a new probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_state: Option<HarnessAuthState>,
    /// The pinned adapter package this entry was probed against. A pin bump
    /// changes what the adapter advertises, so the cached answer has to be
    /// discarded immediately rather than waiting out the TTL. `None` means "an
    /// entry written before pins were recorded" and triggers a re-probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter_version: Option<String>,
    /// Mode and thought-level selects observed on `session/new`, in one unit
    /// like the adapter status reports them. `None` means "never observed" (an
    /// entry written before this field existed, or a probe whose session
    /// failed) and triggers a re-probe; a `Some` with empty lists is a real
    /// "this agent advertises none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_selects: Option<AgentSessionSelects>,
    /// Each model's own thought-level select, observed by live sessions only
    /// (probes never switch models). Newest observation per model wins; kept
    /// across probes and dropped with the entry on a pin change.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_selects: BTreeMap<String, AgentModelSelects>,
    /// Everything `initialize` said the agent supports, keyed by ACP
    /// surface, so the compatibility view has an answer before any adapter
    /// runs. `None` means "never observed" (an entry written before this
    /// field existed) and triggers a re-probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_advertises: Option<BTreeMap<AcpFeature, bool>>,
    /// Recent same-pin live sessions outrank short-lived probes, including
    /// after the adapter stops or Core restarts. Legacy entries have no live
    /// provenance and retain their existing probe/TTL behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    live_models_at_unix: Option<u64>,
    /// Session/load can observe selects without observing models. Keep their
    /// provenance separate so that a partial live answer does not protect
    /// models that have only ever been probed (or refresh older live models).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    live_selects_at_unix: Option<u64>,
    /// TTL scheduling follows each accepted session field independently of
    /// live/probe precedence. Partial answers must not make retained data
    /// fresh. Legacy entries fall back to their original shared probe clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    models_observed_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selects_observed_at_unix: Option<u64>,
    probed_at_unix: u64,
    /// The last flushed TTL clock, distinct from the clock refreshed in
    /// memory on every live read. Otherwise frequent polls postpone the
    /// hourly flush forever, losing freshness/provenance on restart.
    #[serde(skip)]
    persisted_at_unix: u64,
}

struct CatalogObservation {
    models: Vec<AgentModelOption>,
    default_model_id: Option<String>,
    prompt_image: Option<bool>,
    adapter_version: Option<String>,
    session_selects: Option<AgentSessionSelects>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogSource {
    Probe,
    Live,
}

pub struct ModelCatalogService {
    path: Option<PathBuf>,
    entries: Mutex<BTreeMap<String, CatalogEntry>>,
    /// One entry per probe currently running, keyed by agent id. The receiver
    /// lets a second caller await the in-flight probe's outcome instead of
    /// duplicating it (`None` until the probe settles).
    in_flight: Mutex<BTreeMap<String, ProbeWaiter>>,
    /// Last probe attempt (any outcome) per agent, for retry backoff.
    last_attempt: Mutex<BTreeMap<String, u64>>,
    /// Serializes `persist()` so its temp-file writes and rename can't race.
    persist_lock: Mutex<()>,
    /// Installed once by the server, after the outbound channel exists. Absent
    /// in tests and in any catalog built without one, where a change simply
    /// goes unannounced.
    change_sink: OnceLock<CatalogChangeSink>,
    probe_slots: tokio::sync::Semaphore,
    harness_slots: Mutex<BTreeMap<String, Arc<tokio::sync::Semaphore>>>,
    deleting: Mutex<BTreeMap<String, usize>>,
}

/// Each caller owns its fence, including cancellation while waiting for a probe.
pub(crate) struct CatalogDeleteFence<'a> {
    catalog: &'a ModelCatalogService,
    agent_id: AgentId,
}

impl CatalogDeleteFence<'_> {
    pub(crate) async fn wait(&self) {
        let waiter = self
            .catalog
            .in_flight
            .lock()
            .unwrap()
            .get(&self.agent_id.0)
            .cloned();
        if let Some(waiter) = waiter {
            let _ = ModelCatalogService::await_probe(&self.agent_id, waiter).await;
        }
    }
}

impl Drop for CatalogDeleteFence<'_> {
    fn drop(&mut self) {
        let mut deleting = self.catalog.deleting.lock().unwrap();
        if let Some(count) = deleting.get_mut(&self.agent_id.0) {
            *count -= 1;
            if *count == 0 {
                deleting.remove(&self.agent_id.0);
            }
        }
    }
}

impl ModelCatalogService {
    /// Load the catalog persisted under the ottam home directory. A
    /// missing or unreadable home degrades to an in-memory-only catalog.
    pub fn load_default() -> Self {
        let path = crate::storage::ottam_home()
            .ok()
            .map(|home| home.join(CATALOG_FILE_NAME));
        Self::load(path)
    }

    pub fn load(path: Option<PathBuf>) -> Self {
        let mut entries = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|data| serde_json::from_str::<BTreeMap<String, CatalogEntry>>(&data).ok())
            .unwrap_or_default();
        for entry in entries.values_mut() {
            entry.persisted_at_unix = entry.probed_at_unix;
        }
        Self {
            path,
            entries: Mutex::new(entries),
            in_flight: Mutex::new(BTreeMap::new()),
            last_attempt: Mutex::new(BTreeMap::new()),
            persist_lock: Mutex::new(()),
            change_sink: OnceLock::new(),
            probe_slots: tokio::sync::Semaphore::new(2),
            harness_slots: Mutex::new(BTreeMap::new()),
            deleting: Mutex::new(BTreeMap::new()),
        }
    }

    /// Route catalog-content changes somewhere. Called once, when the server
    /// has an outbound channel to push them down; a second call is ignored.
    pub fn set_change_sink(&self, sink: CatalogChangeSink) {
        let _ = self.change_sink.set(sink);
    }

    /// Announce a content change. Always called after the entries lock has been
    /// released: the sink is the server's, and holding a catalog lock across it
    /// would put an unrelated subsystem inside this one's critical section.
    fn announce_change(&self, agent_id: &AgentId) {
        if let Some(sink) = self.change_sink.get() {
            sink(agent_id);
        }
    }

    pub fn models_for(&self, agent_id: &AgentId) -> Vec<AgentModelOption> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .map(|entry| entry.models.clone())
            .unwrap_or_default()
    }

    /// Models and default read from a single snapshot, so a concurrent
    /// `record()` can never yield an empty model list paired with a set
    /// default (or vice versa).
    pub fn catalog_for(&self, agent_id: &AgentId) -> (Vec<AgentModelOption>, Option<String>) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .map(|entry| (entry.models.clone(), entry.default_model_id.clone()))
            .unwrap_or_default()
    }

    /// The mode and thought-level selects observed the last time a probe (or a
    /// live adapter) created a session. `None` when never observed.
    pub fn session_selects_for(&self, agent_id: &AgentId) -> Option<AgentSessionSelects> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .and_then(|entry| entry.session_selects.clone())
    }

    /// Sign-ins the agent advertised the last time one was observed. Empty
    /// when never probed, which reads as "no sign-in to offer yet" rather than
    /// "this agent has none".
    pub fn auth_methods_for(&self, agent_id: &AgentId) -> Vec<AgentAuthMethod> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .and_then(|entry| entry.auth_methods.clone())
            .unwrap_or_default()
    }

    /// Store the sign-ins an agent advertised, independently of its models.
    ///
    /// Separate from [`Self::record`] because it has a different source: models
    /// only exist after a successful `session/new`, whereas auth methods arrive
    /// during `initialize` and are at their most useful precisely when
    /// `session/new` fails for want of credentials.
    /// What `initialize` said the agent supports the last time it was
    /// observed. `None` when never probed, or when the entry was recorded
    /// against a different adapter pin: the old package's answers say nothing
    /// about the new one, and `agent.list` reads this without probing. A key
    /// absent from the map could not be judged from `initialize`.
    pub fn advertised_features_for(
        &self,
        agent_id: &AgentId,
        adapter_version: Option<&str>,
    ) -> Option<BTreeMap<AcpFeature, bool>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .filter(|entry| entry.adapter_version.as_deref() == adapter_version)
            .and_then(|entry| entry.agent_advertises.clone())
    }

    /// Store what a probe's `initialize` reported. Kept separate from
    /// [`Self::record`] like the sign-in fields: `record` also folds in live
    /// adapters, and those have nothing new to say about `initialize`.
    fn record_advertised_features(
        &self,
        agent_id: &AgentId,
        agent_advertises: BTreeMap<AcpFeature, bool>,
    ) {
        let now = unix_now();
        let changed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match entries.get_mut(&agent_id.0) {
                Some(existing) if existing.agent_advertises.as_ref() == Some(&agent_advertises) => {
                    false
                }
                Some(existing) => {
                    existing.agent_advertises = Some(agent_advertises);
                    true
                }
                None => {
                    entries.insert(
                        agent_id.0.clone(),
                        CatalogEntry {
                            models: Vec::new(),
                            default_model_id: None,
                            prompt_image: None,
                            auth_methods: None,
                            auth_state: None,
                            adapter_version: None,
                            session_selects: None,
                            model_selects: BTreeMap::new(),
                            agent_advertises: Some(agent_advertises),
                            live_models_at_unix: None,
                            live_selects_at_unix: None,
                            models_observed_at_unix: None,
                            selects_observed_at_unix: None,
                            probed_at_unix: now,
                            persisted_at_unix: now,
                        },
                    );
                    true
                }
            }
        };
        if changed {
            self.persist();
            self.announce_change(agent_id);
        }
    }

    pub fn record_auth_methods(&self, agent_id: &AgentId, auth_methods: Vec<AgentAuthMethod>) {
        let now = unix_now();
        let changed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match entries.get_mut(&agent_id.0) {
                Some(existing) if existing.auth_methods.as_deref() == Some(&auth_methods) => false,
                Some(existing) => {
                    existing.auth_methods = Some(auth_methods);
                    true
                }
                None => {
                    entries.insert(
                        agent_id.0.clone(),
                        CatalogEntry {
                            models: Vec::new(),
                            default_model_id: None,
                            prompt_image: None,
                            auth_methods: Some(auth_methods),
                            auth_state: None,
                            adapter_version: None,
                            session_selects: None,
                            model_selects: BTreeMap::new(),
                            agent_advertises: None,
                            live_models_at_unix: None,
                            live_selects_at_unix: None,
                            models_observed_at_unix: None,
                            selects_observed_at_unix: None,
                            probed_at_unix: now,
                            persisted_at_unix: now,
                        },
                    );
                    true
                }
            }
        };
        if changed {
            self.persist();
            self.announce_change(agent_id);
        }
    }

    fn record_auth_state(&self, agent_id: &AgentId, auth_state: HarnessAuthState) {
        let now = unix_now();
        let changed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match entries.get_mut(&agent_id.0) {
                Some(existing) if existing.auth_state == Some(auth_state) => false,
                Some(existing) => {
                    existing.auth_state = Some(auth_state);
                    true
                }
                None => {
                    entries.insert(
                        agent_id.0.clone(),
                        CatalogEntry {
                            models: Vec::new(),
                            default_model_id: None,
                            prompt_image: None,
                            auth_methods: None,
                            auth_state: Some(auth_state),
                            adapter_version: None,
                            session_selects: None,
                            model_selects: BTreeMap::new(),
                            agent_advertises: None,
                            live_models_at_unix: None,
                            live_selects_at_unix: None,
                            models_observed_at_unix: None,
                            selects_observed_at_unix: None,
                            probed_at_unix: now,
                            persisted_at_unix: now,
                        },
                    );
                    true
                }
            }
        };
        if changed {
            self.persist();
            self.announce_change(agent_id);
        }
    }

    /// Seed a new registry with the durable verdicts from the last successful
    /// probe. The registry ignores an observed `Unknown`; legacy entries have
    /// no value and are forced through a fresh probe by `needs_probe`.
    pub(crate) fn restore_probed_auth(&self, registry: &AgentRegistry) {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (agent_id, entry) in entries.iter() {
            // A previous adapter's inferred verdict must not survive a pin
            // bump (Claude formerly treated session creation as sign-in).
            let current_pin = registry
                .definition(&AgentId::new(agent_id))
                .ok()
                .and_then(|definition| pinned_adapter_package(&definition));
            if entry.adapter_version != current_pin {
                continue;
            }
            if let Some(auth_state) = entry.auth_state {
                registry.note_probed_auth(&AgentId::new(agent_id), auth_state);
            }
        }
    }

    /// Whether the agent accepted image blocks the last time one was observed.
    /// Defaults to false when never probed, so the composer disables attaching
    /// rather than promising something the agent may reject.
    pub fn prompt_image_for(&self, agent_id: &AgentId) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .and_then(|entry| entry.prompt_image)
            .unwrap_or(false)
    }

    /// Fold a running adapter's advertised models and selects into the
    /// catalog. A live `session/new` is fresher than any probe: Claude's CLI,
    /// for one, refreshes its model list in the background, so a short-lived
    /// probe can capture an answer the next real session already contradicts.
    pub(crate) fn record_live(&self, definition: &AgentDefinition, status: &AdapterStatus) {
        if status.lifecycle != AdapterLifecycleState::Ready {
            return;
        }
        self.record_session_change(definition, status);
    }

    /// Record only the session fields the registry observed changing. A watch
    /// channel can coalesce a session response and an immediate stop/death,
    /// so the snapshot's final lifecycle cannot invalidate that observation.
    /// Retained fields are omitted by the watcher; engine.list instead uses
    /// record_live so stopped snapshots cannot refresh their freshness clocks.
    pub(crate) fn record_session_change(
        &self,
        definition: &AgentDefinition,
        status: &AdapterStatus,
    ) {
        if status.models.is_empty()
            && status.session_selects.is_none()
            && status.model_selects.is_empty()
        {
            return;
        }
        self.record_observation(
            &definition.agent_id,
            CatalogObservation {
                models: status.models.clone(),
                default_model_id: status.default_model_id.clone(),
                prompt_image: Some(status.capabilities.prompt_image),
                adapter_version: pinned_adapter_package(definition),
                session_selects: status.session_selects.clone(),
            },
            CatalogSource::Live,
        );
        self.record_model_selects(
            &definition.agent_id,
            pinned_adapter_package(definition).as_deref(),
            &status.model_selects,
        );
    }

    /// Merge per-model selects into an existing entry from the same pin.
    /// Without one there is no model list to attribute them to yet.
    fn record_model_selects(
        &self,
        agent_id: &AgentId,
        adapter_version: Option<&str>,
        observed: &BTreeMap<String, AgentModelSelects>,
    ) {
        if observed.is_empty() {
            return;
        }
        let changed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(entry) = entries.get_mut(&agent_id.0) else {
                return;
            };
            if adapter_version.is_some() && entry.adapter_version.as_deref() != adapter_version {
                return;
            }
            let listed =
                |model_id: &String| entry.models.iter().any(|model| &model.model_id == model_id);
            let before = entry.model_selects.len();
            let mut model_selects = std::mem::take(&mut entry.model_selects);
            model_selects.retain(|model_id, _| listed(model_id));
            let mut changed = model_selects.len() != before;
            for (model_id, selects) in observed.iter().filter(|(model_id, _)| listed(model_id)) {
                if model_selects.get(model_id) != Some(selects) {
                    model_selects.insert(model_id.clone(), selects.clone());
                    changed = true;
                }
            }
            entry.model_selects = model_selects;
            if changed {
                entry.persisted_at_unix = unix_now();
            }
            changed
        };
        if changed {
            self.persist();
            self.announce_change(agent_id);
        }
    }

    /// Each listed model's own thought-level select, where a live session has
    /// observed it. Writes drop models no longer listed.
    pub fn model_selects_for(&self, agent_id: &AgentId) -> BTreeMap<String, AgentModelSelects> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .map(|entry| entry.model_selects.clone())
            .unwrap_or_default()
    }

    /// Store a probe's observed model and image capabilities for an agent.
    /// Recent live session models/selects from the same pin take precedence.
    /// An observation with neither models, image information, nor session
    /// selects is degraded and ignored; each is still independently cacheable.
    pub fn record(
        &self,
        agent_id: &AgentId,
        models: Vec<AgentModelOption>,
        default_model_id: Option<String>,
        prompt_image: Option<bool>,
        adapter_version: Option<String>,
        session_selects: Option<AgentSessionSelects>,
    ) {
        self.record_observation(
            agent_id,
            CatalogObservation {
                models,
                default_model_id,
                prompt_image,
                adapter_version,
                session_selects,
            },
            CatalogSource::Probe,
        );
    }

    fn record_observation(
        &self,
        agent_id: &AgentId,
        observation: CatalogObservation,
        source: CatalogSource,
    ) {
        let CatalogObservation {
            models,
            default_model_id,
            prompt_image,
            adapter_version,
            session_selects,
        } = observation;
        if models.is_empty() && prompt_image.is_none() && session_selects.is_none() {
            return;
        }
        let now = unix_now();
        // Separate persistence from content changes: an unchanged answer can
        // still need its timestamp or live provenance flushed. Announcing that
        // would make `engine.list` ask its own caller to poll again.
        let (changed, should_persist) = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let existing = entries.get(&agent_id.0);
            // Decide and merge under the same lock as live recording. Checking
            // before a probe starts (or outside this lock) would miss a live
            // session arriving while the probe is delayed.
            let same_pin = existing.is_some_and(|entry| entry.adapter_version == adapter_version);
            let prior_models_at = existing.and_then(|entry| entry.live_models_at_unix);
            let prior_selects_at = existing.and_then(|entry| entry.live_selects_at_unix);
            let protects = |at: Option<u64>| {
                source == CatalogSource::Probe
                    && same_pin
                    && at.is_some_and(|at| now.saturating_sub(at) < CATALOG_TTL.as_secs())
            };
            let prefer_live_models = protects(prior_models_at);
            let prefer_live_selects = protects(prior_selects_at);
            let observes_models = !models.is_empty();
            let observes_selects = session_selects.is_some();
            let accepted_probe = source == CatalogSource::Probe
                && ((observes_models && !prefer_live_models)
                    || (observes_selects && !prefer_live_selects));
            let prior_models_observed_at = existing.and_then(|entry| entry.models_observed_at_unix);
            let prior_selects_observed_at =
                existing.and_then(|entry| entry.selects_observed_at_unix);
            let observed_timestamp = |prior: Option<u64>, observed: bool, protected: bool| {
                if observed && !protected {
                    Some(now)
                } else {
                    existing.map(|entry| prior.unwrap_or(entry.probed_at_unix))
                }
            };
            let models_observed_at_unix = observed_timestamp(
                prior_models_observed_at,
                observes_models,
                prefer_live_models,
            );
            let selects_observed_at_unix = observed_timestamp(
                prior_selects_observed_at,
                observes_selects,
                prefer_live_selects,
            );
            let live_timestamp = |prior, observed, protected| {
                if source == CatalogSource::Live && observed {
                    Some(now)
                } else if same_pin && (!observed || protected) {
                    prior
                } else {
                    None
                }
            };
            let live_models_at_unix =
                live_timestamp(prior_models_at, observes_models, prefer_live_models);
            let live_selects_at_unix =
                live_timestamp(prior_selects_at, observes_selects, prefer_live_selects);
            let freshness_metadata_changed = prior_models_at.is_some()
                != live_models_at_unix.is_some()
                || prior_selects_at.is_some() != live_selects_at_unix.is_some()
                || prior_models_observed_at.is_some() != models_observed_at_unix.is_some()
                || prior_selects_observed_at.is_some() != selects_observed_at_unix.is_some();
            let prefer_live = prefer_live_models || prefer_live_selects;
            let observed_at = if prefer_live {
                existing.expect("protected live entry").probed_at_unix
            } else {
                now
            };
            let (models, default_model_id) = if prefer_live_models || models.is_empty() {
                existing
                    .map(|entry| (entry.models.clone(), entry.default_model_id.clone()))
                    .unwrap_or_default()
            } else {
                (models, default_model_id)
            };
            // A caller with nothing to say about image support must not erase
            // what a probe already learned.
            let prompt_image =
                prompt_image.or_else(|| existing.and_then(|existing| existing.prompt_image));
            // Same for the session selects: `None` is "no session was created",
            // not "the agent stopped advertising them".
            let session_selects = if prefer_live_selects {
                existing.and_then(|entry| entry.session_selects.clone())
            } else {
                session_selects
                    .or_else(|| existing.and_then(|existing| existing.session_selects.clone()))
            };
            // `record` knows nothing about sign-ins; replacing the entry must
            // not throw away what a probe learned about them.
            let auth_methods = existing.and_then(|existing| existing.auth_methods.clone());
            let auth_state = existing.and_then(|existing| existing.auth_state);
            let agent_advertises = existing.and_then(|existing| existing.agent_advertises.clone());
            let model_selects = existing
                .filter(|_| same_pin)
                .map(|existing| {
                    let mut model_selects = existing.model_selects.clone();
                    model_selects.retain(|model_id, _| {
                        models.iter().any(|model| &model.model_id == model_id)
                    });
                    model_selects
                })
                .unwrap_or_default();
            let adapter_version = adapter_version
                .or_else(|| existing.and_then(|entry| entry.adapter_version.clone()));
            match entries.get_mut(&agent_id.0) {
                Some(existing)
                    if existing.models == models
                        && existing.default_model_id == default_model_id
                        && existing.prompt_image == prompt_image
                        && existing.auth_state == auth_state
                        && existing.adapter_version == adapter_version
                        && existing.session_selects == session_selects =>
                {
                    // Accepted probes complete a durable TTL refresh even if
                    // only one stale field was re-observed without changing.
                    // Repeated live reads only flush hourly, avoiding a write
                    // on every engine.list.
                    let persisted_age = now.saturating_sub(existing.persisted_at_unix);
                    existing.probed_at_unix = observed_at;
                    existing.live_models_at_unix = live_models_at_unix;
                    existing.live_selects_at_unix = live_selects_at_unix;
                    existing.models_observed_at_unix = models_observed_at_unix;
                    existing.selects_observed_at_unix = selects_observed_at_unix;
                    let should_persist = freshness_metadata_changed
                        || accepted_probe
                        || (!prefer_live && persisted_age >= TIMESTAMP_REFRESH_INTERVAL.as_secs());
                    if should_persist {
                        existing.persisted_at_unix = now;
                    }
                    (false, should_persist)
                }
                _ => {
                    entries.insert(
                        agent_id.0.clone(),
                        CatalogEntry {
                            models,
                            default_model_id,
                            prompt_image,
                            auth_methods,
                            auth_state,
                            adapter_version,
                            session_selects,
                            model_selects,
                            agent_advertises,
                            live_models_at_unix,
                            live_selects_at_unix,
                            models_observed_at_unix,
                            selects_observed_at_unix,
                            probed_at_unix: observed_at,
                            persisted_at_unix: now,
                        },
                    );
                    (true, true)
                }
            }
        };
        if should_persist {
            self.persist();
        }
        if changed {
            self.announce_change(agent_id);
        }
    }

    /// True when a probe was attempted (any outcome) within the backoff window,
    /// so a failed or empty probe isn't retried on every `engine.list` poll.
    fn recently_attempted(&self, agent_id: &AgentId) -> bool {
        self.last_attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .is_some_and(|at| unix_now().saturating_sub(*at) < PROBE_RETRY_BACKOFF.as_secs())
    }

    /// Drop the backoff stamp for one agent so its next `spawn_probe` may try
    /// again immediately. For when the world changed out from under a failed
    /// probe — a sign-in succeeding means the auth-required failure it recorded
    /// minutes ago says nothing anymore.
    pub fn forget_attempt(&self, agent_id: &AgentId) {
        self.last_attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&agent_id.0);
    }

    /// A successful sign-in invalidates the probe verdict captured while the
    /// agent was signed out. Clear it durably so a Core restart cannot restore
    /// `Missing`; the absent field also forces the next catalog probe.
    pub(crate) fn forget_auth_state(&self, agent_id: &AgentId) {
        let changed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            entries
                .get_mut(&agent_id.0)
                .is_some_and(|entry| entry.auth_state.take().is_some())
        };
        if changed {
            self.persist();
            self.announce_change(agent_id);
        }
    }

    /// A successful sign-in can change what the agent offers: omp lists only
    /// the providers it holds credentials for. A live session's models from
    /// before it would otherwise outrank the next probe for `CATALOG_TTL`, so
    /// drop the live provenance and let that probe's answer stand.
    pub(crate) fn forget_live_observations(&self, agent_id: &AgentId) {
        let changed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            entries.get_mut(&agent_id.0).is_some_and(|entry| {
                let models = entry.live_models_at_unix.take().is_some();
                let selects = entry.live_selects_at_unix.take().is_some();
                models || selects
            })
        };
        if changed {
            self.persist();
        }
    }

    /// Drop everything known about one agent — the cached catalog and the
    /// probe backoff. For an uninstall: what the removed adapter advertised
    /// says nothing about whatever gets installed next, and a pin-identical
    /// reinstall must re-verify rather than serve the old answer.
    pub fn forget_agent(&self, agent_id: &AgentId) {
        let removed = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&agent_id.0)
            .is_some();
        self.forget_attempt(agent_id);
        if removed {
            self.persist();
            self.announce_change(agent_id);
        }
    }

    fn mark_attempt(&self, agent_id: &AgentId) {
        self.last_attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(agent_id.0.clone(), unix_now());
    }

    fn needs_probe(&self, agent_id: &AgentId, adapter_version: Option<&str>) -> bool {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match entries.get(&agent_id.0) {
            None => true,
            // The pinned adapter moved: what the old version advertised says
            // nothing about the new one.
            Some(entry) if entry.adapter_version.as_deref() != adapter_version => true,
            Some(entry) if entry.models.is_empty() && entry.prompt_image.is_none() => true,
            // Entries written before defaults were captured: re-probe so the
            // composer can resolve "Default" to a concrete model.
            Some(entry) if entry.default_model_id.is_none() => true,
            // Entries written before image support was captured: re-probe so
            // the composer knows whether attaching is allowed.
            Some(entry) if entry.prompt_image.is_none() => true,
            // Entries written before sign-ins were captured: re-probe so the
            // engine picker can offer one on a cold launch.
            Some(entry) if entry.auth_methods.is_none() => true,
            // Entries written before the sign-in verdict was persisted need a
            // fresh probe even if the rest of their catalog is still young.
            Some(entry) if entry.auth_state.is_none() => true,
            // Entries written before the advertised feature set was captured:
            // re-probe so the compatibility view has an answer on cold launch.
            Some(entry) if entry.agent_advertises.is_none() => true,
            // Entries written before mode/thought-level selects were captured:
            // re-probe so the composer can offer them.
            Some(entry) if entry.session_selects.is_none() => true,
            Some(entry) => {
                let now = unix_now();
                let models_at = entry
                    .models_observed_at_unix
                    .unwrap_or(entry.probed_at_unix);
                let selects_at = entry
                    .selects_observed_at_unix
                    .unwrap_or(entry.probed_at_unix);
                now.saturating_sub(models_at) >= CATALOG_TTL.as_secs()
                    || now.saturating_sub(selects_at) >= CATALOG_TTL.as_secs()
            }
        }
    }

    /// Listing hook retained without adapter launches. Catalog discovery is
    /// driven by explicit instance use, not a sweep of all registered homes.
    pub fn spawn_probes(self: &Arc<Self>, registry: &Arc<AgentRegistry>) {
        // Listings are intentionally lazy. Keep the entrypoint for callers
        // that refresh runtime metadata, without launching account adapters.
        let _ = registry;
    }

    /// Refresh one used instance when stale, respecting backoff and the test
    /// gate. Explicit Recheck and install verification use `probe_now`, or
    /// [`Self::reprobe_in_background`].
    pub fn spawn_probe(self: &Arc<Self>, registry: &Arc<AgentRegistry>, selected: &AgentId) {
        if background_probes_off() {
            return;
        }
        for definition in registry.definitions() {
            let agent_id = definition.agent_id.clone();
            if &agent_id != selected || self.deleting.lock().unwrap().contains_key(&agent_id.0) {
                continue;
            }
            // A pinned adapter that is not installed must not be probed: the
            // launch spec would fall back to `npx`, fetching and running the
            // package before the user ever pressed Get. Skipping here rather
            // than failing inside the probe also leaves the retry backoff
            // untouched, so the probe runs promptly once the install lands.
            if registry.adapter_needs_install(&definition) {
                continue;
            }
            let adapter_version = pinned_adapter_package(&definition);
            if !self.needs_probe(&agent_id, adapter_version.as_deref())
                || self.recently_attempted(&agent_id)
            {
                continue;
            }
            let Ok((sender, _waiter)) = self.begin_probe(&agent_id) else {
                continue;
            };
            // Stamp the attempt before spawning so the backoff window covers a
            // failed probe even after `in_flight` clears.
            self.mark_attempt(&agent_id);
            let catalog = Arc::clone(self);
            let registry = Arc::clone(registry);
            tokio::spawn(async move {
                let outcome = catalog
                    .run_probe_and_record(&registry, &agent_id, adapter_version)
                    .await;
                catalog.finish_probe(&agent_id, sender, outcome);
            });
        }
    }

    /// [`Self::probe_now`] without waiting: an explicit Recheck that must not
    /// stall on a cold CLI start. It ignores freshness and backoff, since the
    /// user may have just changed the agent outside Ottam, but honors the
    /// test gate.
    pub(crate) fn reprobe_in_background(
        self: &Arc<Self>,
        registry: &Arc<AgentRegistry>,
        agent_id: &AgentId,
    ) {
        if background_probes_off() {
            return;
        }
        let catalog = Arc::clone(self);
        let registry = Arc::clone(registry);
        let agent_id = agent_id.clone();
        tokio::spawn(async move {
            // `run_probe_and_record` logs a failure.
            let _ = catalog.probe_now(&registry, &agent_id).await;
        });
    }

    /// Probe one agent right now and wait for the outcome. This is what an
    /// adapter install's `verifying` phase runs: it bypasses the freshness
    /// check, the retry backoff, and the `OTTAM_MODEL_CATALOG_PROBE` gate
    /// — the caller explicitly asked for a handshake, so staleness heuristics
    /// for background polling don't apply. If a probe for the agent is already
    /// in flight, joins it instead of starting a second one.
    pub(crate) async fn probe_now(
        self: &Arc<Self>,
        registry: &Arc<AgentRegistry>,
        agent_id: &AgentId,
    ) -> Result<(), HarnessError> {
        if self.deleting.lock().unwrap().contains_key(&agent_id.0) {
            return Err(HarnessError::new(
                HarnessErrorKind::AgentNotFound,
                "This agent was removed",
            ));
        }
        // The probe always runs as a detached task, never inline on the
        // caller's future. A caller that goes away — an install racing this
        // against its cancellation token — would otherwise drop the probe
        // mid-flight and strand the slot, leaving every later probe joining a
        // waiter that never settles. Detached, the task settles the slot no
        // matter what the caller does; this function only ever awaits.
        let waiter = match self.begin_probe(agent_id) {
            Ok((sender, waiter)) => {
                // Stamp the attempt so background probes back off from this
                // one, whatever its outcome.
                self.mark_attempt(agent_id);
                let adapter_version = registry
                    .definitions()
                    .into_iter()
                    .find(|definition| &definition.agent_id == agent_id)
                    .as_ref()
                    .and_then(pinned_adapter_package);
                let catalog = Arc::clone(self);
                let registry = Arc::clone(registry);
                let agent_id = agent_id.clone();
                tokio::spawn(async move {
                    let outcome = catalog
                        .run_probe_and_record(&registry, &agent_id, adapter_version)
                        .await;
                    catalog.finish_probe(&agent_id, sender, outcome);
                });
                waiter
            }
            Err(waiter) => waiter,
        };
        Self::await_probe(agent_id, waiter).await
    }

    /// Verify a newly published adapter even if an older catalog probe was
    /// already in flight. Anything occupying the slot at entry may have
    /// launched from the replaced tree, so wait for it to settle and only then
    /// begin (or join) a probe created after publication.
    pub(crate) async fn probe_now_fresh(
        self: &Arc<Self>,
        registry: &Arc<AgentRegistry>,
        agent_id: &AgentId,
    ) -> Result<(), HarnessError> {
        let existing = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .cloned();
        if let Some(waiter) = existing {
            let _ = Self::await_probe(agent_id, waiter).await;
        }
        self.probe_now(registry, agent_id).await
    }

    /// Block until the owning task settles the probe.
    async fn await_probe(agent_id: &AgentId, mut waiter: ProbeWaiter) -> ProbeOutcome {
        loop {
            if let Some(outcome) = waiter.borrow_and_update().clone() {
                return outcome;
            }
            if waiter.changed().await.is_err() {
                // The probing task was dropped without settling (runtime
                // shutdown); report that rather than hanging.
                return Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!("model catalog probe for {agent_id} was abandoned"),
                ));
            }
        }
    }

    /// Claim the probe slot for an agent. `Ok((sender, waiter))` means the
    /// caller owns the probe and must settle it via [`Self::finish_probe`];
    /// `Err(waiter)` means one is already running and can be awaited instead.
    fn begin_probe(&self, agent_id: &AgentId) -> Result<(ProbeOwner, ProbeWaiter), ProbeWaiter> {
        let deleting = self.deleting.lock().unwrap();
        if deleting.contains_key(&agent_id.0) {
            let (_, waiter) = tokio::sync::watch::channel(Some(Err(HarnessError::new(
                HarnessErrorKind::AgentNotFound,
                "This agent was removed",
            ))));
            return Err(waiter);
        }
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(waiter) = in_flight.get(&agent_id.0) {
            return Err(waiter.clone());
        }
        let (sender, waiter) = tokio::sync::watch::channel(None);
        in_flight.insert(agent_id.0.clone(), waiter.clone());
        Ok((sender, waiter))
    }

    /// Release the probe slot and wake every joined waiter with the outcome.
    fn finish_probe(&self, agent_id: &AgentId, sender: ProbeOwner, outcome: ProbeOutcome) {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&agent_id.0);
        let _ = sender.send(Some(outcome));
    }

    /// The shared probe body: run the handshake, record what it advertised.
    /// A failure keeps any cached catalog — stale models beat no models.
    async fn run_probe_and_record(
        self: &Arc<Self>,
        registry: &Arc<AgentRegistry>,
        agent_id: &AgentId,
        adapter_version: Option<String>,
    ) -> Result<(), HarnessError> {
        let definition = registry.definition(agent_id)?;
        let harness_slot = self
            .harness_slots
            .lock()
            .unwrap()
            .entry(definition.harness_id().into())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone();
        let _harness_slot = harness_slot.acquire().await.map_err(|_| {
            HarnessError::new(HarnessErrorKind::Internal, "Catalog admission stopped")
        })?;
        let _global_slot = self.probe_slots.acquire().await.map_err(|_| {
            HarnessError::new(HarnessErrorKind::Internal, "Catalog admission stopped")
        })?;
        registry.definition(agent_id)?;
        // The probe bounds itself and always shuts the throwaway adapter
        // down, so a timeout can't leak a live child process.
        match catalog_probe::probe(registry, agent_id).await {
            Ok(probe) => {
                tracing::info!(
                    target: "ottam_core::agent",
                    agent_id = %agent_id,
                    model_count = probe.models.len(),
                    default_model_id = ?probe.default_model_id,
                    prompt_image = probe.prompt_image,
                    auth_method_count = probe.auth_methods.len(),
                    "model catalog probe completed"
                );
                self.record_auth_methods(agent_id, probe.auth_methods);
                self.record_auth_state(agent_id, probe.auth_state);
                self.record_advertised_features(agent_id, probe.agent_advertises);
                self.record(
                    agent_id,
                    probe.models,
                    probe.default_model_id,
                    Some(probe.prompt_image),
                    adapter_version,
                    probe.session_selects,
                );
                // The verdict is the probe's most actionable finding for a
                // signed-out agent: without it the picker offers an engine
                // that will refuse its first prompt, and Settings shows no
                // Sign In until then.
                registry.note_probed_auth(agent_id, probe.auth_state);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    target: "ottam_core::agent",
                    agent_id = %agent_id,
                    error = %error,
                    "model catalog probe failed; keeping any cached catalog"
                );
                Err(error)
            }
        }
    }

    pub(crate) fn fence_delete(&self, agent_id: &AgentId) -> CatalogDeleteFence<'_> {
        *self
            .deleting
            .lock()
            .unwrap()
            .entry(agent_id.0.clone())
            .or_default() += 1;
        CatalogDeleteFence {
            catalog: self,
            agent_id: agent_id.clone(),
        }
    }

    #[cfg(test)]
    fn adapter_version_for(&self, agent_id: &AgentId) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&agent_id.0)
            .and_then(|entry| entry.adapter_version.clone())
    }

    fn persist(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        // Serialize persistence so two racing writers can't interleave on the
        // shared temp file or the rename.
        let _guard = self
            .persist_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Ok(data) = serde_json::to_string_pretty(&snapshot) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Write to a sibling temp file then atomically rename, so a crash or a
        // concurrent reader never observes a half-written catalog.
        let tmp = path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&tmp, &data) {
            tracing::warn!(
                target: "ottam_core::agent",
                path = %tmp.display(),
                error = %error,
                "failed to write model catalog temp file"
            );
            return;
        }
        if let Err(error) = std::fs::rename(&tmp, path) {
            tracing::warn!(
                target: "ottam_core::agent",
                path = %path.display(),
                error = %error,
                "failed to persist model catalog"
            );
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// `OTTAM_MODEL_CATALOG_PROBE=off` stops probes nobody waits on, for tests
/// that must not launch adapters.
fn background_probes_off() -> bool {
    std::env::var("OTTAM_MODEL_CATALOG_PROBE").is_ok_and(|value| value == "off")
}

/// The pinned `package@version` an npm-launched adapter runs, as recorded on a
/// catalog entry. `None` for agents launched some other way (the in-process
/// test agent), which then age out on the TTL alone.
pub(crate) fn pinned_adapter_package(definition: &AgentDefinition) -> Option<String> {
    definition.adapter.as_ref().map(|package| package.spec())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delete_fences_are_owned_and_survive_a_cancelled_waiter() {
        let catalog = Arc::new(ModelCatalogService::load(None));
        let id = AgentId::new("codex.profile");
        let (sender, _) = catalog.begin_probe(&id).unwrap();
        let first = catalog.fence_delete(&id);
        let second = catalog.fence_delete(&id);
        drop(second);
        assert!(catalog.begin_probe(&id).is_err());
        let cancelled_catalog = catalog.clone();
        let cancelled_id = id.clone();
        let pending = tokio::spawn(async move {
            let fence = cancelled_catalog.fence_delete(&cancelled_id);
            fence.wait().await;
        });
        tokio::task::yield_now().await;
        pending.abort();
        let _ = pending.await;
        assert!(catalog.begin_probe(&id).is_err());
        catalog.finish_probe(&id, sender, Ok(()));
        first.wait().await;
        assert!(catalog.begin_probe(&id).is_err());
        drop(first);
        let (sender, _) = catalog
            .begin_probe(&id)
            .expect("last owner released its fence");
        catalog.finish_probe(&id, sender, Ok(()));
    }

    fn option(id: &str) -> AgentModelOption {
        AgentModelOption {
            model_id: id.to_string(),
            display_name: id.to_uppercase(),
        }
    }

    /// The adapter pin these tests probe against; only the pin-bump test cares
    /// about its value.
    fn pin() -> Option<String> {
        Some("adapter@1.0.0".to_string())
    }

    /// A real (empty) selects answer — what a probe records for an agent that
    /// advertises neither modes nor thought levels.
    fn no_selects() -> Option<AgentSessionSelects> {
        Some(AgentSessionSelects::default())
    }

    #[test]
    fn record_and_models_for_round_trip_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet"), option("haiku")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        // A complete entry now includes the sign-ins, so record the (genuinely
        // empty) answer or the reload below would ask for a fresh probe.
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Missing);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());

        let reloaded = ModelCatalogService::load(Some(path));
        assert_eq!(
            reloaded.models_for(&agent_id),
            vec![option("sonnet"), option("haiku")]
        );
        assert_eq!(
            reloaded
                .entries
                .lock()
                .expect("entries")
                .get(&agent_id.0)
                .and_then(|entry| entry.auth_state),
            Some(HarnessAuthState::Missing)
        );
        assert!(!reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    /// The push channel behind `engine.changed`. What matters is not that a
    /// change is announced — it is that an unchanged catalog is silent: every
    /// `engine.list` folds the live adapter's answer back in, so announcing
    /// those would have each poll ask its own caller to poll again.
    #[test]
    fn only_content_changes_are_announced() {
        let catalog = ModelCatalogService::load(None);
        let announced = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&announced);
        catalog.set_change_sink(Arc::new(move |agent_id: &AgentId| {
            recorder
                .lock()
                .expect("announced agents")
                .push(agent_id.0.clone());
        }));
        let agent_id = AgentId::new("claude");
        let announcements = || announced.lock().expect("announced agents").clone();

        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        assert_eq!(announcements(), vec!["claude".to_string()]);

        // The same observation again, which is what every engine.list produces
        // once a probe has landed. Only the TTL clock moves.
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        assert_eq!(
            announcements(),
            vec!["claude".to_string()],
            "an unchanged entry must not announce a change"
        );

        // A model the cache did not have — the case that leaves a client's
        // picker stale without this channel.
        catalog.record(
            &agent_id,
            vec![option("sonnet"), option("fable")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        // Sign-ins and the uninstall path change what engine.list answers too.
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.forget_agent(&agent_id);
        assert_eq!(
            announcements(),
            vec![
                "claude".to_string(),
                "claude".to_string(),
                "claude".to_string(),
                "claude".to_string()
            ],
            "new models, first sign-in answer, and the uninstall announce; the repeat does not"
        );
    }

    /// The stale-probe case: a probe cached an older model list, then a real
    /// session advertised the current one.
    #[test]
    fn live_sessions_replace_a_probed_catalog() {
        let registry = AgentRegistry::with_runtime_probe(Arc::new(
            super::super::registry::SystemRuntimeProbe::default(),
        ));
        let agent_id = AgentId::new("claude");
        let definition = registry.definition(&agent_id).unwrap();
        let catalog = ModelCatalogService::load(None);
        catalog.record(
            &agent_id,
            vec![option("sonnet-5")],
            Some("sonnet-5".to_string()),
            Some(true),
            pinned_adapter_package(&definition),
            no_selects(),
        );

        // An adapter that has not created a session yet has nothing to say.
        catalog.record_live(&definition, &AdapterStatus::stopped());
        assert_eq!(catalog.models_for(&agent_id), vec![option("sonnet-5")]);

        let mut live = AdapterStatus::stopped();
        live.lifecycle = super::super::types::AdapterLifecycleState::Ready;
        live.models = vec![option("sonnet-5"), option("sonnet-5-5")];
        live.default_model_id = Some("sonnet-5-5".to_string());
        live.capabilities.prompt_image = true;
        catalog.record_live(&definition, &live);
        assert_eq!(
            catalog.catalog_for(&agent_id),
            (
                vec![option("sonnet-5"), option("sonnet-5-5")],
                Some("sonnet-5-5".to_string())
            )
        );
        assert_eq!(
            catalog.adapter_version_for(&agent_id),
            pinned_adapter_package(&definition)
        );
    }

    fn live_status(models: Vec<AgentModelOption>) -> AdapterStatus {
        let mut status = AdapterStatus::stopped();
        status.lifecycle = super::super::types::AdapterLifecycleState::Ready;
        status.default_model_id = models.first().map(|model| model.model_id.clone());
        status.models = models;
        status.session_selects = no_selects();
        status
    }

    #[test]
    fn probes_preserve_recent_live_models_and_selects_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-catalog.json");
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let mut live = live_status(vec![option("current")]);
        live.session_selects.as_mut().unwrap().default_mode_id = Some("plan".into());
        // First record the identical answer from a probe: recording it live
        // must persist its provenance even though the content did not change.
        catalog.record(
            agent_id,
            live.models.clone(),
            live.default_model_id.clone(),
            Some(false),
            pin(),
            live.session_selects.clone(),
        );
        catalog.record_live(&definition, &live);

        for catalog in [catalog, ModelCatalogService::load(Some(path))] {
            catalog.record(
                agent_id,
                vec![option("old")],
                Some("old".into()),
                Some(true),
                pin(),
                no_selects(),
            );
            assert_eq!(
                catalog.catalog_for(agent_id),
                (live.models.clone(), live.default_model_id.clone())
            );
            assert_eq!(catalog.session_selects_for(agent_id), live.session_selects);
            // Initialize metadata can still be learned from the probe.
            assert!(catalog.prompt_image_for(agent_id));
        }
    }

    #[test]
    fn per_model_selects_accumulate_survive_probes_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-catalog.json");
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let level = |id: &str| AgentModelSelects {
            thought_levels: vec![super::super::types::AgentSelectOption {
                id: id.into(),
                display_name: id.into(),
            }],
            default_thought_level_id: Some(id.into()),
        };
        let mut live = live_status(vec![option("kimi"), option("glm")]);
        live.model_selects.insert("kimi".into(), level("high"));
        catalog.record_live(&definition, &live);
        // A later session in another generation adds a model without
        // forgetting the first; models no longer listed are not reported.
        live.model_selects.clear();
        live.model_selects.insert("glm".into(), level("max"));
        live.model_selects.insert("gone".into(), level("low"));
        catalog.record_session_change(&definition, &live);
        // Probes never switch models, so they leave the map alone.
        catalog.record(
            agent_id,
            live.models.clone(),
            live.default_model_id.clone(),
            Some(false),
            pin(),
            no_selects(),
        );

        let expected = BTreeMap::from([
            ("glm".to_string(), level("max")),
            ("kimi".to_string(), level("high")),
        ]);
        assert_eq!(catalog.model_selects_for(agent_id), expected);
        assert_eq!(
            ModelCatalogService::load(Some(path)).model_selects_for(agent_id),
            expected,
            "per-model selects persist"
        );

        // A model the agent stops listing loses its selects.
        catalog.record_live(&definition, &live_status(vec![option("glm")]));
        assert_eq!(
            catalog.model_selects_for(agent_id),
            BTreeMap::from([("glm".to_string(), level("max"))])
        );
    }

    #[test]
    fn live_updates_replace_models_and_clear_selects_without_unioning() {
        let definition = probe_now::definition();
        let catalog = ModelCatalogService::load(None);
        let mut live = live_status(vec![option("removed"), option("current")]);
        live.session_selects.as_mut().unwrap().default_mode_id = Some("plan".into());
        catalog.record_live(&definition, &live);

        let mut renamed = option("current");
        renamed.display_name = "Updated name".into();
        let updated = live_status(vec![renamed]);
        catalog.record_live(&definition, &updated);
        assert_eq!(
            catalog.catalog_for(&definition.agent_id),
            (updated.models, updated.default_model_id)
        );
        assert_eq!(
            catalog.session_selects_for(&definition.agent_id),
            no_selects()
        );
    }

    #[test]
    fn live_selects_alone_do_not_claim_unobserved_models() {
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(None);
        catalog.record(
            agent_id,
            vec![option("old")],
            Some("old".into()),
            Some(false),
            pin(),
            no_selects(),
        );
        // A loaded session reports selects, but has not advertised models.
        let mut loaded = live_status(Vec::new());
        loaded.session_selects.as_mut().unwrap().default_mode_id = Some("plan".into());
        catalog.record_live(&definition, &loaded);
        catalog.record(
            agent_id,
            vec![option("new")],
            Some("new".into()),
            Some(false),
            pin(),
            no_selects(),
        );
        assert_eq!(catalog.models_for(agent_id), vec![option("new")]);
        assert_eq!(catalog.catalog_for(agent_id).1, Some("new".into()));
        assert_eq!(
            catalog.session_selects_for(agent_id),
            loaded.session_selects
        );
    }

    #[test]
    fn partial_live_observations_do_not_postpone_probes_for_unobserved_fields() {
        // Exercise both session/load (selects only) and a model-only status.
        // Every other completeness check is satisfied, so only TTL scheduling
        // can request the probe that must refresh the unobserved field.
        for observes_models in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("model-catalog.json");
            let definition = probe_now::definition();
            let agent_id = &definition.agent_id;
            let catalog = ModelCatalogService::load(Some(path.clone()));
            catalog.record(
                agent_id,
                vec![option("cached")],
                Some("cached".into()),
                Some(false),
                pin(),
                no_selects(),
            );
            catalog.record_auth_methods(agent_id, Vec::new());
            catalog.record_auth_state(agent_id, HarnessAuthState::Ready);
            catalog.record_advertised_features(agent_id, BTreeMap::new());
            let mut persisted: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            persisted[&agent_id.0]["probedAtUnix"] = (unix_now() - CATALOG_TTL.as_secs()).into();
            // Exercise legacy complete entries too: their single old timestamp
            // must be retained for whichever field the live status omits.
            persisted[&agent_id.0]
                .as_object_mut()
                .unwrap()
                .remove("modelsObservedAtUnix");
            persisted[&agent_id.0]
                .as_object_mut()
                .unwrap()
                .remove("selectsObservedAtUnix");
            std::fs::write(&path, serde_json::to_string(&persisted).unwrap()).unwrap();
            let catalog = ModelCatalogService::load(Some(path.clone()));
            assert!(catalog.needs_probe(agent_id, pin().as_deref()));
            // A probe that only completed initialize likewise cannot renew
            // either session field when session/new did not yield an answer.
            catalog.record(agent_id, Vec::new(), None, Some(true), pin(), None);
            assert!(catalog.needs_probe(agent_id, pin().as_deref()));

            let mut partial = live_status(if observes_models {
                vec![option("current")]
            } else {
                Vec::new()
            });
            if observes_models {
                partial.session_selects = None;
            }
            for _ in 0..3 {
                catalog.record_live(&definition, &partial);
                assert!(
                    catalog.needs_probe(agent_id, pin().as_deref()),
                    "repeated partial live reads must not refresh the unobserved field's TTL"
                );
            }
            let reloaded = ModelCatalogService::load(Some(path.clone()));
            assert!(reloaded.needs_probe(agent_id, pin().as_deref()));
            catalog.record(
                agent_id,
                vec![option("cached")],
                Some("cached".into()),
                Some(false),
                pin(),
                no_selects(),
            );
            assert!(!catalog.needs_probe(agent_id, pin().as_deref()));
            let reloaded = ModelCatalogService::load(Some(path));
            assert!(!reloaded.needs_probe(agent_id, pin().as_deref()));
        }
    }

    #[test]
    fn live_provenance_flushes_hourly_despite_frequent_unchanged_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-catalog.json");
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let live = live_status(vec![option("current")]);
        catalog.record_live(&definition, &live);
        let announced = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&announced);
        catalog.set_change_sink(Arc::new(move |agent_id| {
            recorder.lock().unwrap().push(agent_id.clone());
        }));
        let stale = unix_now() - TIMESTAMP_REFRESH_INTERVAL.as_secs();
        {
            let mut entries = catalog.entries.lock().unwrap();
            let entry = entries.get_mut(&agent_id.0).unwrap();
            entry.persisted_at_unix = stale;
            entry.probed_at_unix = stale;
            entry.live_models_at_unix = Some(stale);
            entry.live_selects_at_unix = Some(stale);
        }
        catalog.persist();
        // Simulate intervening polls refreshing memory but not yet flushed.
        catalog
            .entries
            .lock()
            .unwrap()
            .get_mut(&agent_id.0)
            .unwrap()
            .probed_at_unix = unix_now();
        catalog.record_live(&definition, &live);
        let reloaded = ModelCatalogService::load(Some(path));
        let entries = reloaded.entries.lock().unwrap();
        assert!(entries[&agent_id.0].live_models_at_unix.unwrap() > stale);
        assert!(entries[&agent_id.0].live_selects_at_unix.unwrap() > stale);
        assert!(
            announced.lock().unwrap().is_empty(),
            "freshness alone is silent"
        );
    }

    /// After a sign-in the probe sees providers the earlier live session
    /// could not, so its answer must replace the live one.
    #[test]
    fn forgetting_live_observations_lets_the_next_probe_replace_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-catalog.json");
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(Some(path.clone()));
        catalog.record_live(&definition, &live_status(vec![option("before-login")]));
        let probe = || {
            catalog.record(
                agent_id,
                vec![option("before-login"), option("after-login")],
                Some("before-login".into()),
                Some(false),
                pin(),
                no_selects(),
            )
        };
        probe();
        assert_eq!(
            catalog.models_for(agent_id),
            vec![option("before-login")],
            "a fresh live session outranks a probe"
        );

        catalog.forget_live_observations(agent_id);
        probe();
        assert_eq!(
            catalog.models_for(agent_id),
            vec![option("before-login"), option("after-login")]
        );
        let reloaded = ModelCatalogService::load(Some(path));
        assert!(reloaded.entries.lock().unwrap()[&agent_id.0]
            .live_models_at_unix
            .is_none());
    }

    #[test]
    fn stopped_adapters_do_not_refresh_live_freshness_and_expired_probes_replace_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-catalog.json");
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let live = live_status(vec![option("removed"), option("old")]);
        catalog.record_live(&definition, &live);
        let mut persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stale = unix_now() - CATALOG_TTL.as_secs();
        persisted[&agent_id.0]["liveModelsAtUnix"] = stale.into();
        persisted[&agent_id.0]["liveSelectsAtUnix"] = stale.into();
        persisted[&agent_id.0]["modelsObservedAtUnix"] = stale.into();
        persisted[&agent_id.0]["selectsObservedAtUnix"] = stale.into();
        persisted[&agent_id.0]["probedAtUnix"] = stale.into();
        std::fs::write(&path, serde_json::to_string(&persisted).unwrap()).unwrap();
        let catalog = ModelCatalogService::load(Some(path));

        for lifecycle in [
            super::super::types::AdapterLifecycleState::Stopped,
            super::super::types::AdapterLifecycleState::Starting,
            super::super::types::AdapterLifecycleState::Stopping,
            super::super::types::AdapterLifecycleState::Dead,
        ] {
            let mut stopped = live.clone();
            stopped.lifecycle = lifecycle;
            catalog.record_live(&definition, &stopped);
            assert_eq!(
                catalog.entries.lock().unwrap()[&agent_id.0].probed_at_unix,
                stale,
                "retained fields from a non-ready adapter are not a new observation"
            );
        }
        assert!(catalog.needs_probe(agent_id, pin().as_deref()));
        catalog.record(
            agent_id,
            vec![option("replacement")],
            Some("replacement".into()),
            Some(false),
            pin(),
            no_selects(),
        );
        assert_eq!(
            catalog.catalog_for(agent_id),
            (vec![option("replacement")], Some("replacement".into()))
        );
    }

    #[test]
    fn pin_changes_and_uninstall_discard_live_precedence() {
        let definition = probe_now::definition();
        let agent_id = &definition.agent_id;
        let catalog = ModelCatalogService::load(None);
        let live = live_status(vec![option("old-pin")]);
        catalog.record_live(&definition, &live);
        catalog.record(
            agent_id,
            vec![option("new-pin")],
            Some("new-pin".into()),
            Some(false),
            Some("adapter@2.0.0".into()),
            no_selects(),
        );
        assert_eq!(catalog.models_for(agent_id), vec![option("new-pin")]);

        catalog.record_live(&definition, &live);
        catalog.forget_agent(agent_id);
        catalog.record(
            agent_id,
            vec![option("reinstalled")],
            Some("reinstalled".into()),
            Some(false),
            pin(),
            no_selects(),
        );
        assert_eq!(catalog.models_for(agent_id), vec![option("reinstalled")]);
    }

    #[test]
    fn empty_observations_do_not_erase_a_cached_catalog() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("codex");
        catalog.record(
            &agent_id,
            vec![option("gpt")],
            Some("gpt".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        catalog.record(&agent_id, Vec::new(), None, None, pin(), None);
        assert_eq!(catalog.models_for(&agent_id), vec![option("gpt")]);
    }

    /// Mode/thought-level selects ride the entry like image support does: they
    /// survive a restart, a silent caller cannot erase them, and entries from
    /// before they were captured re-probe once.
    #[test]
    fn session_selects_round_trip_and_survive_silent_callers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        let selects = AgentSessionSelects {
            modes: vec![super::super::types::AgentSelectOption {
                id: "plan".to_string(),
                display_name: "Plan".to_string(),
            }],
            default_mode_id: Some("plan".to_string()),
            thought_levels: vec![super::super::types::AgentSelectOption {
                id: "high".to_string(),
                display_name: "High".to_string(),
            }],
            default_thought_level_id: Some("high".to_string()),
        };
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            Some(selects.clone()),
        );
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));

        // A fold-in with no session observed leaves the answer alone.
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            None,
            pin(),
            None,
        );
        assert_eq!(
            catalog.session_selects_for(&agent_id),
            Some(selects.clone())
        );

        let reloaded = ModelCatalogService::load(Some(path));
        assert_eq!(reloaded.session_selects_for(&agent_id), Some(selects));
        assert!(!reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    /// Entries written before selects were captured re-probe once, so the
    /// composer gains mode/effort rows without waiting out the TTL.
    #[test]
    fn entries_without_observed_selects_re_probe() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            None,
        );
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));

        catalog.record(&agent_id, Vec::new(), None, None, pin(), no_selects());
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));
    }

    #[test]
    fn missing_empty_and_stale_entries_need_a_probe() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("claude");
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));

        // Entries without a captured default (pre-default cache files) re-probe.
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            None,
            Some(true),
            pin(),
            no_selects(),
        );
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));

        // Entries without observed image support (cache files written before
        // attachments existed) re-probe, so the composer stops refusing pastes
        // after one probe. Needs its own catalog: within one, `record` keeps a
        // previously observed answer rather than dropping back to unknown.
        let pre_attachment = ModelCatalogService::load(None);
        pre_attachment.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            None,
            pin(),
            no_selects(),
        );
        assert!(pre_attachment.needs_probe(&agent_id, pin().as_deref()));

        // Entries written before sign-ins were captured re-probe too, for the
        // same reason: the engine picker cannot offer one it has never seen.
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));

        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));

        {
            let stale = unix_now() - CATALOG_TTL.as_secs() - 1;
            let mut entries = catalog.entries.lock().expect("entries lock");
            let entry = entries.get_mut("claude").expect("entry");
            entry.probed_at_unix = stale;
            entry.models_observed_at_unix = Some(stale);
            entry.selects_observed_at_unix = Some(stale);
        }
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));
    }

    /// The reason the pin is recorded at all: a bumped adapter advertises a
    /// different catalog, so the cached answer has to go immediately rather
    /// than surviving until the TTL expires.
    #[test]
    fn a_bumped_adapter_pin_forces_a_fresh_probe() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));

        assert!(catalog.needs_probe(&agent_id, Some("adapter@2.0.0")));
        assert_eq!(catalog.adapter_version_for(&agent_id), pin());
    }

    /// Entries written before pins were recorded carry no version, so they
    /// re-probe once rather than being trusted indefinitely.
    #[test]
    fn entries_without_a_recorded_pin_re_probe() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            None,
            no_selects(),
        );
        catalog.record_auth_methods(&agent_id, Vec::new());
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));
    }

    #[test]
    fn the_pinned_package_is_the_definitions_adapter_spec() {
        let definition = AgentDefinition {
            launch: super::super::types::AgentLaunchSpec {
                command: "npx".to_string(),
                arguments: vec!["-y".to_string(), "pkg@1.2.3".to_string()],
                environment: Vec::new(),
                minimum_node_major: 18,
            },
            adapter: Some(super::super::types::AdapterPackage {
                name: "pkg".to_string(),
                version: "1.2.3".to_string(),
                source: crate::agent::AdapterSource::Npm,
            }),
            ..AgentDefinition::for_test("claude")
        };
        assert_eq!(
            pinned_adapter_package(&definition),
            Some("pkg@1.2.3".to_string())
        );

        let in_process = AgentDefinition {
            launch: super::super::types::AgentLaunchSpec {
                command: "in-process".to_string(),
                arguments: Vec::new(),
                ..definition.launch.clone()
            },
            adapter: None,
            ..definition
        };
        assert_eq!(pinned_adapter_package(&in_process), None);
    }

    fn wire_method(id: &str) -> AgentAuthMethod {
        AgentAuthMethod {
            method_id: id.to_string(),
            name: id.to_string(),
            description: None,
            kind: ottam_protocol::AgentAuthMethodKind::Terminal,
        }
    }

    /// The case this exists for: a signed-out agent's `session/new` fails, so
    /// it has no models to cache — but its sign-ins came off `initialize` and
    /// are exactly what the picker needs to offer a way out.
    #[test]
    fn sign_ins_cache_for_an_agent_with_no_models_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        catalog.record_auth_methods(&agent_id, vec![wire_method("claude-ai-login")]);

        let reloaded = ModelCatalogService::load(Some(path));
        assert_eq!(
            reloaded.auth_methods_for(&agent_id),
            vec![wire_method("claude-ai-login")]
        );
        // Still incomplete in every other respect, so the models are still worth
        // another probe.
        assert!(reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    /// `record` speaks only for models and images; it must not quietly drop an
    /// answer it was never asked about.
    #[test]
    fn recording_models_leaves_the_cached_sign_ins_alone() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("claude");
        catalog.record_auth_methods(&agent_id, vec![wire_method("claude-ai-login")]);
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        assert_eq!(
            catalog.auth_methods_for(&agent_id),
            vec![wire_method("claude-ai-login")]
        );
    }

    #[test]
    fn a_recent_probe_attempt_backs_off() {
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("other");
        assert!(!catalog.recently_attempted(&agent_id));
        catalog.mark_attempt(&agent_id);
        assert!(catalog.recently_attempted(&agent_id));

        // A stale attempt (beyond the backoff window) is retryable again.
        catalog.last_attempt.lock().expect("attempts lock").insert(
            agent_id.0.clone(),
            unix_now() - PROBE_RETRY_BACKOFF.as_secs() - 1,
        );
        assert!(!catalog.recently_attempted(&agent_id));
    }

    #[test]
    fn a_sign_in_forgets_the_backoff_so_the_next_probe_runs_now() {
        // The probe that failed while signed out recorded auth-required and
        // stamped the backoff; the login succeeding makes that answer stale.
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("other");
        catalog.mark_attempt(&agent_id);
        assert!(catalog.recently_attempted(&agent_id));
        catalog.forget_attempt(&agent_id);
        assert!(!catalog.recently_attempted(&agent_id));
    }

    #[test]
    fn a_sign_in_durably_invalidates_the_missing_auth_verdict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        catalog.record_auth_methods(&agent_id, vec![wire_method("claude-ai-login")]);
        catalog.record_auth_state(&agent_id, HarnessAuthState::Missing);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));

        catalog.forget_auth_state(&agent_id);
        let reloaded = ModelCatalogService::load(Some(path));
        assert!(reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    #[tokio::test]
    async fn old_adapter_sign_in_verdict_is_not_restored() {
        let registry = AgentRegistry::with_runtime_probe(Arc::new(
            super::super::registry::SystemRuntimeProbe::default(),
        ));
        let catalog = ModelCatalogService::load(None);
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            Some("@agentclientprotocol/claude-agent-acp@0.73.0".to_string()),
            no_selects(),
        );
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        catalog.restore_probed_auth(&registry);
        assert_eq!(
            registry.cached_status(&agent_id).await.unwrap().auth_state,
            HarnessAuthState::Unknown
        );

        let current_pin = pinned_adapter_package(&registry.definition(&agent_id).unwrap());
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            current_pin,
            no_selects(),
        );
        catalog.record_auth_state(&agent_id, HarnessAuthState::Missing);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        catalog.restore_probed_auth(&registry);
        assert_eq!(
            registry.cached_status(&agent_id).await.unwrap().auth_state,
            HarnessAuthState::Missing
        );
    }

    #[test]
    fn forgetting_an_agent_drops_its_catalog_and_backoff_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("codex");
        catalog.record(
            &agent_id,
            vec![option("gpt")],
            Some("gpt".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        catalog.mark_attempt(&agent_id);
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));

        catalog.forget_agent(&agent_id);
        assert!(catalog.models_for(&agent_id).is_empty());
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));
        assert!(!catalog.recently_attempted(&agent_id));

        // The removal is persisted, not just in memory: a relaunch after an
        // uninstall must not resurrect the old adapter's answer.
        let reloaded = ModelCatalogService::load(Some(path));
        assert!(reloaded.models_for(&agent_id).is_empty());
        assert!(reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    #[test]
    fn unchanged_catalog_refreshes_the_persisted_timestamp_once_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );

        // Backdate the probe time past the refresh interval, then re-observe the
        // identical catalog: the timestamp must flush to disk so a later restart
        // reads a fresh time and doesn't re-probe.
        let stale = unix_now() - TIMESTAMP_REFRESH_INTERVAL.as_secs() - 1;
        catalog
            .entries
            .lock()
            .expect("entries lock")
            .get_mut("claude")
            .expect("entry")
            .probed_at_unix = stale;
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );

        // A complete entry includes both the advertised sign-ins and the auth
        // verdict reached while opening the probe session.
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Ready);
        catalog.record_advertised_features(&agent_id, BTreeMap::new());

        let reloaded = ModelCatalogService::load(Some(path));
        let persisted = reloaded
            .entries
            .lock()
            .expect("entries lock")
            .get("claude")
            .expect("entry")
            .probed_at_unix;
        assert!(
            persisted > stale,
            "unchanged catalog should refresh disk timestamp"
        );
        assert!(!reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    #[test]
    fn image_support_survives_a_restart_and_is_not_erased_by_a_silent_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        assert!(!catalog.prompt_image_for(&agent_id));

        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        // A live-adapter fold-in that says nothing about images must leave the
        // probed answer alone — otherwise the composer would disable attaching
        // the moment `engine.list` ran against a stopped adapter.
        catalog.record(
            &agent_id,
            vec![option("sonnet"), option("haiku")],
            Some("sonnet".to_string()),
            None,
            pin(),
            None,
        );
        assert!(catalog.prompt_image_for(&agent_id));

        // This is the launch case: nothing has started an adapter yet, so the
        // answer has to come off disk.
        let reloaded = ModelCatalogService::load(Some(path));
        assert!(reloaded.prompt_image_for(&agent_id));
    }

    #[test]
    fn advertised_features_survive_a_restart_and_a_silent_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("claude");
        assert_eq!(
            catalog.advertised_features_for(&agent_id, pin().as_deref()),
            None
        );

        let advertised: BTreeMap<AcpFeature, bool> = [
            (AcpFeature::LoadSession, true),
            (AcpFeature::SessionList, false),
        ]
        .into_iter()
        .collect();
        catalog.record_advertised_features(&agent_id, advertised.clone());
        // An entry with only the advertised set still needs the rest probed.
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));
        // A live-adapter fold-in knows nothing about `initialize` and must not
        // erase what the probe learned.
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        assert_eq!(
            catalog
                .advertised_features_for(&agent_id, pin().as_deref())
                .as_ref(),
            Some(&advertised)
        );

        let reloaded = ModelCatalogService::load(Some(path.clone()));
        assert_eq!(
            reloaded
                .advertised_features_for(&agent_id, pin().as_deref())
                .as_ref(),
            Some(&advertised)
        );
        // A pin bump makes the stored answers someone else's: `agent.list`
        // reads this without probing, so it must come back empty rather than
        // pair the new package with the old adapter's matrix.
        assert_eq!(
            reloaded.advertised_features_for(&agent_id, Some("adapter@2.0.0")),
            None
        );
        // The wire name is the catalog key, so a rename would silently
        // orphan every stored answer.
        let raw = std::fs::read_to_string(&path).expect("catalog file");
        assert!(raw.contains("\"agentAdvertises\""));
        assert!(raw.contains("\"session.list\": false"));
    }

    /// An entry written before the advertised set was captured re-probes
    /// even when everything else about it is fresh.
    #[test]
    fn missing_advertised_features_trigger_a_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path));
        let agent_id = AgentId::new("claude");
        catalog.record(
            &agent_id,
            vec![option("sonnet")],
            Some("sonnet".to_string()),
            Some(true),
            pin(),
            no_selects(),
        );
        catalog.record_auth_methods(&agent_id, Vec::new());
        catalog.record_auth_state(&agent_id, HarnessAuthState::Unknown);
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));
        catalog.record_advertised_features(&agent_id, BTreeMap::new());
        assert!(!catalog.needs_probe(&agent_id, pin().as_deref()));
    }

    #[test]
    fn image_support_without_model_options_is_still_cacheable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        let catalog = ModelCatalogService::load(Some(path.clone()));
        let agent_id = AgentId::new("minimal");

        catalog.record(&agent_id, Vec::new(), None, Some(true), pin(), no_selects());
        assert!(catalog.prompt_image_for(&agent_id));
        assert!(catalog.needs_probe(&agent_id, pin().as_deref()));

        let reloaded = ModelCatalogService::load(Some(path));
        assert!(reloaded.prompt_image_for(&agent_id));
        assert!(reloaded.models_for(&agent_id).is_empty());
        assert!(reloaded.needs_probe(&agent_id, pin().as_deref()));
    }

    #[test]
    fn corrupt_cache_file_degrades_to_an_empty_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model-catalog.json");
        std::fs::write(&path, "not json").expect("write");
        let catalog = ModelCatalogService::load(Some(path));
        assert!(catalog.models_for(&AgentId::new("claude")).is_empty());
    }

    mod probe_now {
        use super::*;
        use crate::agent::registry::AdapterFactory;
        use crate::agent::types::{
            AdapterStatus, AgentLaunchSpec, AgentRuntimeStatus, HarnessAdapter, HarnessFuture,
            HarnessPrompt, HarnessSessionId, HarnessSessionSummary, HarnessStopReason,
            HarnessUpdateReceiver, HarnessUpdateSender, SessionConfigRequest,
        };
        use crate::agent::{ProbeCleanup, RuntimeProbe};
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::watch;
        use tokio_util::sync::CancellationToken;

        /// What the probes asked of their adapters, for the cleanup tests.
        #[derive(Default)]
        struct ProbeLog {
            arguments: Vec<Vec<String>>,
            metas: Vec<Option<serde_json::Map<String, serde_json::Value>>>,
            deleted: Vec<String>,
        }

        struct AlwaysReadyProbe;

        impl RuntimeProbe for AlwaysReadyProbe {
            fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
                AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
            }
        }

        /// An adapter whose status carries a canned model list; the probe reads
        /// it off after `start` + tolerated `new_session`.
        struct FakeProbeAdapter {
            definition: AgentDefinition,
            status_tx: watch::Sender<AdapterStatus>,
            updates_tx: HarnessUpdateSender,
            fail_start: bool,
            hold_start: Option<CancellationToken>,
            fail_delete: bool,
            sessions_created: Arc<AtomicUsize>,
            shutdowns: Arc<AtomicUsize>,
            /// Shutdowns still to refuse as a refresh in progress.
            refresh_busy: Arc<AtomicUsize>,
            log: Arc<Mutex<ProbeLog>>,
        }

        impl HarnessAdapter for FakeProbeAdapter {
            fn definition(&self) -> &AgentDefinition {
                &self.definition
            }

            fn status(&self) -> watch::Receiver<AdapterStatus> {
                self.status_tx.subscribe()
            }

            fn subscribe_updates(&self) -> HarnessUpdateReceiver {
                self.updates_tx.subscribe()
            }

            fn sessions(&self) -> Vec<HarnessSessionSummary> {
                Vec::new()
            }

            fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
                Box::pin(async move {
                    if let Some(gate) = &self.hold_start {
                        gate.cancelled().await;
                    }
                    if self.fail_start {
                        return Err(HarnessError::new(
                            HarnessErrorKind::Internal,
                            "adapter refused to start",
                        ));
                    }
                    Ok(self.status_tx.borrow().clone())
                })
            }

            fn shutdown(&self) -> HarnessFuture<'_, ()> {
                let busy = self
                    .refresh_busy
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                    .is_ok();
                if busy {
                    return Box::pin(std::future::ready(Err(HarnessError::new(
                        HarnessErrorKind::AuthRefreshBusy,
                        "refreshing",
                    ))));
                }
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
                Box::pin(std::future::ready(Ok(())))
            }

            fn authenticate(&self, _method_id: String) -> HarnessFuture<'_, ()> {
                Box::pin(std::future::ready(Ok(())))
            }

            fn new_session(
                &self,
                _cwd: PathBuf,
                config: SessionConfigRequest,
            ) -> HarnessFuture<'_, HarnessSessionId> {
                self.sessions_created.fetch_add(1, Ordering::SeqCst);
                self.log.lock().unwrap().metas.push(config.meta);
                Box::pin(std::future::ready(Ok(HarnessSessionId(
                    "probe-session".to_string(),
                ))))
            }

            fn load_session(
                &self,
                _session_id: HarnessSessionId,
                _cwd: PathBuf,
            ) -> HarnessFuture<'_, ()> {
                Box::pin(std::future::ready(Ok(())))
            }

            fn delete_session(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
                self.log.lock().unwrap().deleted.push(session_id.0);
                Box::pin(std::future::ready(if self.fail_delete {
                    Err(HarnessError::new(HarnessErrorKind::Internal, "delete refused"))
                } else {
                    Ok(())
                }))
            }

            fn prompt(
                &self,
                _session_id: HarnessSessionId,
                _prompt: HarnessPrompt,
            ) -> HarnessFuture<'_, HarnessStopReason> {
                Box::pin(async { unreachable!("the probe never prompts") })
            }

            fn steer(
                &self,
                _session_id: HarnessSessionId,
                _prompt: HarnessPrompt,
            ) -> HarnessFuture<'_, ()> {
                Box::pin(std::future::ready(Ok(())))
            }

            fn cancel(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
                Box::pin(std::future::ready(Ok(())))
            }
        }

        struct FakeProbeFactory {
            models: Vec<AgentModelOption>,
            created: AtomicUsize,
            fail_start: bool,
            hold_start: Option<CancellationToken>,
            auth_state: Option<HarnessAuthState>,
            fail_delete: bool,
            sessions_created: Arc<AtomicUsize>,
            shutdowns: Arc<AtomicUsize>,
            refresh_busy: Arc<AtomicUsize>,
            log: Arc<Mutex<ProbeLog>>,
        }

        impl FakeProbeFactory {
            fn with_models(models: Vec<AgentModelOption>) -> Self {
                Self {
                    models,
                    created: AtomicUsize::new(0),
                    fail_start: false,
                    hold_start: None,
                    auth_state: None,
                    fail_delete: false,
                    sessions_created: Arc::new(AtomicUsize::new(0)),
                    shutdowns: Arc::new(AtomicUsize::new(0)),
                    refresh_busy: Arc::new(AtomicUsize::new(0)),
                    log: Arc::default(),
                }
            }
        }

        impl AdapterFactory for FakeProbeFactory {
            fn create(
                &self,
                definition: AgentDefinition,
                generation: u64,
            ) -> Arc<dyn HarnessAdapter> {
                self.created.fetch_add(1, Ordering::SeqCst);
                self.log
                    .lock()
                    .unwrap()
                    .arguments
                    .push(definition.launch.arguments.clone());
                let mut status = AdapterStatus::stopped();
                status.generation = generation;
                if let Some(auth) = self.auth_state {
                    status.auth_state = auth;
                }
                status.models = self.models.clone();
                status.default_model_id = self.models.first().map(|m| m.model_id.clone());
                // A real adapter settles the selects (possibly to "none") on
                // the same session/new that advertises the models.
                status.session_selects = Some(AgentSessionSelects::default());
                status.capabilities.agent_advertises = fake_advertised();
                let (status_tx, _) = watch::channel(status);
                let updates_tx = HarnessUpdateSender::default();
                Arc::new(FakeProbeAdapter {
                    definition,
                    status_tx,
                    updates_tx,
                    fail_start: self.fail_start,
                    hold_start: self.hold_start.clone(),
                    fail_delete: self.fail_delete,
                    sessions_created: self.sessions_created.clone(),
                    shutdowns: self.shutdowns.clone(),
                    refresh_busy: self.refresh_busy.clone(),
                    log: self.log.clone(),
                })
            }
        }

        /// What the fake adapter's `initialize` "said"; the probe must carry
        /// it into the catalog unchanged.
        fn fake_advertised() -> BTreeMap<AcpFeature, bool> {
            [
                (AcpFeature::LoadSession, true),
                (AcpFeature::SessionFork, false),
            ]
            .into_iter()
            .collect()
        }

        pub(super) fn definition() -> AgentDefinition {
            AgentDefinition {
                launch: AgentLaunchSpec {
                    command: "npx".to_string(),
                    arguments: vec!["-y".to_string(), "adapter@1.0.0".to_string()],
                    environment: Vec::new(),
                    minimum_node_major: 20,
                },
                adapter: Some(crate::agent::AdapterPackage {
                    name: "adapter".to_string(),
                    version: "1.0.0".to_string(),
                    source: crate::agent::AdapterSource::Npm,
                }),
                ..AgentDefinition::for_test("claude")
            }
        }

        fn registry(factory: Arc<FakeProbeFactory>) -> Arc<AgentRegistry> {
            Arc::new(AgentRegistry::with_components(
                vec![definition()],
                Arc::new(AlwaysReadyProbe),
                factory as Arc<dyn AdapterFactory>,
            ))
        }

        #[tokio::test]
        async fn additional_instances_probe_lazily_and_one_per_harness() {
            let gate = tokio_util::sync::CancellationToken::new();
            let mut factory = FakeProbeFactory::with_models(vec![option("sonnet")]);
            factory.hold_start = Some(gate.clone());
            factory.auth_state = Some(HarnessAuthState::Ready);
            let factory = Arc::new(factory);
            let registry = registry(factory.clone());
            let ids: Vec<_> = (0..3)
                .map(|n| AgentId::new(format!("claude.profile{n}")))
                .collect();
            registry.load_instances(
                ids.iter()
                    .map(|id| crate::agent::AgentInstance {
                        id: id.clone(),
                        harness_id: crate::agent::HarnessId("claude".into()),
                        label: id.0.clone(),
                        home: crate::agent::HomeLayout::Managed,
                        claimed: false,
                    })
                    .collect(),
            );
            let catalog = Arc::new(ModelCatalogService::load(None));
            catalog.spawn_probes(&registry);
            tokio::task::yield_now().await;
            assert_eq!(
                factory.created.load(Ordering::SeqCst),
                0,
                "listing launches no adapters"
            );
            let mut tasks = tokio::task::JoinSet::new();
            for id in ids {
                let registry = registry.clone();
                let catalog = catalog.clone();
                tasks.spawn(async move { catalog.probe_now(&registry, &id).await });
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while factory.created.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                factory.created.load(Ordering::SeqCst),
                1,
                "siblings serialize through cleanup"
            );
            assert_eq!(catalog.probe_slots.available_permits(), 1);
            gate.cancel();
            while let Some(result) = tasks.join_next().await {
                result.unwrap().unwrap();
            }
            assert_eq!(factory.shutdowns.load(Ordering::SeqCst), 3);
            assert_eq!(catalog.probe_slots.available_permits(), 2);
        }

        fn registry_with_cleanup(
            factory: Arc<FakeProbeFactory>,
            cleanup: ProbeCleanup,
        ) -> Arc<AgentRegistry> {
            Arc::new(AgentRegistry::with_components(
                vec![AgentDefinition {
                    catalog_probe: cleanup,
                    ..definition()
                }],
                Arc::new(AlwaysReadyProbe),
                factory as Arc<dyn AdapterFactory>,
            ))
        }

        fn publish_existing(registry: &AgentRegistry, home: &std::path::Path) -> AgentId {
            let id = AgentId::new("claude.existing");
            registry.publish_instance(crate::agent::AgentInstance {
                id: id.clone(),
                harness_id: crate::agent::HarnessId("claude".into()),
                label: "CLI Claude".into(),
                home: crate::agent::HomeLayout::Existing {
                    path: home.into(),
                    environment: BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), None)]),
                },
                claimed: false,
            });
            id
        }

        /// A CLI setup is probed like an Ottam profile: a throwaway adapter
        /// opens the session, so the shared one never holds a synthetic one.
        #[tokio::test]
        async fn a_cli_setup_is_probed_through_a_throwaway_adapter() {
            let home = tempfile::tempdir().unwrap();
            let mut factory = FakeProbeFactory::with_models(vec![option("sonnet")]);
            factory.auth_state = Some(HarnessAuthState::Ready);
            let factory = Arc::new(factory);
            let registry = registry(factory.clone());
            let id = publish_existing(&registry, home.path());
            let result = catalog_probe::probe(&registry, &id).await.unwrap();
            assert_eq!(result.models, vec![option("sonnet")]);
            assert_eq!(factory.sessions_created.load(Ordering::SeqCst), 1);
            // A home with a refresh lock tears down in the background.
            tokio::time::timeout(Duration::from_secs(2), async {
                while factory.shutdowns.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the throwaway adapter stops");
            assert_eq!(
                factory.created.load(Ordering::SeqCst),
                1,
                "only the throwaway adapter starts, never the shared one"
            );
        }

        /// Stopping a process mid-refresh can strand the user's credentials,
        /// so the throwaway adapter outlives a held lock; the probe still
        /// settles, so an install waiting on it never stalls.
        #[tokio::test]
        async fn a_held_refresh_lock_defers_teardown_but_not_the_probe() {
            let home = tempfile::tempdir().unwrap();
            let lock = home
                .path()
                .join(crate::agent::refresh_lock::CLAUDE_REFRESH_LOCK);
            std::fs::create_dir(&lock).unwrap();
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            let registry = registry(factory.clone());
            let id = publish_existing(&registry, home.path());
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                catalog_probe::probe(&registry, &id),
            )
            .await
            .expect("the probe settles while the lock is held")
            .unwrap();
            assert_eq!(result.models, vec![option("sonnet")]);
            tokio::time::sleep(Duration::from_millis(250)).await;
            assert_eq!(factory.shutdowns.load(Ordering::SeqCst), 0);
            std::fs::remove_dir(&lock).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while factory.shutdowns.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the adapter stops once the refresh lets go");
        }

        /// A refresh can take the lock between the teardown's wait and its
        /// shutdown, which a shared home refuses; the teardown tries again.
        #[tokio::test]
        async fn a_refused_shutdown_is_retried_after_the_refresh() {
            let home = tempfile::tempdir().unwrap();
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            factory.refresh_busy.store(2, Ordering::SeqCst);
            let registry = registry(factory.clone());
            let id = publish_existing(&registry, home.path());
            catalog_probe::probe(&registry, &id).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while factory.shutdowns.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the adapter stops once shutdown is accepted");
            assert_eq!(factory.refresh_busy.load(Ordering::SeqCst), 0);
        }

        /// One process at a time waits behind a CLI home's lock.
        #[tokio::test]
        async fn a_pending_teardown_refuses_another_probe() {
            let home = tempfile::tempdir().unwrap();
            let lock = home
                .path()
                .join(crate::agent::refresh_lock::CLAUDE_REFRESH_LOCK);
            std::fs::create_dir(&lock).unwrap();
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            let registry = registry(factory.clone());
            let id = publish_existing(&registry, home.path());
            catalog_probe::probe(&registry, &id).await.unwrap();
            let refused = catalog_probe::probe(&registry, &id).await.err().expect("refused");
            assert!(refused.message.contains("still stopping"), "{}", refused.message);
            assert_eq!(factory.created.load(Ordering::SeqCst), 1);
            std::fs::remove_dir(&lock).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while registry.probe_teardown_pending(&id) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the teardown settles once the refresh lets go");
            catalog_probe::probe(&registry, &id).await.unwrap();
            assert_eq!(factory.created.load(Ordering::SeqCst), 2);
        }

        #[tokio::test]
        async fn delete_session_cleanup_deletes_the_probe_session() {
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("big")]));
            let registry = registry_with_cleanup(factory.clone(), ProbeCleanup::DeleteSession);
            catalog_probe::probe(&registry, &AgentId::new("claude"))
                .await
                .unwrap();
            assert_eq!(factory.log.lock().unwrap().deleted, vec!["probe-session"]);
            assert_eq!(factory.shutdowns.load(Ordering::SeqCst), 1);
        }

        /// A broken cleanup is a warning, not an empty model list.
        #[tokio::test]
        async fn a_refused_delete_still_records_the_models() {
            let mut factory = FakeProbeFactory::with_models(vec![option("big")]);
            factory.fail_delete = true;
            let factory = Arc::new(factory);
            let registry = registry_with_cleanup(factory.clone(), ProbeCleanup::DeleteSession);
            let catalog = Arc::new(ModelCatalogService::load(None));
            catalog
                .probe_now(&registry, &AgentId::new("claude"))
                .await
                .unwrap();
            assert_eq!(
                catalog.models_for(&AgentId::new("claude")),
                vec![option("big")]
            );
            assert_eq!(factory.shutdowns.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn session_meta_cleanup_reaches_session_new() {
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("big")]));
            let registry = registry_with_cleanup(
                factory.clone(),
                ProbeCleanup::SessionMeta(|| {
                    serde_json::Map::from_iter([(
                        "vendor".to_string(),
                        serde_json::json!({"persist": false}),
                    )])
                }),
            );
            catalog_probe::probe(&registry, &AgentId::new("claude"))
                .await
                .unwrap();
            let log = factory.log.lock().unwrap();
            assert_eq!(
                log.metas,
                vec![Some(
                    serde_json::json!({"vendor": {"persist": false}})
                        .as_object()
                        .unwrap()
                        .clone()
                )]
            );
            assert!(log.deleted.is_empty());
        }

        #[tokio::test]
        async fn scratch_session_dir_cleanup_points_the_store_away_and_removes_it() {
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("big")]));
            let registry = registry_with_cleanup(
                factory.clone(),
                ProbeCleanup::ScratchSessionDir("--session-dir"),
            );
            catalog_probe::probe(&registry, &AgentId::new("claude"))
                .await
                .unwrap();
            let log = factory.log.lock().unwrap();
            let arguments = log.arguments.last().expect("one probe adapter");
            let flag = arguments
                .iter()
                .position(|argument| argument == "--session-dir")
                .expect("the probe passes its store");
            let store = PathBuf::from(&arguments[flag + 1]);
            assert!(store.is_absolute());
            assert!(!store.exists(), "the probe removes its session store");
            assert_eq!(log.metas, vec![None]);
        }

        #[tokio::test]
        async fn no_cleanup_sends_plain_session_new() {
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("big")]));
            let registry = registry(factory.clone());
            catalog_probe::probe(&registry, &AgentId::new("claude"))
                .await
                .unwrap();
            let log = factory.log.lock().unwrap();
            assert_eq!(log.metas, vec![None]);
            assert!(log.deleted.is_empty());
            assert!(!log
                .arguments
                .iter()
                .flatten()
                .any(|argument| argument == "--session-dir"));
        }

        /// Same registry, but with a managed adapter store rooted at `store` —
        /// which is what makes the pin's install state matter at all.
        fn registry_with_store(
            factory: Arc<FakeProbeFactory>,
            store: &std::path::Path,
        ) -> Arc<AgentRegistry> {
            let installs =
                crate::agent::adapter_install::ManagedAdapterInstalls::at(store.to_path_buf());
            Arc::new(
                AgentRegistry::with_components(
                    vec![definition()],
                    Arc::new(AlwaysReadyProbe),
                    factory as Arc<dyn AdapterFactory>,
                )
                .with_adapter_installs(installs),
            )
        }

        /// Publish the pinned version by hand, the way a finished install would.
        fn seed_install(store: &std::path::Path) {
            let package = definition().adapter.expect("the test pin");
            let version_dir = store.join("claude").join("adapter").join(&package.version);
            let entrypoint_relative = "node_modules/adapter/dist/acp.js";
            let entrypoint = version_dir.join(entrypoint_relative);
            std::fs::create_dir_all(entrypoint.parent().expect("parent")).expect("dirs");
            std::fs::write(&entrypoint, "// adapter").expect("entrypoint");
            std::fs::write(
                version_dir.join("manifest.json"),
                serde_json::json!({
                    "name": package.name,
                    "version": package.version,
                    "resolved": null,
                    "integrity": null,
                    "entrypoint": entrypoint_relative,
                    "nodeVersion": null,
                    "installedAtUnix": 1,
                })
                .to_string(),
            )
            .expect("manifest");
        }

        /// The consent rule: with the pin not installed, no probe may spawn an
        /// adapter — the launch spec would fall back to `npx`, fetching and
        /// running the package before the user pressed Get.
        #[tokio::test]
        async fn an_uninstalled_adapter_is_never_probed() {
            let store = tempfile::tempdir().expect("tempdir");
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            let registry = registry_with_store(Arc::clone(&factory), store.path());
            let catalog = Arc::new(ModelCatalogService::load(None));

            let error = catalog
                .probe_now(&registry, &AgentId::new("claude"))
                .await
                .expect_err("the probe is refused");

            assert_eq!(error.kind, HarnessErrorKind::AdapterNotInstalled);
            assert_eq!(factory.created.load(Ordering::SeqCst), 0);
        }

        /// The background sweep skips it too, and without stamping an attempt —
        /// so the probe runs promptly once the install lands rather than
        /// waiting out the retry backoff.
        #[tokio::test]
        async fn the_background_sweep_skips_an_uninstalled_adapter() {
            let store = tempfile::tempdir().expect("tempdir");
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            let registry = registry_with_store(Arc::clone(&factory), store.path());
            let catalog = Arc::new(ModelCatalogService::load(None));

            catalog.spawn_probes(&registry);
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }

            assert_eq!(factory.created.load(Ordering::SeqCst), 0);
            assert!(catalog
                .last_attempt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty());
        }

        /// And the guard is not over-broad: once the pin is published, the same
        /// registry probes normally.
        #[tokio::test]
        async fn an_installed_adapter_still_probes() {
            let store = tempfile::tempdir().expect("tempdir");
            seed_install(store.path());
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            let registry = registry_with_store(Arc::clone(&factory), store.path());
            let catalog = Arc::new(ModelCatalogService::load(None));
            let agent_id = AgentId::new("claude");

            catalog
                .probe_now(&registry, &agent_id)
                .await
                .expect("the probe succeeds");

            assert_eq!(catalog.models_for(&agent_id), vec![option("sonnet")]);
        }

        #[tokio::test]
        async fn probe_now_records_the_catalog_and_settles_ok() {
            let factory = Arc::new(FakeProbeFactory::with_models(vec![
                option("sonnet"),
                option("haiku"),
            ]));
            let registry = registry(Arc::clone(&factory));
            let catalog = Arc::new(ModelCatalogService::load(None));
            let agent_id = AgentId::new("claude");

            catalog
                .probe_now(&registry, &agent_id)
                .await
                .expect("the probe succeeds");

            assert_eq!(
                catalog.models_for(&agent_id),
                vec![option("sonnet"), option("haiku")]
            );
            // The adapter's advertised set rides the same probe into the
            // catalog, so a cold launch can answer without starting it.
            assert_eq!(
                catalog.advertised_features_for(&agent_id, Some("adapter@1.0.0")),
                Some(fake_advertised())
            );
            // The entry is stamped against the definition's pin, so needs_probe
            // agrees the catalog is fresh.
            assert_eq!(
                catalog.adapter_version_for(&agent_id),
                Some("adapter@1.0.0".to_string())
            );
            assert!(!catalog.needs_probe(&agent_id, Some("adapter@1.0.0")));
        }

        #[tokio::test]
        async fn a_delayed_stale_probe_cannot_overwrite_a_new_live_session() {
            let gate = CancellationToken::new();
            let mut factory = FakeProbeFactory::with_models(vec![option("old")]);
            factory.hold_start = Some(gate.clone());
            let factory = Arc::new(factory);
            let registry = registry(Arc::clone(&factory));
            let catalog = Arc::new(ModelCatalogService::load(None));
            let definition = definition();
            let agent_id = definition.agent_id.clone();
            let probe = tokio::spawn({
                let catalog = Arc::clone(&catalog);
                let registry = Arc::clone(&registry);
                let agent_id = agent_id.clone();
                async move { catalog.probe_now(&registry, &agent_id).await }
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                while factory.created.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("probe has started and is held behind the gate");
            let live = live_status(vec![option("current")]);
            catalog.record_live(&definition, &live);
            // The adapter stops before the stale answer lands: there is no
            // subsequent engine.list/live record available to repair it.
            let mut stopped = live.clone();
            stopped.lifecycle = super::super::super::types::AdapterLifecycleState::Stopped;
            catalog.record_live(&definition, &stopped);
            gate.cancel();
            probe.await.unwrap().expect("probe still succeeds");

            assert_eq!(
                catalog.catalog_for(&agent_id),
                (live.models, live.default_model_id)
            );
            assert_eq!(catalog.session_selects_for(&agent_id), no_selects());
            assert_eq!(
                catalog.advertised_features_for(&agent_id, pin().as_deref()),
                Some(fake_advertised())
            );
            assert!(catalog.in_flight.lock().unwrap().is_empty());
        }

        #[tokio::test]
        async fn probe_now_ignores_the_retry_backoff() {
            let factory = Arc::new(FakeProbeFactory::with_models(vec![option("sonnet")]));
            let registry = registry(Arc::clone(&factory));
            let catalog = Arc::new(ModelCatalogService::load(None));
            let agent_id = AgentId::new("claude");

            // A background probe just failed as far as the backoff knows; an
            // explicit request probes anyway — the caller asked.
            catalog.mark_attempt(&agent_id);
            assert!(catalog.recently_attempted(&agent_id));

            catalog
                .probe_now(&registry, &agent_id)
                .await
                .expect("the probe runs despite the backoff");
            assert_eq!(factory.created.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn a_failed_probe_settles_err_and_keeps_the_cached_catalog() {
            let mut factory = FakeProbeFactory::with_models(vec![option("sonnet")]);
            factory.fail_start = true;
            let registry = registry(Arc::new(factory));
            let catalog = Arc::new(ModelCatalogService::load(None));
            let agent_id = AgentId::new("claude");
            catalog.record(
                &agent_id,
                vec![option("cached")],
                Some("cached".to_string()),
                Some(true),
                pin(),
                no_selects(),
            );

            let error = catalog
                .probe_now(&registry, &agent_id)
                .await
                .expect_err("the start failure surfaces");
            assert_eq!(error.kind, HarnessErrorKind::Internal);
            assert_eq!(catalog.models_for(&agent_id), vec![option("cached")]);
        }

        #[tokio::test]
        async fn a_second_caller_joins_the_in_flight_probe() {
            let gate = CancellationToken::new();
            let mut factory = FakeProbeFactory::with_models(vec![option("sonnet")]);
            factory.hold_start = Some(gate.clone());
            let factory = Arc::new(factory);
            let registry = registry(Arc::clone(&factory));
            let catalog = Arc::new(ModelCatalogService::load(None));
            let agent_id = AgentId::new("claude");

            let first = tokio::spawn({
                let catalog = Arc::clone(&catalog);
                let registry = Arc::clone(&registry);
                let agent_id = agent_id.clone();
                async move { catalog.probe_now(&registry, &agent_id).await }
            });
            // Wait until the first probe owns the slot before joining it.
            while !catalog
                .in_flight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&agent_id.0)
            {
                tokio::task::yield_now().await;
            }
            let second = tokio::spawn({
                let catalog = Arc::clone(&catalog);
                let registry = Arc::clone(&registry);
                let agent_id = agent_id.clone();
                async move { catalog.probe_now(&registry, &agent_id).await }
            });
            // Let the second caller reach its waiter before the slot frees —
            // on this current-thread runtime, yielding runs it to that point.
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
            gate.cancel();

            first.await.expect("join").expect("first probe succeeds");
            second.await.expect("join").expect("joined probe succeeds");
            // One adapter spawn served both callers.
            assert_eq!(factory.created.load(Ordering::SeqCst), 1);
            assert_eq!(catalog.models_for(&agent_id), vec![option("sonnet")]);
        }
    }
}
