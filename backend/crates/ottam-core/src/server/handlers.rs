use super::engines::{engine_auth_method, harness_engine};
use super::request_params::{decode_params, InvalidParamsLog};
use super::responses::{core_error_response, error_response, success_response};
use super::*;
use crate::agent::AdapterLifecycleState;
use crate::core_error::CoreError;
use crate::diff::load_git_diff_snapshot;
use crate::git::{canonical_git_root, run_git};
use crate::storage::repositories::{
    CoreSettingsRepository, ProjectRepository, WorkspaceRepository,
    ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT,
};
use crate::storage::WorkspaceRuntimeStore;
use ottam_protocol::*;
use serde_json::Value;
use std::path::Path;

impl AgentServer {
    pub(super) fn handle_initialize(
        &self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<InitializeParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        // Client-capability side effects belong to the primary alone. The
        // permissions flag is global last-write-wins — an attached client
        // initializing with empty capabilities would silently disable the
        // app's permission prompts — and auto-reinstalls are the app's
        // consent to exercise, not an inspector's.
        if origin == super::ConnectionId::Primary {
            if let Some(agent_bridge) = self.agent_bridge.as_ref() {
                agent_bridge.set_client_supports_permissions(
                    params
                        .capabilities
                        .experimental_methods
                        .iter()
                        .any(|method| method == METHOD_AGENT_REQUEST_PERMISSION),
                );
                agent_bridge.set_client_supports_questions(
                    params
                        .capabilities
                        .experimental_methods
                        .iter()
                        .any(|method| method == METHOD_AGENT_ASK_QUESTION),
                );
                // Auto-upgrade: a pin that moved out from under an existing
                // install — the managed Node runtime or an adapter — upgrades
                // silently while the older version keeps launching; the first
                // install was the consent. Idempotent (current pins and
                // in-flight installs are skipped), so a reconnect is harmless.
                agent_bridge.reconcile_upgrades();
            }
        }

        tracing::info!(core_version = CORE_VERSION, "initialize accepted");
        success_response(
            request.id,
            InitializeResult {
                core_instance_id: self.core_instance_id.clone(),
                core_version: CORE_VERSION.to_string(),
                server_info: ServerInfo {
                    name: "ottam-core".to_string(),
                    version: CORE_VERSION.to_string(),
                },
            },
        )
    }

    pub(super) async fn handle_project_open_path(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ProjectOpenPathParams>(&request, InvalidParamsLog::Warn)
        {
            Ok(params) => params,
            Err(response) => return response,
        };
        let requested_path = params.path.clone();
        let canonical_root = match crate::git::run_blocking("canonicalGitRoot", move || {
            canonical_git_root(&requested_path)
        })
        .await
        {
            Ok(root) => root,
            Err(error) => return core_error_response(request.id, -32000, error),
        };
        let display_name = Path::new(&canonical_root)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Workspace")
            .to_string();
        let now = chrono::Utc::now();
        let database = self.database.clone();
        let canonical_for_store = canonical_root.clone();
        let display_for_store = display_name.clone();
        match database
            .run("project.openPath", move |connection| {
                let opened = ProjectRepository::open_local_git(
                    connection,
                    &canonical_for_store,
                    &display_for_store,
                    now,
                )?;
                let workspaces =
                    WorkspaceRepository::workspaces_for_project(connection, &opened.project.id)?;
                Ok((opened, workspaces))
            })
            .await
        {
            Ok((opened, workspaces)) => {
                let project_id = ProjectID(opened.project.id.to_string());
                let source_id = ProjectSourceID(opened.source.id.to_string());
                let branch_root = canonical_root.clone();
                let current_branch = crate::git::run_blocking("currentBranch", move || {
                    Ok(current_branch(&branch_root))
                })
                .await
                .unwrap_or(None);
                tracing::info!(
                    project_id = %project_id.0,
                    project_source_id = %source_id.0,
                    "project opened"
                );
                self.project_roots
                    .insert(project_id.clone(), canonical_root.clone());
                self.source_roots
                    .insert(source_id.clone(), canonical_root.clone());
                success_response(
                    request.id,
                    ProjectOpenPathResult {
                        project: ProjectSummary {
                            project_id: project_id.clone(),
                            display_name: opened.project.display_name,
                            created_at: opened.project.created_at.timestamp_millis(),
                            updated_at: opened.project.updated_at.timestamp_millis(),
                            last_opened_at: opened.project.last_opened_at.timestamp_millis(),
                            archived_at: opened
                                .project
                                .archived_at
                                .map(|value| value.timestamp_millis()),
                        },
                        source: ProjectSourceSummary {
                            project_source_id: source_id,
                            project_id,
                            kind: match opened.source.kind {
                                crate::storage::models::ProjectSourceKind::LocalGit => {
                                    ProjectSourceKind::LocalGit
                                }
                            },
                            display_name: opened.source.display_name,
                            canonical_path: opened.source.canonical_path,
                            created_at: opened.source.created_at.timestamp_millis(),
                            updated_at: opened.source.updated_at.timestamp_millis(),
                            archived_at: opened
                                .source
                                .archived_at
                                .map(|value| value.timestamp_millis()),
                            current_branch,
                        },
                        workspaces: workspaces
                            .into_iter()
                            .map(super::workspaces::workspace_summary)
                            .collect(),
                        created_workspace: opened
                            .created_workspace
                            .map(super::workspaces::workspace_summary),
                    },
                )
            }
            Err(error) => {
                tracing::warn!(error = %error, "project.openPath failed");
                core_error_response(request.id, -32000, error.into())
            }
        }
    }

