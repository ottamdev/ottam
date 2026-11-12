use super::AgentRuntimeBridge;
use crate::agent::{setup_detection, AgentId, HomeLayout};
use crate::server::{
    outbound::ConnectionId,
    request_params::{decode_params, InvalidParamsLog},
    responses::{error_response, success_response},
    AgentServer,
};
use crate::storage::{repositories::AgentInstanceRepository, Database};
use ottam_protocol::*;
use serde_json::Value;
use std::sync::Arc;

impl AgentServer {
    pub(in crate::server) fn handle_agent_instance_request(
        &mut self,
        origin: ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let Some(bridge) = self.agent_bridge.clone() else {
            return Some(error_response(
                request.id,
                -32000,
                "Agent runtime is unavailable",
            ));
        };
        // Cancellation must remain responsive when shutdown occupies a regular
        // request slot or holds the adapter map while waiting on a refresh.
        if request.method == METHOD_AGENT_CANCEL_SHUTDOWN {
            let params = match decode_params::<AgentShutdownOperationParams>(
                &request,
                InvalidParamsLog::Silent,
            ) {
                Ok(params) => params,
                Err(response) => return Some(response),
            };
            bridge
                .registry()
                .cancel_shutdown_operation(&params.operation_key);
            return Some(success_response(request.id, AgentEmptyResult {}));
        }
        let database = self.database.clone();
        let task_bridge = bridge.clone();
        let request_id = request.id.clone();
        bridge.spawn_response(origin, request_id, async move {
            match request.method.as_str() {
                METHOD_AGENT_PREPARE_SHUTDOWN => {
                    let params = match decode_params::<AgentShutdownOperationParams>(
                        &request,
                        InvalidParamsLog::Silent,
                    ) {
                        Ok(params) => params,
                        Err(response) => return response,
                    };
                    match task_bridge
                        .registry()
                        .prepare_shutdown(&params.operation_key)
                        .await
                    {
                        Ok(()) => success_response(request.id, AgentEmptyResult {}),
                        Err(error) => super::wire::harness_error_response(request.id, error),
                    }
                }
                METHOD_AGENT_SHUTDOWN_READINESS => {
                    if let Err(response) =
                        decode_params::<AgentListParams>(&request, InvalidParamsLog::Silent)
                    {
                        return response;
                    }
                    let waiting_on = task_bridge.registry().shutdown_waiting_on();
                    success_response(
                        request.id,
                        AgentShutdownReadinessResult {
                            has_shared_homes: Some(
                                task_bridge
                                    .registry()
                                    .definitions()
                                    .iter()
                                    .any(|definition| definition.refresh_lock_path().is_some()),
                            ),
                            ready: waiting_on.is_empty(),
                            waiting_on,
                            refresh_lock_paths: task_bridge
                                .registry()
                                .active_refresh_lock_paths()
                                .into_iter()
                                .map(|path| path.to_string_lossy().into_owned())
                                .collect(),
                        },
                    )
                }
                METHOD_AGENT_DETECT_SETUPS => {
                    if let Err(response) =
                        decode_params::<AgentListParams>(&request, InvalidParamsLog::Silent)
                    {
                        return response;
                    }
                    let detected = tokio::task::spawn_blocking(|| {
                        ["codex", "claude", "opencode", "omp"]
                            .into_iter()
                            .map(setup_detection::summary)
                            .collect::<Vec<_>>()
                    })
                    .await;
                    match detected {
                        Ok(setups) => {
                            success_response(request.id, AgentDetectSetupsResult { setups })
                        }
                        Err(_) => error_response(request.id, -32000, "Setup detection failed"),
                    }
                }
                METHOD_AGENT_CREATE => {
                    let params = match decode_params::<AgentCreateParams>(
                        &request,
                        InvalidParamsLog::Silent,
                    ) {
                        Ok(params) => params,
                        Err(response) => return response,
                    };
                    if params.layout == AgentHomeKind::Managed {
                        if crate::agent::existing_setup(&params.harness_id).is_none() {
                            return error_response(
                                request.id,
                                -32602,
                                "Unsupported profile provider",
                            );
                        }
                        let Some(label) = params.label else {
                            return error_response(
                                request.id,
                                -32602,
                                "A profile name is required",
                            );
                        };
                        let registry = task_bridge.registry();
                        let _changes = registry.instance_changes.lock().await;
                        let result = database
                            .run("agent.create.managed", move |connection| {
                                AgentInstanceRepository::create_managed(
                                    connection,
                                    &params.harness_id,
                                    &label,
                                )
                            })
                            .await;
                        return match result {
                            Ok(instance) => {
                                let id = instance.id.clone();
                                registry.publish_instance(instance);
                                if let Err(error) = registry.prepare_home(&id) {
                                    let failed = id.0.clone();
                                    let _ = database
                                        .run("agent.create.rollback", move |connection| {
                                            AgentInstanceRepository::delete(connection, &failed)
                                        })
                                        .await;
                                    registry.remove_instance(&id);
                                    return super::wire::harness_error_response(request.id, error);
                                }
                                super::services::notify_engine_changed(&task_bridge.outbound, &id);
                                success_response(
                                    request.id,
                                    AgentInstanceResult {
                                        agent_id: AgentID(id.0),
                                    },
                                )
                            }
                            Err(error) => crate::server::responses::core_error_response(
                                request.id,
                                -32000,
                                error.into(),
                            ),
                        };
                    }
                    change_layout(
                        database,
                        task_bridge,
                        request.id,
                        None,
                        params.harness_id,
                        params.layout,
                    )
                    .await
                }
                METHOD_AGENT_RENAME => {
                    let params = match decode_params::<AgentRenameParams>(
                        &request,
                        InvalidParamsLog::Silent,
                    ) {
                        Ok(params) => params,
                        Err(response) => return response,
                    };
                    let registry = task_bridge.registry();
                    let _admission = registry
                        .instance_admission(&AgentId(params.agent_id.0.clone()))
                        .lock_owned()
                        .await;
                    let _changes = registry.instance_changes.lock().await;
                    match database
                        .run("agent.rename", move |connection| {
                            AgentInstanceRepository::rename(
                                connection,
                                &params.agent_id.0,
                                &params.label,
                            )
                        })
                        .await
                    {
                        Ok(instance) => {
                            let id = instance.id.clone();
                            registry.publish_instance(instance);
                            super::services::notify_engine_changed(&task_bridge.outbound, &id);
                            success_response(
                                request.id,
                                AgentInstanceResult {
                                    agent_id: AgentID(id.0),
                                },
                            )
                        }
                        Err(error) => crate::server::responses::core_error_response(
                            request.id,
                            -32000,
                            error.into(),
                        ),
                    }
                }
                METHOD_AGENT_DELETE => {
                    let params = match decode_params::<AgentDeleteParams>(
                        &request,
                        InvalidParamsLog::Silent,
                    ) {
                        Ok(params) => params,
                        Err(response) => return response,
                    };
                    delete_instance(database, task_bridge, request.id, params).await
                }
                METHOD_AGENT_SET_LAYOUT => {
                    let params = match decode_params::<AgentSetLayoutParams>(
                        &request,
                        InvalidParamsLog::Silent,
                    ) {
                        Ok(params) => params,
                        Err(response) => return response,
                    };
                    let definition = match task_bridge
                        .registry()
                        .definition(&AgentId(params.agent_id.0.clone()))
                    {
                        Ok(definition) => definition,
                        Err(error) => {
                            return super::wire::harness_error_response(request.id, error)
                        }
                    };
                    change_layout(
                        database,
                        task_bridge,
                        request.id,
                        Some(params.agent_id),
                        definition.harness_id().into(),
                        params.layout,
                    )
                    .await
                }
                _ => error_response(request.id, -32601, "Unknown instance method"),
            }
        });
        None
    }
}

