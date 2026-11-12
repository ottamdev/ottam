use super::wire::core_error_from_harness;
use crate::agent::InstallOutcome;
use crate::agent::{
    AdapterStatus, AgentDefinition, AgentId, AgentInstallEventSink, AgentInstallService,
    AgentRegistry, AgentRuntimeStatus as HarnessRuntimeStatus, AgentSignInService,
    CatalogChangeSink, HarnessError, HarnessErrorKind, ModelCatalogService, PlanUsageService,
    PlanUsageSink, RuntimeInstallEventSink, RuntimeInstallService,
};
use crate::core_error::CoreError;
use crate::server::outbound::OutboundSink;
use ottam_protocol::{
    AgentAdapterInstallState, AgentInstallEventParams, AgentRuntimeSource, AgentSummary,
    AgentUpdateState, AgentUpdateStatus, EngineChangedParams, EngineID, ProtocolNotification,
    RuntimeInstallEventParams, METHOD_AGENT_INSTALL_EVENT, METHOD_AGENT_PLAN_USAGE_CHANGED,
    METHOD_ENGINE_CHANGED, METHOD_RUNTIME_INSTALL_EVENT,
};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

fn update_status(
    version: &str,
    installing: bool,
    outcome: Option<InstallOutcome>,
) -> AgentUpdateStatus {
    let (state, error) = match (installing, outcome) {
        (true, _) => (AgentUpdateState::Updating, None),
        (false, Some(InstallOutcome::Failed(error))) => (AgentUpdateState::Failed, Some(error)),
        (false, Some(InstallOutcome::Cancelled) | None) => (AgentUpdateState::Pending, None),
    };
    AgentUpdateStatus {
        version: version.to_string(),
        state,
        error,
    }
}

/// Invalidation hint shared by catalog updates and completed lifecycle changes.
pub(super) fn notify_engine_changed(outbound: &OutboundSink, agent_id: &AgentId) {
    let notification = ProtocolNotification::new(
        METHOD_ENGINE_CHANGED,
        EngineChangedParams {
            engine_id: EngineID(agent_id.0.clone()),
        },
    );
    if let Ok(value) = serde_json::to_value(notification) {
        let _ = outbound.notify(value);
    }
}

/// The plan-usage store, announcing each new snapshot on
/// `agent.planUsageChanged`.
pub(super) fn plan_usage_service(
    registry: Arc<AgentRegistry>,
    outbound: &OutboundSink,
) -> Arc<PlanUsageService> {
    let outbound = outbound.clone();
    let sink: PlanUsageSink = Arc::new(move |change| {
        let notification = ProtocolNotification::new(METHOD_AGENT_PLAN_USAGE_CHANGED, change);
        if let Ok(value) = serde_json::to_value(notification) {
            let _ = outbound.notify(value);
        }
    });
    PlanUsageService::new(registry, sink)
}

pub(super) struct AgentServices {
    registry: Arc<AgentRegistry>,
    outbound: OutboundSink,
    model_catalog: Arc<ModelCatalogService>,
    runtime_probes: RuntimeProbeSweep,
    sign_in: OnceLock<Arc<AgentSignInService>>,
    runtime_install: OnceLock<Arc<RuntimeInstallService>>,
    agent_install: OnceLock<Arc<AgentInstallService>>,
}

impl AgentServices {
    pub(super) fn new(
        registry: Arc<AgentRegistry>,
        outbound: OutboundSink,
        model_catalog: Arc<ModelCatalogService>,
    ) -> Self {
        model_catalog.restore_probed_auth(&registry);
        // A background probe landing after a client has already listed engines
        // is the ordinary case, not the edge one: the cache answers the list
        // immediately and the probe rewrites it seconds later. Pushing the
        // change is what stops that client from holding a stale model list —
        // or a stale "no image support" — until it happens to re-list.
        let change_outbound = outbound.clone();
        let changes: CatalogChangeSink = Arc::new(move |agent_id: &AgentId| {
            notify_engine_changed(&change_outbound, agent_id);
        });
        registry.set_auth_change_sink(Arc::clone(&changes));
        let runtime_probes = RuntimeProbeSweep::new(
            Arc::clone(&registry),
            RUNTIME_PROBE_BUDGET,
            Arc::clone(&changes),
        );
        model_catalog.set_change_sink(changes);
        // The catalog doesn't hold the registry, so this closes no cycle.
        let catalog = Arc::clone(&model_catalog);
        registry.set_session_catalog_sink(Arc::new(
            move |definition: &AgentDefinition, status: &AdapterStatus| {
                catalog.record_session_change(definition, status);
            },
        ));
        Self {
            registry,
            outbound,
            model_catalog,
            runtime_probes,
            sign_in: OnceLock::new(),
            runtime_install: OnceLock::new(),
            agent_install: OnceLock::new(),
        }
    }

