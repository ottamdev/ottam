use super::client_link::ClientLink;
use super::deferred::{DeferredLane, DeferredResponseTasks};
use super::outbound::{ConnectionId, OutboundSink};
use crate::agent::{
    AgentDefinition, AgentId, AgentInstallService, AgentRegistry, AgentSignInService,
    AuthEventSink, AuthTerminalHost, HarnessAuthState, HarnessError, HarnessErrorKind,
    HarnessPermissionBroker, ModelCatalogService, RuntimeInstallService, SignedInSink,
};
use crate::core_error::CoreError;

use super::chat::ChatService;
use ottam_protocol::{
    AgentAdapterInstallState, AgentSignInEventParams, AgentSummary, AgentUpdateStatus,
    ConversationCancelRunResult, ConversationDeleteResult, ConversationID, ConversationRunID,
    ConversationRunInputID, ConversationSetModeParams, ConversationSetModeResult,
    ConversationStartRunParams, ConversationStartRunResult, ConversationSteerRunParams,
    ConversationSteerRunResult, ConversationStopBackgroundTaskParams,
    ConversationStopBackgroundTaskResult, ProjectID, ProtocolNotification, RequestID,
    RunInputStatus, WorkspaceID, METHOD_AGENT_SIGN_IN_EVENT,
};
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

mod attribution;
mod conversation_recorder;
mod conversation_runner;
mod events;
mod instance_requests;
mod native_subagents;
mod permission;
mod requests;
mod services;
mod task_index;
mod wire;
use attribution::SessionAttribution;
use conversation_runner::ConversationRunCoordinator;
pub(super) use conversation_runner::DelegationHost;
#[cfg(test)]
use events::forward_harness_update;
use events::{AgentUpdateForwarders, ConversationUpdateSink};
use permission::ServerPermissionBroker;
use services::AgentServices;
#[cfg(test)]
use wire::adapter_package;
pub(super) use wire::{harness_error_response, map_runtime_status, map_steering_capability};
const MAX_DEFERRED_AGENT_REQUESTS: usize = 64;

pub(super) struct AgentRuntimeBridge {
    outbound: OutboundSink,
    permission_broker: Arc<ServerPermissionBroker>,
    forwarders: Arc<AgentUpdateForwarders>,
    response_tasks: StdMutex<Option<DeferredResponseTasks>>,
    regular_lane: DeferredLane,
    conversation: Arc<ConversationRunCoordinator>,
    services: AgentServices,
}

impl AgentRuntimeBridge {
    pub(super) fn new(outbound: OutboundSink, client_link: ClientLink) -> Arc<Self> {
        Self::new_with_limits(
            outbound,
            client_link,
            MAX_DEFERRED_AGENT_REQUESTS,
            Arc::new(ModelCatalogService::load_default()),
        )
    }

    fn new_with_limits(
        outbound: OutboundSink,
        client_link: ClientLink,
        regular_limit: usize,
        model_catalog: Arc<ModelCatalogService>,
    ) -> Arc<Self> {
        let attribution = SessionAttribution::default();
        let permission_broker = Arc::new(ServerPermissionBroker::new(
            client_link,
            attribution.clone(),
        ));
        let permission_control = permission_broker.clone();
        let broker: Arc<dyn HarnessPermissionBroker> = permission_broker.clone();
        let registry = Arc::new(AgentRegistry::built_in_with_permission_broker(broker));
        let regular_lane = DeferredLane::new(regular_limit);
        let conversation: Arc<ConversationRunCoordinator> = Arc::new_cyclic(
            |conversation: &std::sync::Weak<ConversationRunCoordinator>| {
                let update_sink: std::sync::Weak<dyn ConversationUpdateSink> = conversation.clone();
                let forwarders = Arc::new(AgentUpdateForwarders::new(
                    outbound.clone(),
                    update_sink,
                    attribution.clone(),
                ));
                ConversationRunCoordinator::new(
                    Arc::clone(&registry),
                    outbound.clone(),
                    regular_lane.clone(),
                    forwarders,
                    attribution,
                    permission_control,
                    conversation.clone(),
                )
            },
        );
        permission_broker.bind_follow_ups(&conversation);
        let replay_coordinator = Arc::downgrade(&conversation);
        registry.set_replay_live_work_guard(Arc::new(move |agent| {
            replay_coordinator
                .upgrade()
                .is_none_or(|coordinator| coordinator.agent_has_live_work(&agent.0))
        }));
        let forwarders = conversation.forwarders();
        let services = AgentServices::new(Arc::clone(&registry), outbound.clone(), model_catalog);
        Arc::new(Self {
            outbound: outbound.clone(),
            permission_broker,
            forwarders,
            response_tasks: StdMutex::new(Some(DeferredResponseTasks::new(
                outbound.clone(),
                "agent response",
            ))),
            regular_lane,
            conversation,
            services,
        })
    }

