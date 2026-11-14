use super::services::directory_size;
use super::wire::{adapter_slug, harness_error_response, map_adapter_status, map_definition};
use crate::agent::{pinned_adapter_package, AgentId};
use crate::server::outbound::ConnectionId;
use crate::server::request_params::{decode_params, InvalidParamsLog};
use crate::server::responses::{core_error_response, error_response, success_response};
use crate::server::AgentServer;
use ottam_protocol::{
    AgentAuthStateParams, AgentAuthStateResult, AgentEmptyResult, AgentHomeInfoParams,
    AgentHomeInfoResult, AgentInstallCancelParams, AgentInstallParams, AgentInstallResult,
    AgentListParams, AgentListResult, AgentPlanUsageParams, AgentPlanUsageResult,
    AgentSignInCancelParams, AgentSignInFinishParams, AgentSignInParams, AgentUninstallParams,
    ProtocolRequest, METHOD_AGENT_AUTH_STATE, METHOD_AGENT_HOME_INFO, METHOD_AGENT_INSTALL,
    METHOD_AGENT_INSTALL_CANCEL, METHOD_AGENT_LIST, METHOD_AGENT_PLAN_USAGE, METHOD_AGENT_SIGN_IN,
    METHOD_AGENT_SIGN_IN_CANCEL, METHOD_AGENT_SIGN_IN_FINISH, METHOD_AGENT_UNINSTALL,
};
use serde_json::Value;
use std::sync::Arc;