    pub(super) fn registry(&self) -> Arc<AgentRegistry> {
        Arc::clone(&self.registry)
    }

    /// Cancel every adapter and runtime install in flight. Services that were
    /// never built have nothing to cancel.
    pub(super) fn cancel_installs(&self) {
        if let Some(service) = self.agent_install.get() {
            service.cancel_all();
        }
        if let Some(service) = self.runtime_install.get() {
            service.cancel_all();
        }
    }

    pub(super) fn install_sign_in_service(&self, service: Arc<AgentSignInService>) {
        let _ = self.sign_in.set(service);
    }

    /// The managed-runtime installer, built on demand.
    pub(super) fn runtime_install_service(
        &self,
    ) -> Result<Arc<RuntimeInstallService>, HarnessError> {
        if let Some(service) = self.runtime_install.get() {
            return Ok(Arc::clone(service));
        }
        let outbound = self.outbound.clone();
        let registry = Arc::clone(&self.registry);
        let events: RuntimeInstallEventSink = Arc::new(move |params: RuntimeInstallEventParams| {
            // Every ending changes what `agent.list` reports — the runtime,
            // or a background upgrade's failed/pending state.
            if matches!(
                params.phase,
                ottam_protocol::RuntimeInstallPhase::Ready
                    | ottam_protocol::RuntimeInstallPhase::Failed
                    | ottam_protocol::RuntimeInstallPhase::Cancelled
            ) {
                for definition in registry.definitions() {
                    notify_engine_changed(&outbound, &definition.agent_id);
                }
            }
            let notification = ProtocolNotification::new(METHOD_RUNTIME_INSTALL_EVENT, params);
            if let Ok(value) = serde_json::to_value(notification) {
                let _ = outbound.notify(value);
            }
        });
        let service = Arc::new(RuntimeInstallService::new(
            Arc::clone(&self.registry),
            events,
        )?);
        // A concurrent caller may have won the race; either instance is fine,
        // but everyone must share one so "already installing" stays true.
        let _ = self.runtime_install.set(Arc::clone(&service));
        Ok(Arc::clone(self.runtime_install.get().unwrap_or(&service)))
    }

    /// The adapter installer, built on demand.
    pub(super) fn agent_install_service(&self) -> Arc<AgentInstallService> {
        Arc::clone(self.agent_install.get_or_init(|| {
            let outbound = self.outbound.clone();
            let events: AgentInstallEventSink = Arc::new(move |params: AgentInstallEventParams| {
                if matches!(
                    params.phase,
                    ottam_protocol::AgentInstallPhase::Ready
                        | ottam_protocol::AgentInstallPhase::Failed
                        | ottam_protocol::AgentInstallPhase::Cancelled
                ) {
                    notify_engine_changed(&outbound, &AgentId::new(&params.agent_id.0));
                }
                let notification = ProtocolNotification::new(METHOD_AGENT_INSTALL_EVENT, params);
                if let Ok(value) = serde_json::to_value(notification) {
                    let _ = outbound.notify(value);
                }
            });
            let service = Arc::new(AgentInstallService::new(
                Arc::clone(&self.registry),
                Arc::clone(&self.model_catalog),
                events,
            ));
            let installing = Arc::downgrade(&service);
            self.registry
                .set_install_in_flight_guard(Arc::new(move |agent: &AgentId| {
                    installing
                        .upgrade()
                        .is_some_and(|service| service.is_installing(agent))
                }));
            service
        }))
    }