    #[cfg(test)]
    fn new_with_registry(
        outbound: OutboundSink,
        client_link: ClientLink,
        registry: AgentRegistry,
    ) -> Arc<Self> {
        Self::new_with_registry_and_regular_limit(
            outbound,
            client_link,
            registry,
            MAX_DEFERRED_AGENT_REQUESTS,
        )
    }

    #[cfg(test)]
    fn new_with_registry_and_regular_limit(
        outbound: OutboundSink,
        client_link: ClientLink,
        registry: AgentRegistry,
        regular_limit: usize,
    ) -> Arc<Self> {
        let attribution = SessionAttribution::default();
        let permission_broker = Arc::new(ServerPermissionBroker::new(
            client_link,
            attribution.clone(),
        ));
        let permission_control = permission_broker.clone();
        let registry = Arc::new(registry);
        let regular_lane = DeferredLane::new(regular_limit);
        let conversation: Arc<ConversationRunCoordinator> = Arc::new_cyclic(
            |conversation: &std::sync::Weak<ConversationRunCoordinator>| {
                let update_sink: std::sync::Weak<dyn ConversationUpdateSink> = conversation.clone();
                let forwarders = Arc::new(AgentUpdateForwarders::new(
                    outbound.clone(),
                    update_sink,
                    attribution.clone(),
                ));
                ConversationRunCoordinator::new(
                    Arc::clone(&registry),
                    outbound.clone(),
                    regular_lane.clone(),
                    forwarders,
                    attribution,
                    permission_control,
                    conversation.clone(),
                )
            },
        );
        permission_broker.bind_follow_ups(&conversation);
        let replay_coordinator = Arc::downgrade(&conversation);
        registry.set_replay_live_work_guard(Arc::new(move |agent| {
            replay_coordinator
                .upgrade()
                .is_none_or(|coordinator| coordinator.agent_has_live_work(&agent.0))
        }));
        let forwarders = conversation.forwarders();
        let services = AgentServices::new(
            Arc::clone(&registry),
            outbound.clone(),
            Arc::new(ModelCatalogService::load(None)),
        );
        Arc::new(Self {
            outbound: outbound.clone(),
            permission_broker,
            forwarders,
            response_tasks: StdMutex::new(Some(DeferredResponseTasks::new(
                outbound.clone(),
                "agent response",
            ))),
            regular_lane,
            conversation,
            services,
        })
    }

    pub(super) fn registry(&self) -> Arc<AgentRegistry> {
        self.services.registry()
    }

    pub(super) fn runtime_probes(&self) -> services::RuntimeProbeSweep {
        self.services.runtime_probes()
    }

    pub(super) fn runtime_install_service(
        &self,
    ) -> Result<Arc<RuntimeInstallService>, HarnessError> {
        self.services.runtime_install_service()
    }

    pub(super) fn agent_install_service(&self) -> Arc<AgentInstallService> {
        self.services.agent_install_service()
    }

    pub(super) fn adapter_install_state(
        &self,
        definition: &AgentDefinition,
    ) -> AgentAdapterInstallState {
        self.services.adapter_install_state(definition)
    }

    /// See [`AgentServices::reconcile_upgrades`].
    pub(crate) fn reconcile_upgrades(&self) {
        self.services.reconcile_upgrades();
    }

    pub(super) fn annotate_upgrades(
        &self,
        definition: &AgentDefinition,
        summary: &mut AgentSummary,
        runtime_update: Option<&AgentUpdateStatus>,
    ) {
        self.services
            .annotate_upgrades(definition, summary, runtime_update);
    }

    pub(super) fn managed_runtime_update(&self) -> Option<AgentUpdateStatus> {
        self.services.managed_runtime_update()
    }

    fn sign_in_service(&self) -> Result<Arc<AgentSignInService>, HarnessError> {
        self.services.sign_in_service()
    }

    pub(super) fn model_catalog(&self) -> Arc<ModelCatalogService> {
        self.services.model_catalog()
    }

    pub(super) fn spawn_model_catalog_probes(&self) {
        self.services.spawn_model_catalog_probes();
    }

    pub(super) fn plan_usage(&self) -> &Arc<crate::agent::PlanUsageService> {
        self.conversation.plan_usage()
    }

    /// Give every session the run coordinator opens from now on the servers
    /// on for its workspace.
    pub(super) fn install_mcp(&self, service: crate::mcp::McpService) {
        self.conversation.install_mcp(service);
    }

    pub(super) fn install_delegation(&self, host: DelegationHost) {
        self.conversation
            .install_delegation(host, self.services.model_catalog());
    }