impl AgentServer {
    pub(in crate::server) fn handle_agent_request(
        &mut self,
        origin: ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        if matches!(
            request.method.as_str(),
            ottam_protocol::METHOD_AGENT_DETECT_SETUPS
                | ottam_protocol::METHOD_AGENT_CREATE
                | ottam_protocol::METHOD_AGENT_RENAME
                | ottam_protocol::METHOD_AGENT_DELETE
                | ottam_protocol::METHOD_AGENT_SET_LAYOUT
                | ottam_protocol::METHOD_AGENT_SHUTDOWN_READINESS
                | ottam_protocol::METHOD_AGENT_PREPARE_SHUTDOWN
                | ottam_protocol::METHOD_AGENT_CANCEL_SHUTDOWN
        ) {
            return self.handle_agent_instance_request(origin, request);
        }
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return Some(error_response(
                request.id,
                -32000,
                "agent runtime is unavailable",
            ));
        };
        match request.method.as_str() {
            METHOD_AGENT_LIST => {
                let params =
                    match decode_params::<AgentListParams>(&request, InvalidParamsLog::Silent) {
                        Ok(params) => params,
                        Err(response) => return Some(response),
                    };
                let request_id = request.id;
                let task_bridge = Arc::clone(&bridge);
                let database = self.database.clone();
                bridge.spawn_response(origin, request_id.clone(), async move {
                    let registry = task_bridge.registry();
                    if params.refresh_runtime.unwrap_or(false) {
                        registry.invalidate_runtime_cache();
                    }
                    let model_catalog = task_bridge.model_catalog();
                    let probed = match task_bridge.runtime_probes().probe_all().await {
                        Ok(probed) => probed,
                        Err(error) => return core_error_response(request_id, -32000, error),
                    };
                    let mut agents = Vec::with_capacity(probed.len());
                    let runtime_update = task_bridge.managed_runtime_update();
                    for (definition, agent_id, runtime, adapter) in probed {
                        let cached_auth_methods = model_catalog.auth_methods_for(&agent_id);
                        let cached_advertised = model_catalog.advertised_features_for(
                            &agent_id,
                            pinned_adapter_package(&definition).as_deref(),
                        );
                        let cached_models = model_catalog.models_for(&agent_id);
                        let install_state = task_bridge.adapter_install_state(&definition);
                        let mut summary = map_definition(
                            definition.clone(),
                            runtime,
                            adapter,
                            cached_auth_methods,
                            cached_advertised,
                            &cached_models,
                            install_state,
                        );
                        task_bridge.annotate_upgrades(
                            &definition,
                            &mut summary,
                            runtime_update.as_ref(),
                        );
                        agents.push(summary);
                    }
                    let instances = match database
                        .run(
                            "agent.list.instances",
                            crate::storage::repositories::AgentInstanceRepository::list,
                        )
                        .await
                    {
                        Ok(instances) => instances,
                        Err(error) => return error_response(request_id, -32000, error.to_string()),
                    };
                    for agent in &mut agents {
                        agent.can_set_layout = instances
                            .iter()
                            .find(|instance| instance.id.0 == agent.agent_id.0)
                            .map(|instance| !instance.claimed);
                    }
                    success_response(request_id, AgentListResult { agents })
                });
                None
            }
            // Start (or join) an adapter install. Returns as soon as the work
            // is accepted; `agent.installEvent` carries everything after that.
            METHOD_AGENT_INSTALL => {
                let params =
                    match decode_params::<AgentInstallParams>(&request, InvalidParamsLog::Silent) {
                        Ok(params) => params,
                        Err(response) => return Some(response),
                    };
                let agent_id = AgentId(params.agent_id.0);
                Some(match bridge.agent_install_service().start(&agent_id) {
                    Ok(accepted) => success_response(
                        request.id,
                        AgentInstallResult {
                            install_id: accepted.install_id,
                            agent_id: accepted.agent_id,
                            name: accepted.name,
                            version: accepted.version,
                            phase: accepted.phase,
                        },
                    ),
                    Err(error) => harness_error_response(request.id, error),
                })
            }
            // The last-known snapshot; a stale one refreshes in the
            // background and arrives on `agent.planUsageChanged`.
            METHOD_AGENT_PLAN_USAGE => {
                let params =
                    match decode_params::<AgentPlanUsageParams>(&request, InvalidParamsLog::Silent)
                    {
                        Ok(params) => params,
                        Err(response) => return Some(response),
                    };
                let force = params.refresh == Some(true);
                let usage = bridge
                    .plan_usage()
                    .snapshot(&AgentId(params.agent_id.0), force);
                Some(success_response(request.id, AgentPlanUsageResult { usage }))
            }
            METHOD_AGENT_INSTALL_CANCEL => {
                let params = match decode_params::<AgentInstallCancelParams>(
                    &request,
                    InvalidParamsLog::Silent,
                ) {
                    Ok(params) => params,
                    Err(response) => return Some(response),
                };
                // A stale id is deliberately not an error: cancelling something
                // that already finished is a race the client cannot avoid.
                bridge.agent_install_service().cancel(&params.install_id);
                Some(success_response(request.id, AgentEmptyResult {}))
            }
            METHOD_AGENT_UNINSTALL => {
                let params =
                    match decode_params::<AgentUninstallParams>(&request, InvalidParamsLog::Silent)
                    {
                        Ok(params) => params,
                        Err(response) => return Some(response),
                    };
                let request_id = request.id;
                let task_bridge = Arc::clone(&bridge);
                bridge.spawn_response(
                    origin,
                    request_id.clone(),
                    async move {
                        let sign_in = match task_bridge.sign_in_service() {
                            Ok(service) => service,
                            Err(error) => return harness_error_response(request_id, error),
                        };
                        // Hold the sign-in creation boundary until removal is
                        // complete. A live flow is refused, and a concurrent
                        // new one cannot start against files being deleted.
                        let definition = match task_bridge
                            .registry()
                            .definition(&AgentId(params.agent_id.0.clone()))
                        {
                            Ok(definition) => definition,
                            Err(error) => return harness_error_response(request_id, error),
                        };
                        let mut sign_in_guards = Vec::new();
                        for instance in task_bridge
                            .registry()
                            .harness_instances(definition.harness_id())
                        {
                            match sign_in.guard_removal(&ottam_protocol::AgentID(instance.0)).await {
                                Ok(guard) => sign_in_guards.push(guard),
                                Err(error) => return harness_error_response(request_id, error),
                            }
                        }
                        let agent_id = AgentId(params.agent_id.0);
                        let registry = task_bridge.registry();
                        let operation = match params.operation_key.as_deref().map(|key| registry.begin_shutdown_operation(key)).transpose() {
                            Ok(operation) => operation,
                            Err(error) => return harness_error_response(request_id, error),
                        };
                        let install_service = task_bridge.agent_install_service();
                        let remove = install_service.uninstall(&agent_id, params.remove_home.unwrap_or(false));
                        let result = if let Some(operation) = &operation {
                            tokio::select! {
                                biased;
                                _ = operation.cancel.cancelled() => Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "adapter removal cancelled")),
                                result = remove => result,
                            }
                        } else { remove.await };
                        match result
                        {
                            Ok(()) => {
                                super::services::notify_engine_changed(
                                    &task_bridge.outbound,
                                    &agent_id,
                                );
                                success_response(request_id, AgentEmptyResult {})
                            }
                            Err(error) => harness_error_response(request_id, error),
                        }
                    },
                );
                None
            }
            METHOD_AGENT_HOME_INFO => {
                let params = match decode_params::<AgentHomeInfoParams>(
                    &request,
                    InvalidParamsLog::Silent,
                ) {
                    Ok(params) => params,
                    Err(response) => return Some(response),
                };
                let agent_id = AgentId::new(&params.agent_id.0);
                let Some(home) = bridge.registry().agent_home(&agent_id) else {
                    return Some(error_response(
                        request.id,
                        -32000,
                        format!("{} has no managed home", params.agent_id.0),
                    ));
                };
                let request_id = request.id;
                bridge.spawn_response(origin, request_id.clone(), async move {
                    let path = home.to_string_lossy().into_owned();
                    // The walk hits the filesystem hard on a big home, so
                    // it stays off the async runtime's worker threads.
                    let size_bytes = tokio::task::spawn_blocking(move || directory_size(&home))
                        .await
                        .unwrap_or(0);
                    success_response(request_id, AgentHomeInfoResult { path, size_bytes })
                });
                None
            }
            METHOD_AGENT_AUTH_STATE => {
                let params =
                    match decode_params::<AgentAuthStateParams>(&request, InvalidParamsLog::Silent)
                    {
                        Ok(params) => params,
                        Err(response) => return Some(response),
                    };
                let request_id = request.id;
                let task_bridge = Arc::clone(&bridge);
                bridge.spawn_response(origin, request_id.clone(), async move {
                    let agent_id = AgentId(params.agent_id.0.clone());
                    if params.refresh.unwrap_or(false) {
                        let registry = task_bridge.registry();
                        if registry
                            .definition(&agent_id)
                            .is_ok_and(|definition| definition.existing_home().is_none())
                        {
                            if let Err(error) = task_bridge
                                .model_catalog()
                                .probe_now(&registry, &agent_id)
                                .await
                            {
                                return harness_error_response(request_id, error);
                            }
                        }
                        if registry
                            .definition(&agent_id)
                            .is_ok_and(|definition| definition.existing_home().is_some())
                        {
                            // Preserve both bound sessions and runs that
                            // acquired their adapter before session binding.
                            if let Err(error) = registry.recheck_existing_auth(&agent_id).await {
                                return harness_error_response(request_id, error);
                            }
                            registry.forget_probed_auth(&agent_id);
                            // A CLI setup that was signed in before Ottam saw
                            // it never signs in here, so this is where its
                            // catalog fills, and a sign-in in the CLI's own
                            // terminal can change what it offers, so freshness
                            // does not skip it. In the background, so a cold
                            // CLI start never holds up Recheck.
                            task_bridge
                                .model_catalog()
                                .reprobe_in_background(&registry, &agent_id);
                        }
                    }
                    let slug = task_bridge
                        .registry()
                        .definition(&agent_id)
                        .ok()
                        .and_then(|definition| adapter_slug(&definition));
                    match task_bridge.registry().cached_status(&agent_id).await {
                        Ok(status) => success_response(
                            request_id,
                            AgentAuthStateResult {
                                agent_id: params.agent_id,
                                adapter: map_adapter_status(status, slug.as_deref()),
                            },
                        ),
                        Err(error) => harness_error_response(request_id, error),
                    }
                });
                None
            }
            METHOD_AGENT_SIGN_IN => {
                let params =
                    match decode_params::<AgentSignInParams>(&request, InvalidParamsLog::Silent) {
                        Ok(params) => params,
                        Err(response) => return Some(response),
                    };
                let request_id = request.id;
                let task_bridge = Arc::clone(&bridge);
                bridge.spawn_response(origin, request_id.clone(), async move {
                    let service = match task_bridge.sign_in_service() {
                        Ok(service) => service,
                        Err(error) => return harness_error_response(request_id, error),
                    };
                    match service.sign_in(params).await {
                        Ok(result) => success_response(request_id, result),
                        Err(error) => harness_error_response(request_id, error),
                    }
                });
                None
            }
            METHOD_AGENT_SIGN_IN_CANCEL => {
                let params = match decode_params::<AgentSignInCancelParams>(
                    &request,
                    InvalidParamsLog::Silent,
                ) {
                    Ok(params) => params,
                    Err(response) => return Some(response),
                };
                // Cancelling is a local abandonment, so it answers immediately
                // rather than waiting for the flow it is walking away from.
                match bridge.sign_in_service() {
                    Ok(service) => {
                        service.cancel(&params.agent_id, &params.auth_session_id);
                        Some(success_response(request.id, AgentEmptyResult {}))
                    }
                    Err(error) => Some(harness_error_response(request.id, error)),
                }
            }
            METHOD_AGENT_SIGN_IN_FINISH => {
                let params = match decode_params::<AgentSignInFinishParams>(
                    &request,
                    InvalidParamsLog::Silent,
                ) {
                    Ok(params) => params,
                    Err(response) => return Some(response),
                };
                // Answers immediately, like cancel: the close/restart outcome
                // arrives on `agent.signInEvent`, same as every other ending.
                match bridge.sign_in_service() {
                    Ok(service) => {
                        service.finish(&params.agent_id, &params.auth_session_id);
                        Some(success_response(request.id, AgentEmptyResult {}))
                    }
                    Err(error) => Some(harness_error_response(request.id, error)),
                }
            }
            _ => Some(error_response(request.id, -32601, "method not found")),
        }
    }
}