    /// The wire install state for one agent's adapter pin.
    pub(super) fn adapter_install_state(
        &self,
        definition: &AgentDefinition,
    ) -> AgentAdapterInstallState {
        if !self.registry.adapter_needs_install(definition) {
            // Includes registries without a store (opted out, tests): they
            // launch on demand, which the wire reports as installed.
            return AgentAdapterInstallState::Installed;
        }
        if self
            .agent_install
            .get()
            .is_some_and(|service| service.is_installing(&definition.agent_id))
        {
            AgentAdapterInstallState::Installing
        } else {
            AgentAdapterInstallState::NotInstalled
        }
    }

    /// Bring pins an app update moved up to date in the background: the
    /// managed Node runtime and every adapter with an older install on disk.
    /// The older versions keep launching until each new one lands. Called
    /// when the app connects, which is "at launch" for the bundled core.
    pub(super) fn reconcile_upgrades(&self) {
        match self.runtime_install_service() {
            Ok(service) => service.reconcile(),
            Err(error) => tracing::warn!(
                error = %error.message,
                "cannot check the managed Node runtime for an upgrade"
            ),
        }
        self.agent_install_service().spawn_auto_reinstalls();
    }

    /// The run-start half of the retry policy: a background upgrade that
    /// failed (offline at launch, say) tries again when the user next runs
    /// the agent. Never blocks the run, which starts on the installed version.
    pub(super) fn retry_failed_upgrades(&self, agent_id: &AgentId) {
        if let Some(service) = self.runtime_install.get() {
            service.retry_failed_upgrade();
        }
        self.agent_install_service().retry_failed_update(agent_id);
    }

    /// Fill in what `agent.list` says about pins an app update moved: the
    /// adapter version that launches now and how its replacement is going,
    /// and `runtime_update` (see [`Self::managed_runtime_update`]) when this
    /// agent runs on the managed runtime.
    pub(super) fn annotate_upgrades(
        &self,
        definition: &AgentDefinition,
        summary: &mut AgentSummary,
        runtime_update: Option<&AgentUpdateStatus>,
    ) {
        if let Some(package) = summary.adapter_package.as_mut() {
            package.installed_version = self.registry.launched_adapter_version(definition);
            if package
                .installed_version
                .as_ref()
                .is_some_and(|installed| installed != &package.version)
            {
                let install = self.agent_install.get();
                let installing =
                    install.is_some_and(|service| service.is_installing(&definition.agent_id));
                let outcome = install.and_then(|service| service.outcome(&definition.agent_id));
                package.update = Some(update_status(&package.version, installing, outcome));
            }
        }
        if summary.runtime.source == Some(AgentRuntimeSource::Managed) {
            summary.runtime.managed_update = runtime_update.cloned();
        }
    }

    /// How an app update's managed Node pin is going, while an older managed
    /// runtime stands in for it. Read once per `agent.list`, not per agent.
    pub(super) fn managed_runtime_update(&self) -> Option<AgentUpdateStatus> {
        let managed = self.registry.managed_node()?;
        managed.superseded_version()?;
        let install = self.runtime_install.get();
        let installing = install.is_some_and(|service| service.is_installing());
        let outcome = install.and_then(|service| service.outcome());
        Some(update_status(managed.version(), installing, outcome))
    }

    pub(super) fn sign_in_service(&self) -> Result<Arc<AgentSignInService>, HarnessError> {
        self.sign_in.get().cloned().ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                "sign-in is unavailable: the terminal layer is not running",
            )
        })
    }

    pub(super) fn model_catalog(&self) -> Arc<ModelCatalogService> {
        Arc::clone(&self.model_catalog)
    }

    pub(super) fn runtime_probes(&self) -> RuntimeProbeSweep {
        self.runtime_probes.clone()
    }

    /// Kick off background model-catalog probes for agents whose cached
    /// catalog is missing or stale. Fire-and-forget.
    pub(super) fn spawn_model_catalog_probes(&self) {
        self.model_catalog.spawn_probes(&self.registry);
    }
}

/// Each agent's runtime probe pays a real subprocess (worst case an
/// interactive login shell), so both list handlers share one concurrent,
/// budgeted sweep. agent.list originally probed in a serial loop with no
/// timeout at all — one hung shell probe hung the whole request.
const RUNTIME_PROBE_BUDGET: Duration = Duration::from_secs(3);

/// A listed agent. The runtime is `None` while its probe is still running.
pub(in crate::server) type ProbedAgent = (
    AgentDefinition,
    AgentId,
    Option<HarnessRuntimeStatus>,
    AdapterStatus,
);