    /// Resolves the repository inline, then answers off the dispatcher.
    ///
    /// Resolution needs `&mut self` (it memoizes the source root) and is a
    /// single indexed lookup. The snapshot itself shells out to `git` once per
    /// untracked file, so it goes to [`DeferredResponses`] rather than parking
    /// the dispatch loop for every request behind it.
    pub(super) async fn handle_git_diff_snapshot(
        &mut self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<GitDiffSnapshotParams>(&request, InvalidParamsLog::Warn)
        {
            Ok(params) => params,
            Err(response) => return Some(response),
        };

        let repo_path = match self.source_roots.get(&params.project_source_id) {
            Some(repo_path) => repo_path.clone(),
            None => {
                let source_id = params.project_source_id.clone();
                let source_id_for_query = source_id.0.clone();
                let database = self.database.clone();
                match database
                    .run("projectSource.resolve", move |connection| {
                        let source_id =
                            crate::storage::models::ProjectSourceId::try_from(source_id_for_query)?;
                        ProjectRepository::get_opened_by_source_id(connection, &source_id)
                    })
                    .await
                {
                    Ok(opened) => {
                        self.project_roots.insert(
                            ProjectID(opened.project.id.to_string()),
                            opened.source.canonical_path.clone(),
                        );
                        self.source_roots
                            .insert(source_id, opened.source.canonical_path.clone());
                        opened.source.canonical_path
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "git.diffSnapshot failed to resolve source");
                        return Some(core_error_response(request.id, -32000, error.into()));
                    }
                }
            }
        };