async fn delete_instance(
    database: Database,
    bridge: Arc<AgentRuntimeBridge>,
    request_id: RequestID,
    params: AgentDeleteParams,
) -> Value {
    let registry = bridge.registry();
    let id = AgentId(params.agent_id.0.clone());
    let definition = match registry.definition(&id) {
        Ok(definition) => definition,
        Err(error) => return super::wire::harness_error_response(request_id, error),
    };
    if params.purge && definition.existing_home().is_some() {
        return error_response(
            request_id,
            -32602,
            "Your CLI home cannot be removed by Ottam.",
        );
    }
    if bridge.agent_install_service().is_installing(&id) {
        return error_response(
            request_id,
            -32000,
            "Wait for this provider's installation to finish.",
        );
    }
    let operation = match registry.begin_shutdown_operation(&params.operation_key) {
        Ok(operation) => operation,
        Err(error) => return super::wire::harness_error_response(request_id, error),
    };
    let catalog = bridge.model_catalog();
    let result = async {
        let installs = bridge.agent_install_service();
        let _installation = installs.guard_profile_delete(&id)?;
        let signin = bridge.sign_in_service()?;
        let _signin = tokio::select! {
            _ = operation.cancel.cancelled() => return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "Profile deletion cancelled")),
            guard = signin.guard_removal(&params.agent_id) => guard?,
        };
        let fence = catalog.fence_delete(&id);
        tokio::select! {
            _ = operation.cancel.cancelled() => return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "Profile deletion cancelled")),
            _ = fence.wait() => {}
        }
        let _admission = tokio::select! {
            _ = operation.cancel.cancelled() => return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "Profile deletion cancelled")),
            admission = registry.instance_admission(&id).lock_owned() => admission,
        };
        // Layout may have changed while deletion waited for sign-in/admission.
        // Revalidate under the gate before considering a filesystem mutation.
        let definition = registry.definition(&id)?;
        if params.purge && definition.existing_home().is_some() {
            return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, "Your CLI home cannot be removed by Ottam."));
        }
        if registry.instance_in_use(&id).await || bridge.conversation.agent_has_live_work(&id.0) {
            return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, "Stop this profile's active, background and child work before deleting it."));
        }
        let check_id = id.0.clone();
        database.run("agent.delete.check", move |connection| AgentInstanceRepository::check_idle(connection, &check_id)).await.map_err(|error| crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, error.to_string()))?;
        let refresh_lock = definition.refresh_lock_path();
        loop {
            tokio::select! {
                _ = operation.cancel.cancelled() => return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "Profile deletion cancelled")),
                _ = crate::agent::refresh_lock::wait_for_release(refresh_lock.as_deref()) => {}
            }
            let stopped = tokio::select! {
                _ = operation.cancel.cancelled() => return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "Profile deletion cancelled")),
                result = registry.delete_adapters(&id) => result,
            };
            match stopped { Err(error) if error.kind == crate::agent::HarnessErrorKind::AuthRefreshBusy => continue, result => { result?; break; } }
        }
        // Membership changes and purge validation share a short global boundary.
        // Waiting for probes, replay, or refresh above fences only this instance.
        let _changes = tokio::select! {
            _ = operation.cancel.cancelled() => return Err(crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::RequestCancelled, "Profile deletion cancelled")),
            changes = registry.instance_changes.lock() => changes,
        };
        let home = if params.purge {
            let retained = database.run("agent.delete.cli_homes", AgentInstanceRepository::retained_cli_homes).await
                .map_err(|error| crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, error.to_string()))?;
            registry.purge_home(&id, &retained)?
        } else { None };
        let deleted = id.0.clone();
        let cancel = operation.cancel.clone();
        database.run("agent.delete", move |connection| {
            // Recheck on the database worker: cancellation can arrive while
            // this commit is queued after refresh-safe adapter teardown.
            if cancel.is_cancelled() {
                return Err(crate::storage::StorageError::new("requestCancelled", "Profile deletion cancelled"));
            }
            AgentInstanceRepository::delete(connection, &deleted)
        }).await.map_err(|error| crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, error.to_string()))?;
        registry.retire_instance(&id).await;
        catalog.forget_agent(&id);
        super::services::notify_engine_changed(&bridge.outbound, &id);
        if params.purge {
            if let Some(home) = home {
                tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(home) { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(error) }).await
                    .map_err(|_| crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, "Profile removed, but home cleanup did not finish."))?
                    .map_err(|_| crate::agent::HarnessError::new(crate::agent::HarnessErrorKind::Internal, "Profile removed, but its home could not be removed."))?;
            }
        }
        Ok::<(), crate::agent::HarnessError>(())
    }.await;
    match result {
        Ok(()) => success_response(request_id, AgentEmptyResult {}),
        Err(error) => super::wire::harness_error_response(request_id, error),
    }
}