type RuntimeProbeTask = tokio::task::JoinHandle<Result<HarnessRuntimeStatus, HarnessError>>;

/// The sweep behind both list handlers. A probe that outruns the budget costs
/// its own row a verdict, not the whole list, and keeps running: the blocking
/// work cannot be cancelled, and its answer warms the caches for the re-read
/// that `on_settled` prompts.
#[derive(Clone)]
pub(in crate::server) struct RuntimeProbeSweep {
    registry: Arc<AgentRegistry>,
    budget: Duration,
    on_settled: CatalogChangeSink,
    /// Harnesses with an announcement owed, so polls during one slow probe
    /// produce one `engine.changed`.
    settling: Arc<StdMutex<BTreeSet<String>>>,
}

impl RuntimeProbeSweep {
    fn new(registry: Arc<AgentRegistry>, budget: Duration, on_settled: CatalogChangeSink) -> Self {
        Self {
            registry,
            budget,
            on_settled,
            settling: Arc::default(),
        }
    }

    pub(in crate::server) async fn probe_all(&self) -> Result<Vec<ProbedAgent>, CoreError> {
        let definitions = self.registry.definitions();
        let count = definitions.len();
        let mut harnesses = std::collections::BTreeMap::<String, Vec<_>>::new();
        for (index, definition) in definitions.into_iter().enumerate() {
            harnesses
                .entry(definition.harness_id().to_owned())
                .or_default()
                .push((index, definition));
        }
        let mut probes = tokio::task::JoinSet::new();
        for (harness, definitions) in harnesses {
            let sweep = self.clone();
            probes.spawn(async move {
                let agent_id = definitions[0].1.agent_id.clone();
                let runtime = sweep.probe_within_budget(harness, agent_id).await?;
                let mut rows = Vec::with_capacity(definitions.len());
                for (index, definition) in definitions {
                    let agent_id = definition.agent_id.clone();
                    let adapter = sweep
                        .registry
                        .cached_status(&agent_id)
                        .await
                        .map_err(core_error_from_harness)?;
                    rows.push((index, (definition, agent_id, runtime.clone(), adapter)));
                }
                Ok::<_, CoreError>(rows)
            });
        }
        // Collect by index so the response keeps stable definition order.
        let mut probed: Vec<Option<_>> = (0..count).map(|_| None).collect();
        while let Some(joined) = probes.join_next().await {
            for (index, probe) in joined.map_err(probe_task_failed)?? {
                probed[index] = Some(probe);
            }
        }
        Ok(probed.into_iter().flatten().collect())
    }

    async fn probe_within_budget(
        &self,
        harness: String,
        agent_id: AgentId,
    ) -> Result<Option<HarnessRuntimeStatus>, CoreError> {
        let registry = Arc::clone(&self.registry);
        let probed_agent = agent_id.clone();
        let mut probe = tokio::spawn(async move { registry.runtime_status(&probed_agent).await });
        match tokio::time::timeout(self.budget, &mut probe).await {
            Ok(joined) => joined
                .map_err(probe_task_failed)?
                .map(Some)
                .map_err(core_error_from_harness),
            Err(_) => {
                tracing::info!(agent_id = %agent_id.0, "runtime probe outran the list budget");
                self.announce_when_settled(harness, agent_id, probe);
                Ok(None)
            }
        }
    }

    fn announce_when_settled(&self, harness: String, agent_id: AgentId, probe: RuntimeProbeTask) {
        let inserted = self
            .settling
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(harness.clone());
        if !inserted {
            return;
        }
        let settling = Arc::clone(&self.settling);
        let on_settled = Arc::clone(&self.on_settled);
        tokio::spawn(async move {
            let _ = probe.await;
            // Cleared first: a re-read that still finds the probe slow must be
            // able to owe another announcement.
            settling
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&harness);
            on_settled(&agent_id);
        });
    }
}

fn probe_task_failed(error: tokio::task::JoinError) -> CoreError {
    CoreError::new(
        "runtimeProbeFailed",
        format!("agent probe task failed: {error}"),
    )
    .recoverable(true)
}