        let Some(deferred) = self.deferred.as_mut() else {
            return Some(error_response(
                request.id,
                -32000,
                "deferred request runtime is unavailable",
            ));
        };
        let request_id = request.id;
        let saturation_id = request_id.clone();
        if deferred
            .spawn(origin, async move {
                match crate::git::run_blocking("diffSnapshot", move || {
                    load_git_diff_snapshot(&repo_path)
                })
                .await
                {
                    Ok(snapshot) => {
                        tracing::info!(
                            files = snapshot.files.len(),
                            untracked_files = snapshot.untracked_files.len(),
                            omitted_files = snapshot.omitted_files.len(),
                            additions = snapshot.additions,
                            deletions = snapshot.deletions,
                            "git diff snapshot loaded"
                        );
                        success_response(request_id, snapshot)
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "git.diffSnapshot failed");
                        core_error_response(request_id, -32000, error)
                    }
                }
            })
            .is_err()
        {
            return Some(error_response(
                saturation_id,
                -32002,
                "git.diffSnapshot concurrency limit reached",
            ));
        }
        None
    }

    /// Start (or join) the managed-runtime install. Returns as soon as the work
    /// is accepted; `runtime.installEvent` carries everything after that.
    pub(super) fn handle_runtime_install(&mut self, request: ProtocolRequest<Value>) -> Value {
        let Some(bridge) = self.agent_bridge.as_ref() else {
            return error_response(request.id, -32000, "agent runtime is unavailable");
        };
        let service = match bridge.runtime_install_service() {
            Ok(service) => service,
            Err(error) => return super::agents::harness_error_response(request.id, error),
        };
        match service.start() {
            Ok(accepted) => success_response(
                request.id,
                RuntimeInstallResult {
                    install_id: accepted.install_id,
                    version: accepted.version,
                    phase: accepted.phase,
                },
            ),
            Err(error) => super::agents::harness_error_response(request.id, error),
        }
    }

    pub(super) fn handle_runtime_install_cancel(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let Some(bridge) = self.agent_bridge.as_ref() else {
            return error_response(request.id, -32000, "agent runtime is unavailable");
        };
        let service = match bridge.runtime_install_service() {
            Ok(service) => service,
            Err(error) => return super::agents::harness_error_response(request.id, error),
        };
        let request_id = request.id.clone();
        let params =
            match decode_params::<RuntimeInstallCancelParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        // A stale id is deliberately not an error: cancelling something that
        // already finished is a race the client cannot avoid.
        service.cancel(&params.install_id);
        success_response(request_id, RuntimeEmptyResult {})
    }

    pub(super) async fn handle_engine_list(
        &mut self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<EngineListParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return Some(response),
        };
        let workspace_store = self.workspace_runtime_store(&params.workspace_id);
        if let Err(error) = workspace_store
            .resolve_working_root(&WorkingRootSelection::WorkspaceFiles)
            .await
        {
            return Some(core_error_response(request.id, -32000, error));
        }
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return Some(error_response(
                request.id,
                -32000,
                "agent runtime is unavailable",
            ));
        };
        let request_id = request.id;
        let task_bridge = bridge.clone();
        bridge.spawn_response(origin, request_id.clone(), async move {
            let registry = task_bridge.registry();
            let model_catalog = task_bridge.model_catalog();
            if params.refresh_runtime.unwrap_or(false) {
                registry.invalidate_runtime_cache();
            }
            // Fill in any missing or stale catalogs in the background; the
            // response below serves whatever the cache already has.
            task_bridge.spawn_model_catalog_probes();
            let probed = match task_bridge.runtime_probes().probe_all().await {
                Ok(probed) => probed,
                Err(error) => return core_error_response(request_id, -32000, error),
            };
            let mut engines = Vec::with_capacity(probed.len());
            for (definition, agent_id, runtime, adapter) in probed {
                // Live sessions fold in as they start, but a background
                // probe can land later with an older answer; the running
                // adapter's view wins again here.
                model_catalog.record_live(&definition, &adapter);
                // The live adapter is also the freshest source of sign-ins;
                // fold those back too, so a picker consulted after the app
                // has been running once keeps working across a restart.
                // Recorded even when empty: a running adapter that
                // advertises none has answered, and an entry left without
                // an answer would ask for a fresh probe forever.
                if adapter.lifecycle == AdapterLifecycleState::Ready {
                    model_catalog.record_auth_methods(
                        &agent_id,
                        adapter
                            .auth_methods
                            .iter()
                            .map(engine_auth_method)
                            .collect(),
                    );
                }
                let (models, default_model_id) = model_catalog.catalog_for(&agent_id);
                let cached_prompt_image = model_catalog.prompt_image_for(&agent_id);
                let cached_auth_methods = model_catalog.auth_methods_for(&agent_id);
                let cached_selects = model_catalog.session_selects_for(&agent_id);
                let cached_model_selects = model_catalog.model_selects_for(&agent_id);
                let adapter_needs_install = registry.adapter_needs_install(&definition);
                engines.push(harness_engine(
                    definition,
                    runtime,
                    adapter,
                    models,
                    default_model_id,
                    cached_selects,
                    cached_model_selects,
                    cached_prompt_image,
                    cached_auth_methods,
                    adapter_needs_install,
                ));
            }
            success_response(request_id, EngineListResult { engines })
        });
        None
    }

    pub(super) async fn handle_conversation_create(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationCreateParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .create_conversation(
                params.working_root,
                params.context_project_ids,
                params.title,
                params.message,
            )
            .await
        {
            Ok(result) => {
                tracing::info!(
                    workspace_id = %params.workspace_id.0,
                    conversation_id = %result.conversation.conversation.conversation_id.0,
                    "conversation created"
                );
                success_response(request.id, result)
            }
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_delete(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationDeleteParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let chat = self.chat_service(&params.workspace_id);
        let Some(bridge) = self.agent_bridge.as_ref() else {
            return error_response(request.id, -32000, "agent runtime is unavailable");
        };
        match bridge
            .delete_conversation_with_children(
                &chat,
                &params.workspace_id,
                &params.conversation_id,
                params.children,
            )
            .await
        {
            Ok(result) => {
                tracing::info!(
                    workspace_id = %params.workspace_id.0,
                    conversation_id = %params.conversation_id.0,
                    harness_sessions = result.harness_session_cleanup.len(),
                    "conversation deleted"
                );
                success_response(request.id, result)
            }
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationListParams>(&request, InvalidParamsLog::Warn)
        {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat.list_conversations().await {
            Ok(conversations) => {
                success_response(request.id, ConversationListResult { conversations })
            }
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_get(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationGetParams>(&request, InvalidParamsLog::Warn)
        {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat.conversation_detail(&params.conversation_id).await {
            Ok(conversation) => {
                success_response(request.id, ConversationGetResult { conversation })
            }
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list_events(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationListEventsParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .list_events(
                &params.conversation_id,
                params.run_id,
                params.limit,
                params.cursor,
            )
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list_transcript(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationListTranscriptParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self
            .chat_service(&params.workspace_id)
            .list_transcript(params)
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_read_transcript_body(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationReadTranscriptBodyParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self
            .chat_service(&params.workspace_id)
            .read_transcript_body(params)
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_read_transcript_payload(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationReadTranscriptPayloadParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self
            .chat_service(&params.workspace_id)
            .read_transcript_payload(params)
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list_messages(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationListMessagesParams>(&request, InvalidParamsLog::Warn)
            {
                Ok(params) => params,
                Err(response) => return response,
            };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .list_messages(&params.conversation_id, params.limit, params.cursor)
            .await
        {
            Ok(page) => success_response(request.id, ConversationListMessagesResult { page }),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list_runs(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationListRunsParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .list_runs(&params.conversation_id, params.limit, params.cursor)
            .await
        {
            Ok(page) => success_response(request.id, ConversationListRunsResult { page }),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list_run_inputs(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationListRunInputsParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .list_run_inputs(&params.conversation_id, params.limit, params.cursor)
            .await
        {
            Ok(page) => success_response(request.id, ConversationListRunInputsResult { page }),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_list_agent_sessions(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationListAgentSessionsParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .list_agent_sessions(&params.conversation_id, params.limit, params.cursor)
            .await
        {
            Ok(page) => success_response(request.id, ConversationListAgentSessionsResult { page }),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_post_message(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationPostMessageParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .post_message(
                &params.conversation_id,
                params.message,
                params.attachment_ids,
                params.idempotency_key,
            )
            .await
        {
            Ok(result) => {
                tracing::info!(
                    workspace_id = %params.workspace_id.0,
                    conversation_id = %params.conversation_id.0,
                    "conversation message posted"
                );
                success_response(request.id, result)
            }
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_create_attachment(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationCreateAttachmentParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        let workspace_id = params.workspace_id.clone();
        match chat.create_attachment(params).await {
            Ok(result) => {
                tracing::info!(
                    workspace_id = %workspace_id.0,
                    attachment_id = %result.attachment.attachment_id.0,
                    "conversation attachment stored"
                );
                success_response(request.id, result)
            }
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_discard_attachment(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationDiscardAttachmentParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat.discard_attachment(&params.attachment_id).await {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_start_run(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationStartRunParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return error_response(request.id, -32000, "agent runtime is unavailable");
        };
        let known_engine = bridge
            .registry()
            .definitions()
            .iter()
            .any(|definition| definition.agent_id.0 == params.engine_id.0);
        if !known_engine {
            let id = params.engine_id.0.clone();
            let removed = self
                .database
                .run("agent.removed", move |connection| {
                    crate::storage::repositories::AgentInstanceRepository::is_deleted(
                        connection, &id,
                    )
                })
                .await
                .unwrap_or(false);
            return core_error_response(
                request.id,
                -32000,
                CoreError::new(
                    if removed {
                        "agentRemoved"
                    } else {
                        "unsupportedConversationEngine"
                    },
                    if removed {
                        "This agent was removed".into()
                    } else {
                        format!("unsupported conversation engine {}", params.engine_id.0)
                    },
                ),
            );
        }
        let chat = self.chat_service(&params.workspace_id);
        let working_root = match chat.working_root(&params.conversation_id).await {
            Ok(root) if root.availability == WorkingRootAvailability::Available => root,
            Ok(_) => {
                return core_error_response(
                    request.id,
                    -32000,
                    CoreError::new(
                        "workingRootUnavailable",
                        "the Conversation's working root is unavailable",
                    )
                    .recoverable(true),
                );
            }
            Err(error) => return core_error_response(request.id, -32000, error),
        };
        let root_path = std::path::PathBuf::from(&working_root.canonical_path);
        let project_id = working_root.project_id.clone();
        match bridge
            .start_conversation_run(params, chat, root_path, project_id)
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_cancel_run(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationCancelRunParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let chat = self.chat_service(&params.workspace_id);
        let Some(bridge) = self.agent_bridge.as_ref() else {
            return error_response(request.id, -32000, "agent runtime is unavailable");
        };
        match bridge
            .cancel_conversation_run(&chat, &params.conversation_id, &params.run_id)
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_conversation_steer_run(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<ConversationSteerRunParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return error_response(request.id, -32000, "agent runtime is unavailable");
        };
        let chat = self.chat_service(&params.workspace_id);
        let working_root = match chat.working_root(&params.conversation_id).await {
            Ok(root) if root.availability == WorkingRootAvailability::Available => root,
            Ok(_) => {
                return core_error_response(
                    request.id,
                    -32000,
                    CoreError::new(
                        "workingRootUnavailable",
                        "the Conversation's working root is unavailable",
                    )
                    .recoverable(true),
                );
            }
            Err(error) => return core_error_response(request.id, -32000, error),
        };
        let root_path = std::path::PathBuf::from(&working_root.canonical_path);
        let project_id = working_root.project_id.clone();
        match bridge
            .steer_conversation_run(params, chat, root_path, project_id)
            .await
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    /// Answered off the request loop: the agent stops the shell or subagent
    /// before it replies, and nothing else should wait on that.
    pub(super) fn handle_conversation_stop_background_task(
        &mut self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<ConversationStopBackgroundTaskParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return Some(response),
        };
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return Some(error_response(
                request.id,
                -32000,
                "agent runtime is unavailable",
            ));
        };
        let task_bridge = Arc::clone(&bridge);
        let id = request.id.clone();
        bridge.spawn_response(origin, request.id, async move {
            match task_bridge.stop_background_task(&params).await {
                Ok(result) => success_response(id, result),
                Err(error) => super::agents::harness_error_response(id, error),
            }
        });
        None
    }

    /// Answered off the request loop: a live switch waits for the agent's
    /// acknowledgment, which can take as long as the agent's current turn.
    pub(super) fn handle_conversation_set_mode(
        &mut self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params =
            match decode_params::<ConversationSetModeParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return Some(response),
            };
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return Some(error_response(
                request.id,
                -32000,
                "agent runtime is unavailable",
            ));
        };
        let chat = self.chat_service(&params.workspace_id);
        let task_bridge = Arc::clone(&bridge);
        let id = request.id.clone();
        bridge.spawn_response(origin, request.id, async move {
            match task_bridge.set_conversation_mode(&chat, &params).await {
                Ok(result) => success_response(id, result),
                Err(error) => core_error_response(id, -32000, error),
            }
        });
        None
    }

    pub(super) fn handle_conversation_continue_delegation(
        &mut self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<ottam_protocol::ConversationContinueDelegationParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return Some(response),
        };
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return Some(error_response(
                request.id,
                -32000,
                "agent runtime is unavailable",
            ));
        };
        let chat = self.chat_service(&params.workspace_id);
        let task_bridge = Arc::clone(&bridge);
        let id = request.id.clone();
        bridge.spawn_response(origin, request.id, async move {
            match task_bridge
                .continue_delegation(&chat, &params.conversation_id)
                .await
            {
                Ok(result) => success_response(id, result),
                Err(error) => core_error_response(id, -32000, error),
            }
        });
        None
    }

    pub(super) fn handle_conversation_send_to_parent(
        &mut self,
        origin: super::ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<ottam_protocol::ConversationSendToParentParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return Some(response),
        };
        let Some(bridge) = self.agent_bridge.as_ref().cloned() else {
            return Some(error_response(
                request.id,
                -32000,
                "agent runtime is unavailable",
            ));
        };
        let chat = self.chat_service(&params.workspace_id);
        let task_bridge = Arc::clone(&bridge);
        let id = request.id.clone();
        bridge.spawn_response(origin, request.id, async move {
            match task_bridge
                .send_to_parent(&chat, &params.workspace_id, &params.conversation_id)
                .await
            {
                Ok(result) => success_response(id, result),
                Err(error) => core_error_response(id, -32000, error),
            }
        });
        None
    }

    pub(super) async fn handle_conversation_set_always_allow_tool_actions(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationSetAlwaysAllowToolActionsParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        match chat
            .set_always_allow_tool_actions(&params.conversation_id, params.enabled)
            .await
        {
            Ok(()) => success_response(
                request.id,
                ToolPermissionsState {
                    always_allow_tool_actions: params.enabled,
                },
            ),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    /// The default a draft shows; the first message copies it into the new
    /// conversation.
    pub(super) async fn handle_tool_permissions_get(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        if let Err(response) =
            decode_params::<ToolPermissionsGetParams>(&request, InvalidParamsLog::Warn)
        {
            return response;
        }
        let result = self
            .database
            .run("toolPermissions.get", |connection| {
                CoreSettingsRepository::get_bool(connection, ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT)
            })
            .await;
        match result {
            Ok(value) => success_response(
                request.id,
                ToolPermissionsState {
                    always_allow_tool_actions: value.unwrap_or(false),
                },
            ),
            Err(error) => core_error_response(request.id, -32000, CoreError::from(error)),
        }
    }

    pub(super) async fn handle_tool_permissions_set(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ToolPermissionsState>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let enabled = params.always_allow_tool_actions;
        let result = self
            .database
            .run("toolPermissions.set", move |connection| {
                CoreSettingsRepository::set_bool(
                    connection,
                    ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT,
                    enabled,
                )
            })
            .await;
        match result {
            Ok(()) => success_response(request.id, params),
            Err(error) => core_error_response(request.id, -32000, CoreError::from(error)),
        }
    }

    pub(super) async fn handle_conversation_resolve_run_input_delivery(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<ConversationResolveRunInputDeliveryParams>(
            &request,
            InvalidParamsLog::Warn,
        ) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let chat = self.chat_service(&params.workspace_id);
        if matches!(params.resolution, RunInputDeliveryResolution::Retry { .. }) {
            if let Some(bridge) = self.agent_bridge.as_ref() {
                if let Err(error) = bridge
                    .refuse_resend_while_signed_out(
                        &chat,
                        &params.conversation_id,
                        &params.run_input_id,
                    )
                    .await
                {
                    return core_error_response(request.id, -32000, error);
                }
            }
        }
        match chat
            .resolve_run_input_delivery(
                &params.conversation_id,
                &params.run_input_id,
                params.resolution,
            )
            .await
        {
            Ok((resolved_input, retry_input)) => success_response(
                request.id,
                ConversationResolveRunInputDeliveryResult {
                    resolved_input,
                    retry_input,
                },
            ),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_terminal_open(&mut self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<TerminalOpenParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let runtime = self.workspace_runtime_store(&params.workspace_id);
        let resolved = match runtime.resolve_working_root(&params.working_root).await {
            Ok(resolved) => resolved,
            Err(error) => return core_error_response(request.id, -32000, error),
        };
        let root = resolved.path();
        let terminal_manager = match self.terminal_manager_mut() {
            Ok(terminal_manager) => terminal_manager,
            Err(error) => return core_error_response(request.id, -32000, error),
        };
        match terminal_manager.open(params, resolved.summary, &root).await {
            Ok(summary) => {
                tracing::info!(
                    terminal_session_id = %summary.session_id.0,
                    workspace_id = %summary.workspace_id.0,
                    "terminal opened"
                );
                success_response(request.id, summary)
            }
            Err(error) => {
                tracing::warn!(error = %error, "terminal.open failed");
                core_error_response(request.id, -32000, error)
            }
        }
    }

    pub(super) fn handle_terminal_write(&mut self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<TerminalWriteParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self
            .terminal_manager()
            .and_then(|manager| manager.write(params))
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_terminal_resize(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<TerminalResizeParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let result = match self.terminal_manager() {
            Ok(manager) => manager.resize(params).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) fn handle_terminal_close(&mut self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<TerminalCloseParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self
            .terminal_manager_mut()
            .and_then(|manager| manager.close(params))
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_terminal_list(&mut self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<TerminalListParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self
            .terminal_manager()
            .and_then(|manager| manager.list(&params.workspace_id))
        {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_terminal_delete(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params = match decode_params::<TerminalDeleteParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        match self.terminal_manager_mut().and_then(|manager| {
            manager.close_terminal_id(&params.workspace_id, &params.terminal_id)
        }) {
            Ok(()) => success_response(request.id, TerminalEmptyResult {}),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_terminal_update_metadata(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<TerminalUpdateMetadataParams>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            };
        match self.terminal_manager().and_then(|manager| {
            manager.update_metadata(
                &params.workspace_id,
                &params.terminal_id,
                params.title.as_deref(),
            )
        }) {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    pub(super) async fn handle_terminal_replay(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        const DEFAULT_REPLAY_BYTES: u64 = 128 * 1024;
        const MAX_REPLAY_BYTES: u64 = 2 * 1024 * 1024;
        let params = match decode_params::<TerminalReplayParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let max_bytes = params
            .max_bytes
            .unwrap_or(DEFAULT_REPLAY_BYTES)
            .min(MAX_REPLAY_BYTES);
        match self.terminal_manager().and_then(|manager| {
            manager.replay(params.workspace_id.as_ref(), &params.terminal_id, max_bytes)
        }) {
            Ok(result) => success_response(request.id, result),
            Err(error) => core_error_response(request.id, -32000, error),
        }
    }

    /// The manager is installed for the whole of `serve()`, so `None` is not
    /// reachable from a dispatched request. Answering with a recoverable error
    /// rather than unwrapping keeps a protocol call from aborting the Core
    /// process if that ever stops holding.
    fn terminal_manager(&self) -> Result<&TerminalManager, CoreError> {
        self.terminal_manager
            .as_ref()
            .ok_or_else(terminal_unavailable)
    }

    fn terminal_manager_mut(&mut self) -> Result<&mut TerminalManager, CoreError> {
        self.terminal_manager
            .as_mut()
            .ok_or_else(terminal_unavailable)
    }

    pub(super) fn chat_service(&self, workspace_id: &WorkspaceID) -> super::chat::ChatService {
        super::chat::ChatService::new(self.database.clone(), workspace_id.clone())
            .with_registry(self.agent_bridge.as_ref().map(|bridge| bridge.registry()))
            .with_transcript_coordinator(self.transcript_coordinator.clone())
            .with_transcript_delivery(
                self.transcript_outbound.clone(),
                self.core_instance_id.0.clone(),
            )
    }

    pub(super) fn workspace_runtime_store(
        &self,
        workspace_id: &WorkspaceID,
    ) -> WorkspaceRuntimeStore {
        WorkspaceRuntimeStore::new(self.database.clone(), workspace_id.clone())
    }
}

fn terminal_unavailable() -> CoreError {
    CoreError::new("terminalUnavailable", "terminal manager is unavailable").recoverable(true)
}

pub(super) fn current_branch(canonical_root: &str) -> Option<String> {
    run_git(
        canonical_root,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .or_else(|_| run_git(canonical_root, ["rev-parse", "--short", "HEAD"]))
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}