async fn change_layout(
    database: Database,
    bridge: Arc<AgentRuntimeBridge>,
    request_id: RequestID,
    agent_id: Option<AgentID>,
    harness: String,
    layout: AgentHomeKind,
) -> Value {
    let Some(title) = crate::agent::existing_setup(&harness).map(|setup| setup.title) else {
        return error_response(request_id, -32602, "Unsupported setup harness");
    };
    let home = if layout == AgentHomeKind::Existing {
        let harness = harness.clone();
        match tokio::task::spawn_blocking(move || setup_detection::detect(&harness)).await {
            Ok(Ok(setup)) if setup.summary.available => setup.layout,
            Ok(Ok(_)) => {
                return error_response(
                    request_id,
                    -32000,
                    "Your CLI setup is missing or unreadable.",
                )
            }
            Ok(Err(error)) => return super::wire::harness_error_response(request_id, error),
            Err(_) => return error_response(request_id, -32000, "Setup detection failed"),
        }
    } else {
        HomeLayout::Managed
    };
    let registry = bridge.registry();
    let _sign_in = if let Some(id) = &agent_id {
        let sign_in = match bridge.sign_in_service() {
            Ok(service) => service,
            Err(error) => return super::wire::harness_error_response(request_id, error),
        };
        Some(match sign_in.guard_removal(id).await {
            Ok(guard) => guard,
            Err(error) => return super::wire::harness_error_response(request_id, error),
        })
    } else {
        None
    };
    let _admission = if let Some(id) = &agent_id {
        Some(
            registry
                .instance_admission(&AgentId(id.0.clone()))
                .lock_owned()
                .await,
        )
    } else {
        None
    };
    let _change = registry.instance_changes.lock().await;
    if let Err(error) = registry.validate_attachment(&home) {
        return super::wire::harness_error_response(request_id, error);
    }
    if let Some(id) = &agent_id {
        let check_id = id.0.clone();
        let eligible = database
            .run("agent.layout.check", move |connection| {
                Ok(AgentInstanceRepository::list(connection)?
                    .into_iter()
                    .any(|instance| instance.id.0 == check_id && !instance.claimed))
            })
            .await;
        let eligible = match eligible {
            Ok(value) => value,
            Err(error) => return error_response(request_id, -32000, error.to_string()),
        };
        if !eligible {
            return error_response(
                request_id,
                -32000,
                if layout == AgentHomeKind::Managed {
                    "This CLI instance already has conversations and must keep its home. Use Create another profile on the provider page."
                } else {
                    "This instance already has conversations. Add your CLI setup instead."
                },
            );
        }
        if let Err(error) = registry.shutdown(&AgentId(id.0.clone())).await {
            return super::wire::harness_error_response(request_id, error);
        }
    }
    let result = database
        .run("agent.layout.change", move |connection| {
            if let Some(id) = agent_id {
                AgentInstanceRepository::set_layout(connection, &id.0, &home)
            } else {
                AgentInstanceRepository::add_existing(
                    connection,
                    &harness,
                    &format!("{title} — Your setup"),
                    &home,
                )
            }
        })
        .await;
    match result {
        Ok(instance) => {
            let id = instance.id.clone();
            registry.publish_instance(instance);
            registry.forget_probed_auth(&id);
            bridge.model_catalog().forget_agent(&id);
            super::services::notify_engine_changed(&bridge.outbound, &id);
            success_response(
                request_id,
                AgentInstanceResult {
                    agent_id: AgentID(id.0),
                },
            )
        }
        Err(error) => {
            crate::server::responses::core_error_response(request_id, -32000, error.into())
        }
    }
}