/// Rough recursive size — file lengths only, unreadable entries skipped.
/// Good enough for a Settings row; not a `du` replacement.
pub(super) fn directory_size(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let Ok(file_type) = entry.file_type() else {
                return 0;
            };
            if file_type.is_dir() {
                directory_size(&entry.path())
            } else if file_type.is_file() {
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod runtime_probe_tests {
    use super::*;
    use crate::agent::{AgentInstance, HarnessId, HomeLayout, RuntimeProbe};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[tokio::test]
    async fn listing_probes_each_harness_once_for_multiple_profiles() {
        #[derive(Default)]
        struct CountingProbe(Mutex<BTreeMap<String, usize>>);
        impl RuntimeProbe for CountingProbe {
            fn probe(&self, definition: &AgentDefinition) -> HarnessRuntimeStatus {
                *self
                    .0
                    .lock()
                    .unwrap()
                    .entry(definition.harness_id().into())
                    .or_default() += 1;
                HarnessRuntimeStatus::ready("v22.0.0", "10.0.0")
            }
        }
        let probe = Arc::new(CountingProbe::default());
        let registry = Arc::new(AgentRegistry::with_runtime_probe(probe.clone()));
        for harness in ["codex", "claude"] {
            for index in 0..8 {
                registry.publish_instance(AgentInstance {
                    id: AgentId::new(format!("{harness}.profile-{index}")),
                    harness_id: HarnessId(harness.into()),
                    label: format!("Profile {index}"),
                    home: HomeLayout::Managed,
                    claimed: true,
                });
            }
        }
        let expected = registry.definitions();
        let rows =
            RuntimeProbeSweep::new(registry, RUNTIME_PROBE_BUDGET, Arc::new(|_: &AgentId| {}))
                .probe_all()
                .await
                .unwrap();
        assert_eq!(rows.len(), expected.len());
        for (row, definition) in rows.iter().zip(expected) {
            assert_eq!(row.1, definition.agent_id);
            assert_eq!(row.3.generation, 0);
        }
        let counts = probe.0.lock().unwrap();
        assert_eq!(counts.get("codex"), Some(&1));
        assert_eq!(counts.get("claude"), Some(&1));
    }

    /// Codex's probe parks until released; every other harness answers.
    #[derive(Default)]
    struct ParkedCodexProbe {
        released: Mutex<bool>,
        changed: std::sync::Condvar,
    }

    impl ParkedCodexProbe {
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }

    impl RuntimeProbe for ParkedCodexProbe {
        fn probe(&self, definition: &AgentDefinition) -> HarnessRuntimeStatus {
            if definition.harness_id() == "codex" {
                let mut released = self.released.lock().unwrap();
                while !*released {
                    released = self.changed.wait(released).unwrap();
                }
            }
            HarnessRuntimeStatus::ready("v22.0.0", "10.0.0")
        }
    }

    /// One slow harness loses its own verdict and nothing else, and overlapping
    /// lists owe the client a single announcement when it lands.
    #[tokio::test]
    async fn a_slow_probe_is_checking_and_announced_once() {
        let probe = Arc::new(ParkedCodexProbe::default());
        let registry = Arc::new(AgentRegistry::with_runtime_probe(probe.clone()));
        let (announced_tx, mut announced_rx) = tokio::sync::mpsc::unbounded_channel();
        // Long enough that the harnesses which answer always make it; only
        // the parked probe ever spends the whole budget.
        let sweep = RuntimeProbeSweep::new(
            registry.clone(),
            Duration::from_secs(2),
            Arc::new(move |agent_id: &AgentId| {
                announced_tx.send(agent_id.clone()).unwrap();
            }),
        );

        let (first, second) = tokio::join!(sweep.probe_all(), sweep.probe_all());
        for rows in [first.unwrap(), second.unwrap()] {
            assert_eq!(rows.len(), registry.definitions().len());
            for (definition, _, runtime, _) in &rows {
                assert_eq!(
                    runtime.is_none(),
                    definition.harness_id() == "codex",
                    "{}",
                    definition.agent_id.0
                );
            }
        }
        assert!(
            announced_rx.try_recv().is_err(),
            "nothing to announce while the probe still runs"
        );

        probe.release();
        assert_eq!(announced_rx.recv().await.unwrap().0, "codex");
        assert!(sweep
            .probe_all()
            .await
            .unwrap()
            .iter()
            .all(|row| row.2.is_some()));
        drop(sweep);
        assert!(
            announced_rx.recv().await.is_none(),
            "overlapping lists announce once"
        );
    }
}