    /// What the proxy's `ottam` path answers with.
    pub(super) fn ottam_tools(&self) -> Arc<dyn crate::mcp::OttamTools> {
        self.conversation.ottam_tools()
    }

    /// Continue on a note card: deliver the conversation's waiting notes.
    pub(super) async fn continue_delegation(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
    ) -> Result<ottam_protocol::ConversationContinueDelegationResult, CoreError> {
        self.conversation
            .continue_delegation(chat, conversation_id)
            .await
    }

    /// Send to Parent in a child: its deferred reports wake its parent.
    pub(super) async fn send_to_parent(
        &self,
        chat: &ChatService,
        workspace_id: &ottam_protocol::WorkspaceID,
        conversation_id: &ConversationID,
    ) -> Result<ottam_protocol::ConversationSendToParentResult, CoreError> {
        self.conversation
            .send_to_parent(chat, workspace_id, conversation_id)
            .await
    }

    /// Where MCP servers' questions go: the same question panel as agents'.
    pub(super) fn question_broker(&self) -> Arc<dyn HarnessPermissionBroker> {
        self.permission_broker.clone()
    }

    /// Give the bridge the terminal capability a `clientTerminal` sign-in needs.
    /// Called once, when the terminal manager is built.
    pub(super) fn install_sign_in(&self, terminals: Arc<dyn AuthTerminalHost>) {
        let outbound = self.outbound.clone();
        let events: AuthEventSink = Arc::new(move |params: AgentSignInEventParams| {
            if params.phase == ottam_protocol::AgentAuthPhase::Succeeded {
                services::notify_engine_changed(&outbound, &AgentId::new(&params.agent_id.0));
            }
            let notification = ProtocolNotification::new(METHOD_AGENT_SIGN_IN_EVENT, params);
            if let Ok(value) = serde_json::to_value(notification) {
                let _ = outbound.notify(value);
            }
        });
        // A sign-in succeeding makes any auth-required probe failure stale, so
        // the catalog forgets its backoff and probes again right away — that
        // is what fills the model picker minutes instead of half an hour after
        // the user logs in.
        let catalog = self.model_catalog();
        let registry = self.registry();
        let signed_in: SignedInSink = Arc::new(move |agent_id| {
            let agent = crate::agent::AgentId(agent_id.0.clone());
            catalog.forget_attempt(&agent);
            catalog.forget_auth_state(&agent);
            catalog.forget_live_observations(&agent);
            registry.forget_probed_auth(&agent);
            catalog.spawn_probe(&registry, &agent);
        });
        let service = Arc::new(AgentSignInService::new(
            self.registry(),
            terminals,
            events,
            Some(signed_in),
        ));
        self.registry().set_replay_sign_in_service(&service);
        self.services.install_sign_in_service(service);
    }

    pub(super) fn set_client_supports_permissions(&self, supported: bool) {
        self.permission_broker.set_supported(supported);
    }

    pub(super) fn set_client_supports_questions(&self, supported: bool) {
        self.permission_broker.set_questions_supported(supported);
    }

    fn reap_finished_responses(&self) {
        if let Some(tasks) = self
            .response_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            tasks.reap_finished();
        }
    }

    pub(super) fn spawn_response<F>(&self, origin: ConnectionId, request_id: RequestID, future: F)
    where
        F: Future<Output = Value> + Send + 'static,
    {
        self.reap_finished_responses();
        let Ok(permit) = self.regular_lane.try_acquire() else {
            let error = HarnessError::new(
                HarnessErrorKind::Internal,
                "agent request queue is saturated",
            );
            let _ = self
                .outbound
                .respond(origin, harness_error_response(request_id, error));
            return;
        };
        if let Some(tasks) = self
            .response_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            tasks.spawn_admitted(origin, permit, future);
        }
    }

    #[cfg(test)]
    pub(super) async fn ensure_forwarder(
        self: &Arc<Self>,
        adapter: Arc<dyn crate::agent::HarnessAdapter>,
    ) {
        self.forwarders.ensure(adapter).await;
    }

    #[cfg(test)]
    async fn flush_forwarder(&self, agent_id: &AgentId) {
        self.forwarders.flush(agent_id).await;
    }

    /// Refuse a resend to an engine known to be signed out, before any row
    /// changes hands. Allowed through, the retry would supersede the failed
    /// input, start a run, and fail the same way a moment later — one more
    /// failed row and one more run per press. The failed input stays as it is
    /// and is re-offered once the user signs in, the same shape as the
    /// `runAlreadyActive` refusal.
    ///
    /// Only a `failed` input that targeted a run is checked: a `deliveryUnknown`
    /// retry is the user resolving an ambiguity, and the engine's sign-in state
    /// is not evidence either way about that. An engine with no adapter up is
    /// `unknown`, not signed out; only a login this generation saw lapse counts.
    pub(super) async fn refuse_resend_while_signed_out(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        run_input_id: &ConversationRunInputID,
    ) -> Result<(), CoreError> {
        let input = chat.run_input(conversation_id, run_input_id).await?;
        if input.status != RunInputStatus::Failed {
            return Ok(());
        }
        let Some(run_id) = input.target_run_id.as_ref() else {
            return Ok(());
        };
        let run = chat.conversation_run(conversation_id, run_id).await?;
        let agent_id = AgentId(run.engine_id.0.clone());
        let Ok(status) = self.registry().cached_status(&agent_id).await else {
            return Ok(());
        };
        if status.auth_state != HarnessAuthState::Missing {
            return Ok(());
        }
        let name = self
            .registry()
            .definition(&agent_id)
            .map(|definition| definition.presentation.product_name.to_string())
            .unwrap_or_else(|_| run.engine_id.0.clone());
        Err(CoreError::new(
            "authRequired",
            format!("{name} is signed out; sign in before sending this message again"),
        )
        .recoverable(true))
    }

    pub(super) async fn start_conversation_run(
        self: &Arc<Self>,
        params: ConversationStartRunParams,
        chat: ChatService,
        working_root: PathBuf,
        project_id: Option<ProjectID>,
    ) -> Result<ConversationStartRunResult, CoreError> {
        self.reap_finished_responses();
        self.services
            .retry_failed_upgrades(&AgentId::new(&params.engine_id.0));
        self.conversation
            .start_conversation_run(params, chat, working_root, project_id)
            .await
    }

    pub(super) async fn steer_conversation_run(
        self: &Arc<Self>,
        params: ConversationSteerRunParams,
        chat: ChatService,
        working_root: PathBuf,
        project_id: Option<ProjectID>,
    ) -> Result<ConversationSteerRunResult, CoreError> {
        self.reap_finished_responses();
        self.conversation
            .steer_conversation_run(params, chat, working_root, project_id)
            .await
    }

    pub(super) async fn stop_background_task(
        &self,
        params: &ConversationStopBackgroundTaskParams,
    ) -> Result<ConversationStopBackgroundTaskResult, HarnessError> {
        self.conversation
            .stop_background_task(
                &params.workspace_id,
                &params.conversation_id,
                &params.run_id,
                &params.async_task_id,
            )
            .await
    }

    pub(super) async fn set_conversation_mode(
        &self,
        chat: &ChatService,
        params: &ConversationSetModeParams,
    ) -> Result<ConversationSetModeResult, CoreError> {
        self.conversation.set_conversation_mode(chat, params).await
    }

    pub(super) async fn cancel_conversation_run(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
    ) -> Result<ConversationCancelRunResult, CoreError> {
        let result = self
            .conversation
            .cancel_conversation_run(chat, conversation_id, run_id)
            .await?;
        // Stopping a parent's turn stops the children it delegated to.
        self.conversation.stop_children(chat, conversation_id).await;
        Ok(result)
    }

    pub(super) async fn cancel_project_runs(
        &self,
        workspace_id: &WorkspaceID,
        project_id: &ProjectID,
    ) {
        self.conversation
            .cancel_project_runs(workspace_id, project_id)
            .await;
    }

    #[cfg(test)]
    async fn reserve_followup_launch(
        &self,
        chat: &ChatService,
        conversation_id: &ConversationID,
        input_id: &ottam_protocol::ConversationRunInputID,
        engine_id: &ottam_protocol::EngineID,
    ) -> Result<(tokio::sync::OwnedSemaphorePermit, AgentId), CoreError> {
        self.conversation
            .reserve_followup_launch(chat, conversation_id, input_id, engine_id)
            .await
    }

    /// Deletes a conversation, and its delegated children if `children`
    /// says so; kept ones stop working.
    pub(super) async fn delete_conversation_with_children(
        &self,
        chat: &ChatService,
        workspace_id: &WorkspaceID,
        conversation_id: &ConversationID,
        children: ottam_protocol::ConversationDeleteChildren,
    ) -> Result<ConversationDeleteResult, CoreError> {
        self.conversation
            .delete_with_children(chat, workspace_id, conversation_id, children)
            .await
    }

    pub(super) async fn shutdown(&self) {
        // First, so npm's process-group kill overlaps the rest of shutdown.
        self.services.cancel_installs();
        self.permission_broker.cancel_all();
        self.conversation.shutdown_runs().await;
        let response_tasks = self
            .response_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(response_tasks) = response_tasks {
            response_tasks.shutdown().await;
        }
        self.forwarders.shutdown().await;
        self.conversation.forget_sessions();
        if let Err(error) = self.registry().shutdown_all().await {
            tracing::warn!(error = %error, "agent registry shutdown failed");
        }
    }
}

#[cfg(test)]
mod tests;
