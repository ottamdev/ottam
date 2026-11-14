use crate::*;
use serde_json::Value;

pub fn initialize_request_fixture() -> InitializeRequest {
    ProtocolRequest::new(
        RequestID::Number(1),
        METHOD_INITIALIZE,
        InitializeParams {
            client_info: ClientInfo {
                name: "OttamApp".to_string(),
                version: "0.1.0".to_string(),
            },
            capabilities: Capabilities {
                experimental_methods: vec![
                    METHOD_AGENT_REQUEST_PERMISSION.to_string(),
                    METHOD_AGENT_ASK_QUESTION.to_string(),
                ],
            },
        },
    )
}

pub fn initialize_response_fixture() -> InitializeResponse {
    ProtocolResponse::success(
        RequestID::Number(1),
        InitializeResult {
            core_instance_id: CoreInstanceID("019b0000-0000-7000-8000-000000000000".to_string()),
            // A fixed version, not CORE_VERSION: a release cut moves the
            // workspace version, and committed fixtures must not move with it.
            core_version: "0.1.0".to_string(),
            server_info: ServerInfo {
                name: "ottam-core".to_string(),
                version: "0.1.0".to_string(),
            },
        },
    )
}

pub fn protocol_error_response_fixture() -> ProtocolResponse<Value> {
    ProtocolResponse::failure(
        RequestID::Null,
        ProtocolError::new(-32600, "invalid request"),
    )
}

pub fn initialized_notification_fixture() -> InitializedNotification {
    ProtocolNotification::new(METHOD_INITIALIZED, InitializedParams {})
}

pub fn core_exit_error_notification_fixture() -> CoreExitErrorNotification {
    ProtocolNotification::new(
        METHOD_CORE_EXIT_ERROR,
        CoreExitError {
            kind: "storageMigrationFailed".into(),
            message: "This database contains migrations newer than this binary.".into(),
            details: Some(
                "Open this database with a newer Ottam binary. No data was reset.".into(),
            ),
        },
    )
}

pub fn project_open_path_request_fixture() -> ProjectOpenPathRequest {
    ProtocolRequest::new(
        RequestID::Number(2),
        METHOD_PROJECT_OPEN_PATH,
        ProjectOpenPathParams {
            path: "/Users/example/Developer/ottam".to_string(),
        },
    )
}

pub fn project_open_path_response_fixture() -> ProjectOpenPathResponse {
    let project_id = ProjectID("019b0000-0000-7000-8000-000000000001".to_string());
    let project = ProjectSummary {
        project_id: project_id.clone(),
        display_name: "ottam".to_string(),
        created_at: 1_750_636_800_000,
        updated_at: 1_750_636_800_000,
        last_opened_at: 1_750_636_800_000,
        archived_at: None,
    };
    let source = ProjectSourceSummary {
        project_source_id: ProjectSourceID("019b0000-0000-7000-8000-000000000002".to_string()),
        project_id,
        kind: ProjectSourceKind::LocalGit,
        display_name: "ottam".to_string(),
        canonical_path: "/Users/example/Developer/ottam".to_string(),
        created_at: 1_750_636_800_000,
        updated_at: 1_750_636_800_000,
        archived_at: None,
        current_branch: Some("main".to_string()),
    };
    let workspace = WorkspaceSummary {
        workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
        display_name: "ottam".to_string(),
        created_at: 1_750_636_800_000,
        updated_at: 1_750_636_800_000,
        last_opened_at: 1_750_636_800_000,
        archived_at: None,
    };
    ProtocolResponse::success(
        RequestID::Number(2),
        ProjectOpenPathResult {
            project,
            source,
            workspaces: vec![workspace.clone()],
            created_workspace: Some(workspace),
        },
    )
}

fn workspace_summary_fixture() -> WorkspaceSummary {
    WorkspaceSummary {
        workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
        display_name: "Ottam".to_string(),
        created_at: 1_750_636_800_000,
        updated_at: 1_750_636_800_000,
        last_opened_at: 1_750_636_800_000,
        archived_at: None,
    }
}

fn workspace_files_root_fixture() -> WorkingRootSummary {
    WorkingRootSummary {
        kind: WorkingRootKind::WorkspaceFiles,
        display_name: "Workspace files".to_string(),
        canonical_path:
            "/Users/example/.ottam/workspaces/019b0000-0000-7000-8000-000000000003/files".to_string(),
        project_id: None,
        source_id: None,
        availability: WorkingRootAvailability::Available,
    }
}

fn project_working_root_fixture() -> WorkingRootSummary {
    WorkingRootSummary {
        kind: WorkingRootKind::Project,
        display_name: "ottam".to_string(),
        canonical_path: "/Users/example/Developer/ottam".to_string(),
        project_id: Some(ProjectID(
            "019b0000-0000-7000-8000-000000000001".to_string(),
        )),
        source_id: Some(ProjectSourceID(
            "019b0000-0000-7000-8000-000000000002".to_string(),
        )),
        availability: WorkingRootAvailability::Available,
    }
}

fn workspace_detail_fixture() -> WorkspaceDetail {
    let opened = project_open_path_response_fixture()
        .result
        .expect("project fixture result");
    WorkspaceDetail {
        workspace: workspace_summary_fixture(),
        workspace_files: workspace_files_root_fixture(),
        projects: vec![WorkspaceProjectSummary {
            project: opened.project,
            source: opened.source,
            sort_order: 0,
            added_at: 1_750_636_800_000,
        }],
    }
}

pub fn workspace_list_request_fixture() -> WorkspaceListRequest {
    ProtocolRequest::new(
        RequestID::Number(20),
        METHOD_WORKSPACE_LIST,
        WorkspaceListParams {
            include_archived: true,
        },
    )
}

pub fn workspace_list_response_fixture() -> WorkspaceListResponse {
    ProtocolResponse::success(
        RequestID::Number(20),
        WorkspaceListResult {
            workspaces: vec![WorkspaceListItem {
                workspace: workspace_summary_fixture(),
                project_paths: vec!["/Users/example/Developer/ottam".to_string()],
            }],
        },
    )
}

pub fn workspace_create_request_fixture() -> WorkspaceCreateRequest {
    ProtocolRequest::new(
        RequestID::Number(21),
        METHOD_WORKSPACE_CREATE,
        WorkspaceCreateParams {
            display_name: "Ottam".to_string(),
        },
    )
}

pub fn workspace_create_response_fixture() -> WorkspaceCreateResponse {
    ProtocolResponse::success(RequestID::Number(21), workspace_detail_fixture())
}

pub fn workspace_open_request_fixture() -> WorkspaceOpenRequest {
    ProtocolRequest::new(
        RequestID::Number(22),
        METHOD_WORKSPACE_OPEN,
        WorkspaceOpenParams {
            workspace_id: workspace_summary_fixture().workspace_id,
        },
    )
}

pub fn workspace_open_response_fixture() -> WorkspaceOpenResponse {
    ProtocolResponse::success(RequestID::Number(22), workspace_detail_fixture())
}

pub fn workspace_get_request_fixture() -> WorkspaceGetRequest {
    ProtocolRequest::new(
        RequestID::Number(23),
        METHOD_WORKSPACE_GET,
        WorkspaceGetParams {
            workspace_id: workspace_summary_fixture().workspace_id,
        },
    )
}

pub fn workspace_get_response_fixture() -> WorkspaceGetResponse {
    ProtocolResponse::success(RequestID::Number(23), workspace_detail_fixture())
}

pub fn workspace_update_request_fixture() -> WorkspaceUpdateRequest {
    ProtocolRequest::new(
        RequestID::Number(24),
        METHOD_WORKSPACE_UPDATE,
        WorkspaceUpdateParams {
            workspace_id: workspace_summary_fixture().workspace_id,
            display_name: "Delivery".to_string(),
        },
    )
}

pub fn workspace_update_response_fixture() -> WorkspaceUpdateResponse {
    ProtocolResponse::success(RequestID::Number(24), workspace_detail_fixture())
}

pub fn workspace_archive_request_fixture() -> WorkspaceArchiveRequest {
    ProtocolRequest::new(
        RequestID::Number(25),
        METHOD_WORKSPACE_ARCHIVE,
        WorkspaceArchiveParams {
            workspace_id: workspace_summary_fixture().workspace_id,
        },
    )
}

pub fn workspace_archive_response_fixture() -> WorkspaceArchiveResponse {
    ProtocolResponse::success(RequestID::Number(25), workspace_summary_fixture())
}

pub fn workspace_restore_request_fixture() -> WorkspaceRestoreRequest {
    ProtocolRequest::new(
        RequestID::Number(26),
        METHOD_WORKSPACE_RESTORE,
        WorkspaceRestoreParams {
            workspace_id: workspace_summary_fixture().workspace_id,
        },
    )
}

pub fn workspace_restore_response_fixture() -> WorkspaceRestoreResponse {
    ProtocolResponse::success(RequestID::Number(26), workspace_summary_fixture())
}

pub fn workspace_add_project_request_fixture() -> WorkspaceAddProjectRequest {
    ProtocolRequest::new(
        RequestID::Number(27),
        METHOD_WORKSPACE_ADD_PROJECT,
        WorkspaceAddProjectParams {
            workspace_id: workspace_summary_fixture().workspace_id,
            path: "/Users/example/Developer/ottam".to_string(),
        },
    )
}

pub fn workspace_add_project_response_fixture() -> WorkspaceAddProjectResponse {
    let detail = workspace_detail_fixture();
    ProtocolResponse::success(
        RequestID::Number(27),
        WorkspaceAddProjectResult {
            project_id: detail.projects[0].project.project_id.clone(),
            workspace: detail,
        },
    )
}

pub fn workspace_remove_project_request_fixture() -> WorkspaceRemoveProjectRequest {
    let detail = workspace_detail_fixture();
    ProtocolRequest::new(
        RequestID::Number(28),
        METHOD_WORKSPACE_REMOVE_PROJECT,
        WorkspaceRemoveProjectParams {
            workspace_id: detail.workspace.workspace_id,
            project_id: detail.projects[0].project.project_id.clone(),
        },
    )
}

pub fn workspace_remove_project_response_fixture() -> WorkspaceRemoveProjectResponse {
    ProtocolResponse::success(
        RequestID::Number(28),
        WorkspaceDetail {
            workspace: workspace_summary_fixture(),
            workspace_files: workspace_files_root_fixture(),
            projects: Vec::new(),
        },
    )
}

pub fn git_diff_snapshot_request_fixture() -> GitDiffSnapshotRequest {
    ProtocolRequest::new(
        RequestID::Number(3),
        METHOD_GIT_DIFF_SNAPSHOT,
        GitDiffSnapshotParams {
            project_source_id: ProjectSourceID("019b0000-0000-7000-8000-000000000002".to_string()),
        },
    )
}

pub fn git_diff_snapshot_response_fixture() -> GitDiffSnapshotResponse {
    ProtocolResponse::success(
        RequestID::Number(3),
        DiffSnapshot {
            repo_path: "/Users/example/Developer/ottam".to_string(),
            branch: "main".to_string(),
            ahead: 0,
            behind: 0,
            additions: 1,
            deletions: 1,
            files: vec![DiffFile {
                path: "README.md".to_string(),
                old_path: Some("README.md".to_string()),
                status: "modified".to_string(),
                additions: 1,
                deletions: 1,
                hunks: vec![DiffHunk {
                    kind: "diff".to_string(),
                    header: "@@ -1 +1 @@".to_string(),
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    lines: vec![
                        DiffLine {
                            kind: "deletion".to_string(),
                            old_line: Some(1),
                            new_line: None,
                            content: "old".to_string(),
                        },
                        DiffLine {
                            kind: "addition".to_string(),
                            old_line: None,
                            new_line: Some(1),
                            content: "new".to_string(),
                        },
                    ],
                }],
            }],
            untracked_files: Vec::new(),
            omitted_files: Vec::new(),
        },
    )
}

pub fn engine_summary_fixture() -> EngineSummary {
    EngineSummary {
        engine_id: EngineID("codex".to_string()),
        display_name: "Codex".to_string(),
        brand: Some("openai".to_string()),
        auth_state: EngineAuthState::Ready,
        install_state: EngineInstallState::Ready,
        capabilities: vec![EngineCapability::Streaming, EngineCapability::Cancel],
        models: vec![EngineModelOption {
            model_id: "gpt-5".to_string(),
            display_name: "GPT-5".to_string(),
            thought_levels: Some(vec![EngineSelectOption {
                id: "high".to_string(),
                display_name: "High".to_string(),
            }]),
            default_thought_level_id: Some("high".to_string()),
        }],
        default_model_id: Some("gpt-5".to_string()),
        modes: vec![EngineSelectOption {
            id: "auto".to_string(),
            display_name: "Auto".to_string(),
        }],
        default_mode_id: Some("auto".to_string()),
        thought_levels: vec![EngineSelectOption {
            id: "medium".to_string(),
            display_name: "Medium".to_string(),
        }],
        default_thought_level_id: Some("medium".to_string()),
        steering_capability: SteeringCapability::InterruptThenPrompt,
        auth_methods: vec![AgentAuthMethod {
            method_id: "chat-gpt".to_string(),
            name: "Sign in with ChatGPT".to_string(),
            description: None,
            kind: AgentAuthMethodKind::Agent,
        }],
        runtime: Some(AgentRuntimeStatus {
            availability: AgentRuntimeAvailability::Ready,
            source: Some(AgentRuntimeSource::System),
            component: None,
            found_major: None,
            minimum_major: None,
            node_version: Some("v22.12.0".to_string()),
            npx_version: Some("10.9.0".to_string()),
            managed_update: None,
        }),
    }
}

pub fn engine_list_request_fixture() -> EngineListRequest {
    ProtocolRequest::new(
        RequestID::Number(40),
        METHOD_ENGINE_LIST,
        EngineListParams {
            workspace_id: Some(WorkspaceID(
                "019b0000-0000-7000-8000-000000000003".to_string(),
            )),
            refresh_runtime: None,
        },
    )
}

pub fn engine_list_response_fixture() -> EngineListResponse {
    ProtocolResponse::success(
        RequestID::Number(40),
        EngineListResult {
            engines: vec![engine_summary_fixture()],
        },
    )
}

pub fn engine_changed_notification_fixture() -> EngineChangedNotification {
    ProtocolNotification::new(
        METHOD_ENGINE_CHANGED,
        EngineChangedParams {
            engine_id: engine_summary_fixture().engine_id,
        },
    )
}

pub fn agent_summary_fixture() -> AgentSummary {
    AgentSummary {
        can_set_layout: Some(true),
        harness_id: Some("codex".into()),
        label: Some("Codex".into()),
        home_kind: Some(AgentHomeKind::Managed),
        agent_id: AgentID("codex".to_string()),
        display_name: "Codex".to_string(),
        vendor_name: "OpenAI".to_string(),
        vendor_home: Some("~/.codex".to_string()),
        brand: Some("openai".to_string()),
        account_label: "ChatGPT account".to_string(),
        account_kind: AgentAccountKind::Subscription,
        model_providers: None,
        runtime: AgentRuntimeStatus {
            availability: AgentRuntimeAvailability::Ready,
            source: Some(AgentRuntimeSource::System),
            component: None,
            found_major: None,
            minimum_major: None,
            node_version: Some("v22.12.0".to_string()),
            npx_version: Some("10.9.0".to_string()),
            managed_update: None,
        },
        adapter: AgentAdapterStatus {
            lifecycle: AgentLifecycleState::Ready,
            auth_state: AgentAuthState::Ready,
            auth_methods: vec![AgentAuthMethod {
                method_id: "chatgpt".to_string(),
                name: "Sign in with ChatGPT".to_string(),
                description: None,
                kind: AgentAuthMethodKind::Agent,
            }],
            agent_name: Some("codex-acp".to_string()),
            agent_version: Some("1.0.0".to_string()),
            steering_capability: SteeringCapability::InterruptThenPrompt,
            supports_image_input: true,
            compatibility: vec![
                AgentFeatureCompatibility {
                    feature: "loadSession".to_string(),
                    group: AgentFeatureGroup::AgentCapability,
                    sdk_status: AgentFeatureSdkStatus::Stable,
                    sdk_detail: None,
                    agent_advertises: Some(true),
                    client_support: AgentFeatureClientSupport::Supported,
                    client_detail: None,
                    verdict: AgentFeatureVerdict::Works,
                },
                AgentFeatureCompatibility {
                    feature: "usage_update".to_string(),
                    group: AgentFeatureGroup::UpdateKind,
                    sdk_status: AgentFeatureSdkStatus::Stable,
                    sdk_detail: None,
                    agent_advertises: None,
                    client_support: AgentFeatureClientSupport::Partial,
                    client_detail: Some("persisted as session metadata; not surfaced".to_string()),
                    verdict: AgentFeatureVerdict::Unknown,
                },
                AgentFeatureCompatibility {
                    feature: "session.compaction".to_string(),
                    group: AgentFeatureGroup::ClientCapability,
                    sdk_status: AgentFeatureSdkStatus::UnstableOff,
                    sdk_detail: Some("unstable_session_compaction".to_string()),
                    agent_advertises: None,
                    client_support: AgentFeatureClientSupport::Unsupported,
                    client_detail: None,
                    verdict: AgentFeatureVerdict::Unknown,
                },
            ],
            credentials_seen: true,
            generation: 1,
            last_error: None,
        },
        adapter_package: Some(AgentAdapterPackage {
            name: "@agentclientprotocol/codex-acp".to_string(),
            version: "1.1.9".to_string(),
            source: AgentAdapterSource::Npm,
            install_state: AgentAdapterInstallState::Installed,
            // An app update moved the pin; the older install keeps launching.
            installed_version: Some("1.1.8".to_string()),
            update: Some(AgentUpdateStatus {
                version: "1.1.9".to_string(),
                state: AgentUpdateState::Updating,
                error: None,
            }),
        }),
    }
}

pub fn agent_home_info_request_fixture() -> AgentHomeInfoRequest {
    ProtocolRequest::new(
        RequestID::Number(48),
        METHOD_AGENT_HOME_INFO,
        AgentHomeInfoParams {
            agent_id: AgentID("claude".to_string()),
        },
    )
}

pub fn agent_home_info_response_fixture() -> AgentHomeInfoResponse {
    ProtocolResponse::success(
        RequestID::Number(48),
        AgentHomeInfoResult {
            path: "/Users/example/.ottam/agents/claude/home".to_string(),
            size_bytes: 4_718_592,
        },
    )
}

pub fn agent_list_request_fixture() -> AgentListRequest {
    ProtocolRequest::new(
        RequestID::Number(41),
        METHOD_AGENT_LIST,
        AgentListParams {
            refresh_runtime: Some(true),
        },
    )
}

pub fn storage_status_request_fixture() -> ProtocolRequest<StorageUpdateParams> {
    ProtocolRequest::new(
        RequestID::Number(80),
        METHOD_STORAGE_STATUS,
        StorageUpdateParams { limit_gb: None },
    )
}

pub fn storage_update_request_fixture() -> ProtocolRequest<StorageUpdateParams> {
    ProtocolRequest::new(
        RequestID::Number(81),
        METHOD_STORAGE_UPDATE,
        StorageUpdateParams { limit_gb: Some(5) },
    )
}

pub fn storage_clear_cache_request_fixture() -> ProtocolRequest<StorageUpdateParams> {
    ProtocolRequest::new(
        RequestID::Number(82),
        METHOD_STORAGE_CLEAR_CACHE,
        StorageUpdateParams { limit_gb: None },
    )
}

pub fn storage_status_response_fixture() -> ProtocolResponse<StorageStatusResult> {
    ProtocolResponse::success(
        RequestID::Number(80),
        StorageStatusResult {
            cache_bytes: 1024,
            limit_gb: 5,
            database_bytes: 32768,
            wal_bytes: 0,
        },
    )
}

pub fn agent_list_response_fixture() -> AgentListResponse {
    ProtocolResponse::success(
        RequestID::Number(41),
        AgentListResult {
            agents: vec![agent_summary_fixture()],
        },
    )
}

pub fn agent_auth_state_request_fixture() -> AgentAuthStateRequest {
    ProtocolRequest::new(
        RequestID::Number(42),
        METHOD_AGENT_AUTH_STATE,
        AgentAuthStateParams {
            agent_id: AgentID("codex".to_string()),
            refresh: None,
        },
    )
}

pub fn agent_detect_setups_request_fixture() -> ProtocolRequest<AgentListParams> {
    ProtocolRequest::new(
        RequestID::Number(420),
        METHOD_AGENT_DETECT_SETUPS,
        AgentListParams {
            refresh_runtime: None,
        },
    )
}

pub fn agent_detect_setups_response_fixture() -> ProtocolResponse<AgentDetectSetupsResult> {
    ProtocolResponse::success(
        RequestID::Number(420),
        AgentDetectSetupsResult {
            setups: vec![AgentDetectedSetup {
                harness_id: "codex".into(),
                display_name: "Codex".into(),
                path: "/fixture/.codex".into(),
                available: true,
                detail: None,
                cli_version: Some("codex-cli fixture".into()),
                skills: Some(2),
                hooks: None,
                mcp_servers: Some(0),
                plugins: None,
            }],
        },
    )
}

pub fn agent_create_request_fixture() -> ProtocolRequest<AgentCreateParams> {
    ProtocolRequest::new(
        RequestID::Number(421),
        METHOD_AGENT_CREATE,
        AgentCreateParams {
            harness_id: "codex".into(),
            layout: AgentHomeKind::Existing,
            label: None,
        },
    )
}

pub fn agent_create_response_fixture() -> ProtocolResponse<AgentInstanceResult> {
    ProtocolResponse::success(
        RequestID::Number(421),
        AgentInstanceResult {
            agent_id: AgentID("codex.fixture".into()),
        },
    )
}

pub fn agent_create_managed_request_fixture() -> ProtocolRequest<AgentCreateParams> {
    ProtocolRequest::new(
        RequestID::Number(427),
        METHOD_AGENT_CREATE,
        AgentCreateParams {
            harness_id: "claude".into(),
            layout: AgentHomeKind::Managed,
            label: Some("Work".into()),
        },
    )
}

pub fn agent_rename_request_fixture() -> ProtocolRequest<AgentRenameParams> {
    ProtocolRequest::new(
        RequestID::Number(428),
        METHOD_AGENT_RENAME,
        AgentRenameParams {
            agent_id: AgentID("claude.profile".into()),
            label: "Office".into(),
        },
    )
}

pub fn agent_delete_request_fixture() -> ProtocolRequest<AgentDeleteParams> {
    ProtocolRequest::new(
        RequestID::Number(429),
        METHOD_AGENT_DELETE,
        AgentDeleteParams {
            agent_id: AgentID("claude.profile".into()),
            purge: false,
            operation_key: "delete-profile".into(),
        },
    )
}

pub fn agent_set_layout_request_fixture() -> ProtocolRequest<AgentSetLayoutParams> {
    ProtocolRequest::new(
        RequestID::Number(422),
        METHOD_AGENT_SET_LAYOUT,
        AgentSetLayoutParams {
            agent_id: AgentID("codex".into()),
            layout: AgentHomeKind::Existing,
        },
    )
}

pub fn agent_set_layout_response_fixture() -> ProtocolResponse<AgentInstanceResult> {
    ProtocolResponse::success(
        RequestID::Number(422),
        AgentInstanceResult {
            agent_id: AgentID("codex".into()),
        },
    )
}

pub fn agent_shutdown_readiness_request_fixture() -> ProtocolRequest<AgentListParams> {
    ProtocolRequest::new(
        RequestID::Number(423),
        METHOD_AGENT_SHUTDOWN_READINESS,
        AgentListParams {
            refresh_runtime: None,
        },
    )
}

pub fn agent_shutdown_readiness_response_fixture() -> ProtocolResponse<AgentShutdownReadinessResult>
{
    ProtocolResponse::success(
        RequestID::Number(423),
        AgentShutdownReadinessResult {
            has_shared_homes: Some(false),
            ready: false,
            waiting_on: vec!["Claude — Your setup".into()],
            refresh_lock_paths: vec!["/fixture/.claude/.oauth_refresh.lock".into()],
        },
    )
}

pub fn agent_prepare_shutdown_request_fixture() -> ProtocolRequest<AgentShutdownOperationParams> {
    ProtocolRequest::new(
        RequestID::Number(424),
        METHOD_AGENT_PREPARE_SHUTDOWN,
        AgentShutdownOperationParams {
            operation_key: "fixture-quit".into(),
        },
    )
}

pub fn agent_prepare_shutdown_response_fixture() -> ProtocolResponse<AgentEmptyResult> {
    ProtocolResponse::success(RequestID::Number(424), AgentEmptyResult {})
}

pub fn agent_cancel_shutdown_request_fixture() -> ProtocolRequest<AgentShutdownOperationParams> {
    ProtocolRequest::new(
        RequestID::Number(425),
        METHOD_AGENT_CANCEL_SHUTDOWN,
        AgentShutdownOperationParams {
            operation_key: "fixture-quit".into(),
        },
    )
}

pub fn agent_cancel_shutdown_response_fixture() -> ProtocolResponse<AgentEmptyResult> {
    ProtocolResponse::success(RequestID::Number(425), AgentEmptyResult {})
}

pub fn agent_auth_state_response_fixture() -> AgentAuthStateResponse {
    ProtocolResponse::success(
        RequestID::Number(42),
        AgentAuthStateResult {
            agent_id: AgentID("codex".to_string()),
            adapter: agent_summary_fixture().adapter,
        },
    )
}

pub fn agent_sign_in_request_fixture() -> AgentSignInRequest {
    ProtocolRequest::new(
        RequestID::Number(430),
        METHOD_AGENT_SIGN_IN,
        AgentSignInParams {
            agent_id: AgentID("codex".to_string()),
            // The common case still names a workspace; omitting it (Settings,
            // where there may be none) is covered by core's own tests.
            workspace_id: Some(WorkspaceID(
                "019b0000-0000-7000-8000-000000000003".to_string(),
            )),
            method_id: None,
        },
    )
}

pub fn agent_sign_in_response_fixture() -> AgentSignInResponse {
    ProtocolResponse::success(
        RequestID::Number(430),
        AgentSignInResult {
            agent_id: AgentID("codex".to_string()),
            auth_session_id: AgentAuthSessionID("agent_auth_fixture".to_string()),
            delivery: AgentAuthDelivery::AdapterBrowser,
            phase: AgentAuthPhase::Starting,
        },
    )
}

pub fn agent_sign_in_cancel_request_fixture() -> AgentSignInCancelRequest {
    ProtocolRequest::new(
        RequestID::Number(431),
        METHOD_AGENT_SIGN_IN_CANCEL,
        AgentSignInCancelParams {
            agent_id: AgentID("codex".to_string()),
            auth_session_id: AgentAuthSessionID("agent_auth_fixture".to_string()),
        },
    )
}

pub fn agent_sign_in_cancel_response_fixture() -> AgentSignInCancelResponse {
    ProtocolResponse::success(RequestID::Number(431), AgentEmptyResult {})
}

pub fn agent_sign_in_finish_request_fixture() -> AgentSignInFinishRequest {
    ProtocolRequest::new(
        RequestID::Number(432),
        METHOD_AGENT_SIGN_IN_FINISH,
        AgentSignInFinishParams {
            agent_id: AgentID("codex".to_string()),
            auth_session_id: AgentAuthSessionID("agent_auth_fixture".to_string()),
        },
    )
}

pub fn agent_sign_in_finish_response_fixture() -> AgentSignInFinishResponse {
    ProtocolResponse::success(RequestID::Number(432), AgentEmptyResult {})
}

pub fn runtime_install_request_fixture() -> RuntimeInstallRequest {
    ProtocolRequest::new(
        RequestID::Number(440),
        METHOD_RUNTIME_INSTALL,
        RuntimeInstallParams {},
    )
}

pub fn runtime_install_response_fixture() -> RuntimeInstallResponse {
    ProtocolResponse::success(
        RequestID::Number(440),
        RuntimeInstallResult {
            install_id: RuntimeInstallID("runtime_install_fixture".to_string()),
            version: "v24.22.1".to_string(),
            phase: RuntimeInstallPhase::Starting,
        },
    )
}

pub fn runtime_install_cancel_request_fixture() -> RuntimeInstallCancelRequest {
    ProtocolRequest::new(
        RequestID::Number(441),
        METHOD_RUNTIME_INSTALL_CANCEL,
        RuntimeInstallCancelParams {
            install_id: RuntimeInstallID("runtime_install_fixture".to_string()),
        },
    )
}

pub fn runtime_install_cancel_response_fixture() -> RuntimeInstallCancelResponse {
    ProtocolResponse::success(RequestID::Number(441), RuntimeEmptyResult {})
}

pub fn agent_install_request_fixture() -> AgentInstallRequest {
    ProtocolRequest::new(
        RequestID::Number(450),
        METHOD_AGENT_INSTALL,
        AgentInstallParams {
            agent_id: AgentID("codex".to_string()),
        },
    )
}

pub fn agent_install_response_fixture() -> AgentInstallResponse {
    ProtocolResponse::success(
        RequestID::Number(450),
        AgentInstallResult {
            install_id: AgentInstallID("agent_install_fixture".to_string()),
            agent_id: AgentID("codex".to_string()),
            name: "@agentclientprotocol/codex-acp".to_string(),
            version: "1.1.9".to_string(),
            phase: AgentInstallPhase::Starting,
        },
    )
}

pub fn agent_install_cancel_request_fixture() -> AgentInstallCancelRequest {
    ProtocolRequest::new(
        RequestID::Number(451),
        METHOD_AGENT_INSTALL_CANCEL,
        AgentInstallCancelParams {
            install_id: AgentInstallID("agent_install_fixture".to_string()),
        },
    )
}

pub fn agent_install_cancel_response_fixture() -> AgentInstallCancelResponse {
    ProtocolResponse::success(RequestID::Number(451), AgentEmptyResult {})
}

/// The remove-everything shape, since it is the one carrying the optional
/// field; a plain remove is this one without it.
pub fn agent_uninstall_request_fixture() -> AgentUninstallRequest {
    ProtocolRequest::new(
        RequestID::Number(452),
        METHOD_AGENT_UNINSTALL,
        AgentUninstallParams {
            operation_key: None,
            agent_id: AgentID("codex".to_string()),
            remove_home: Some(true),
        },
    )
}

pub fn agent_uninstall_response_fixture() -> AgentUninstallResponse {
    ProtocolResponse::success(RequestID::Number(452), AgentEmptyResult {})
}

pub fn agent_plan_usage_request_fixture() -> AgentPlanUsageRequest {
    ProtocolRequest::new(
        RequestID::Number(458),
        METHOD_AGENT_PLAN_USAGE,
        AgentPlanUsageParams {
            agent_id: AgentID("claude".to_string()),
            refresh: Some(true),
        },
    )
}

fn plan_usage_fixture() -> AgentPlanUsage {
    AgentPlanUsage {
        agent_id: AgentID("claude".to_string()),
        windows: vec![
            AgentPlanUsageWindow {
                kind: AgentPlanUsageWindowKind::FiveHour,
                label: "5h".to_string(),
                used_percent: 12.0,
                resets_at: Some(1_793_946_600),
                model: None,
            },
            AgentPlanUsageWindow {
                kind: AgentPlanUsageWindowKind::Weekly,
                label: "wk".to_string(),
                used_percent: 15.0,
                resets_at: Some(1_794_042_000),
                model: None,
            },
            AgentPlanUsageWindow {
                kind: AgentPlanUsageWindowKind::Weekly,
                label: "Fable".to_string(),
                used_percent: 2.0,
                resets_at: Some(1_794_041_999),
                model: Some("Fable".to_string()),
            },
        ],
        limit_status: Some("allowed".to_string()),
        source: AgentPlanUsageSource::Probe,
        observed_at: 1_793_935_258_763,
        error: None,
    }
}

pub fn agent_plan_usage_response_fixture() -> AgentPlanUsageResponse {
    ProtocolResponse::success(
        RequestID::Number(458),
        AgentPlanUsageResult {
            usage: Some(plan_usage_fixture()),
        },
    )
}

/// A failed refresh: the last windows stay, with the error beside them.
pub fn agent_plan_usage_changed_notification_fixture() -> AgentPlanUsageChangedNotification {
    ProtocolNotification::new(
        METHOD_AGENT_PLAN_USAGE_CHANGED,
        AgentPlanUsageChangedParams {
            agent_id: AgentID("claude".to_string()),
            usage: Some(AgentPlanUsage {
                error: Some("Usage refresh timed out".to_string()),
                ..plan_usage_fixture()
            }),
        },
    )
}

/// The failed phase, since it is the one carrying the error — every other
/// phase is this one minus the optional field.
pub fn agent_install_event_notification_fixture() -> AgentInstallEventNotification {
    ProtocolNotification::new(
        METHOD_AGENT_INSTALL_EVENT,
        AgentInstallEventParams {
            install_id: AgentInstallID("agent_install_fixture".to_string()),
            agent_id: AgentID("codex".to_string()),
            name: "@agentclientprotocol/codex-acp".to_string(),
            version: "1.1.9".to_string(),
            phase: AgentInstallPhase::Failed,
            error: Some("npm install exited with status 1".to_string()),
        },
    )
}

/// The downloading phase, since it is the one carrying the byte counts — every
/// other phase is this one minus the optional fields.
pub fn runtime_install_event_notification_fixture() -> RuntimeInstallEventNotification {
    ProtocolNotification::new(
        METHOD_RUNTIME_INSTALL_EVENT,
        RuntimeInstallEventParams {
            install_id: RuntimeInstallID("runtime_install_fixture".to_string()),
            version: "v24.22.1".to_string(),
            phase: RuntimeInstallPhase::Downloading,
            bytes_received: Some(12_582_912),
            bytes_total: Some(52_428_800),
            error: None,
        },
    )
}

/// The terminal-delivery shape, since it is the one carrying every optional
/// field — an `adapterBrowser` event is this one minus the terminal.
pub fn agent_sign_in_event_notification_fixture() -> AgentSignInEventNotification {
    ProtocolNotification::new(
        METHOD_AGENT_SIGN_IN_EVENT,
        AgentSignInEventParams {
            agent_id: AgentID("claude".to_string()),
            auth_session_id: AgentAuthSessionID("agent_auth_fixture".to_string()),
            delivery: AgentAuthDelivery::ClientTerminal,
            phase: AgentAuthPhase::NeedsInput,
            reveal_terminal: true,
            terminal: Some(AgentAuthTerminal {
                session_id: TerminalSessionID("terminal_local_fixture".to_string()),
                terminal_id: TerminalID("019b0000-0000-7000-8000-000000000004".to_string()),
                rows: 24,
                cols: 80,
            }),
            error: None,
        },
    )
}

pub fn agent_request_permission_request_fixture() -> AgentRequestPermissionRequest {
    ProtocolRequest::new(
        RequestID::String("core/req/1".to_string()),
        METHOD_AGENT_REQUEST_PERMISSION,
        AgentRequestPermissionParams {
            agent_id: AgentID("codex".to_string()),
            session_id: AgentSessionID("session_fixture".to_string()),
            workspace_id: Some(WorkspaceID(
                "019b0000-0000-7000-8000-000000000003".to_string(),
            )),
            project_id: Some(ProjectID(
                "019b0000-0000-7000-8000-000000000001".to_string(),
            )),
            conversation_id: Some(ConversationID("conversation_fixture".to_string())),
            run_id: Some(ConversationRunID("conversation_run_fixture".to_string())),
            subagent_thread_id: Some("subagent:child_session_fixture".to_string()),
            subagent_name: Some("Reviewer".to_string()),
            plan: None,
            tool_call: serde_json::json!({"toolCallID":"tool_fixture","title":"Write file"}),
            options: vec![AgentPermissionOption {
                option_id: "allow_once".to_string(),
                name: "Allow once".to_string(),
                kind: AgentPermissionOptionKind::AllowOnce,
            }],
        },
    )
}

pub fn agent_request_permission_response_fixture() -> AgentRequestPermissionResponse {
    ProtocolResponse::success(
        RequestID::String("core/req/1".to_string()),
        AgentRequestPermissionResult {
            outcome: AgentPermissionOutcome::Selected {
                option_id: "allow_once".to_string(),
            },
        },
    )
}

pub fn agent_ask_question_request_fixture() -> AgentAskQuestionRequest {
    ProtocolRequest::new(
        RequestID::String("core/req/2".to_string()),
        METHOD_AGENT_ASK_QUESTION,
        AgentAskQuestionParams {
            agent_id: AgentID("claude".to_string()),
            session_id: AgentSessionID("session_fixture".to_string()),
            workspace_id: Some(WorkspaceID(
                "019b0000-0000-7000-8000-000000000003".to_string(),
            )),
            project_id: Some(ProjectID(
                "019b0000-0000-7000-8000-000000000001".to_string(),
            )),
            conversation_id: Some(ConversationID("conversation_fixture".to_string())),
            run_id: Some(ConversationRunID("conversation_run_fixture".to_string())),
            subagent_thread_id: None,
            subagent_name: None,
            provider_tool_call_id: Some("tool_fixture".to_string()),
            title: "Please answer the following questions.".to_string(),
            questions: vec![
                AgentQuestion {
                    id: "q0".to_string(),
                    header: Some("Approach".to_string()),
                    prompt: "Which approach should I take?".to_string(),
                    required: false,
                    kind: AgentQuestionKind::SingleSelect {
                        options: vec![
                            AgentQuestionOption {
                                value: "Refactor".to_string(),
                                label: "Refactor".to_string(),
                                description: Some("Smaller diff.".to_string()),
                            },
                            AgentQuestionOption {
                                value: "Rewrite".to_string(),
                                label: "Rewrite".to_string(),
                                description: None,
                            },
                        ],
                        allows_other: true,
                        default: None,
                    },
                },
                AgentQuestion {
                    id: "q1".to_string(),
                    header: None,
                    prompt: "Token".to_string(),
                    required: true,
                    kind: AgentQuestionKind::Text {
                        secure: true,
                        default: None,
                    },
                },
            ],
        },
    )
}

pub fn agent_ask_question_response_fixture() -> AgentAskQuestionResponse {
    ProtocolResponse::success(
        RequestID::String("core/req/2".to_string()),
        AgentAskQuestionResult {
            outcome: AgentQuestionOutcome::Submitted {
                answers: vec![AgentQuestionAnswer {
                    question_id: "q0".to_string(),
                    values: vec!["Refactor".to_string()],
                    other: None,
                }],
            },
        },
    )
}

pub fn cancel_request_notification_fixture() -> CancelRequestNotification {
    ProtocolNotification::new(
        NOTIFICATION_CANCEL_REQUEST,
        CancelRequestParams {
            id: RequestID::String("core/req/2".to_string()),
        },
    )
}

pub fn conversation_summary_fixture() -> ConversationSummary {
    ConversationSummary {
        conversation_id: ConversationID("conversation_fixture".to_string()),
        workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
        working_root: project_working_root_fixture(),
        title: "Chat engine routing".to_string(),
        created_at: "2026-07-03T22:00:00.000Z".to_string(),
        updated_at: "2026-07-03T22:00:01.000Z".to_string(),
        always_allow_tool_actions: false,
        parent_conversation_id: None,
        delegated_agent_name: None,
        delegated_task_title: None,
        delegated_avatar_seed: None,
        delegation_key: None,
        delegated_from_run_id: None,
        delegation_activity: None,
        deferred_report_run_id: None,
    }
}

pub fn conversation_run_summary_fixture() -> ConversationRunSummary {
    ConversationRunSummary {
        run_id: ConversationRunID("conversation_run_fixture".to_string()),
        conversation_id: ConversationID("conversation_fixture".to_string()),
        engine_id: EngineID("codex".to_string()),
        model_id: Some("gpt-5".to_string()),
        mode_id: Some("default".to_string()),
        thought_level_id: Some("medium".to_string()),
        status: ConversationRunStatus::Completed,
        started_at: "2026-07-03T22:00:00.500Z".to_string(),
        completed_at: Some("2026-07-03T22:00:01.000Z".to_string()),
        agent_session_id: Some(AgentSessionID("agent_session_fixture".to_string())),
        session_origin: Some(SessionOrigin::New),
        origin: RunOrigin::User,
        stop_reason: Some(AgentStopReason::EndTurn),
        cost: serde_json::json!({}),
        usage: Some(ConversationRunUsage {
            scope: ConversationRunUsageScope::LastRequest,
            input_tokens: 21_400,
            output_tokens: 980,
            total_tokens: 26_832,
            thought_tokens: Some(452),
            cached_read_tokens: Some(4_000),
            cached_write_tokens: None,
            adapter: Some("@agentclientprotocol/codex-acp".to_string()),
            adapter_version: Some("2.1.1".to_string()),
        }),
        usage_unavailable: false,
        history_replay: Some(ConversationHistoryReplay {
            entries: 12,
            truncated: false,
        }),
        error: serde_json::json!({}),
    }
}

pub fn conversation_run_input_fixture() -> ConversationRunInput {
    ConversationRunInput {
        run_input_id: ConversationRunInputID("run_input_fixture".to_string()),
        conversation_id: ConversationID("conversation_fixture".to_string()),
        message_id: ConversationMessageID("message_user_fixture".to_string()),
        requested_against_run_id: None,
        target_run_id: Some(ConversationRunID("conversation_run_fixture".to_string())),
        kind: RunInputKind::Initial,
        retry_of_input_id: None,
        idempotency_key: "client-key-1".to_string(),
        status: RunInputStatus::Delivered,
        delivery_evidence: Some(RunInputDeliveryEvidence::AdapterConfirmed),
    }
}

fn pending_run_input_fixture() -> ConversationRunInput {
    ConversationRunInput {
        target_run_id: None,
        status: RunInputStatus::Pending,
        delivery_evidence: None,
        ..conversation_run_input_fixture()
    }
}

pub fn agent_session_summary_fixture() -> AgentSessionSummary {
    AgentSessionSummary {
        agent_session_id: AgentSessionID("agent_session_fixture".to_string()),
        conversation_id: ConversationID("conversation_fixture".to_string()),
        engine_id: EngineID("codex".to_string()),
        state: AgentSessionState::Active,
        source_scope_fingerprint:
            "c3140d13b064c644aad268b97c03c362a1a19d4591d115f4c51453016bc54b18".to_string(),
        created_at: "2026-07-03T22:00:00.400Z".to_string(),
        last_used_at: "2026-07-03T22:00:01.000Z".to_string(),
        ended_at: None,
        end_reason: None,
        current_mode_id: Some("workspace-write".to_string()),
        commands: Some(vec![ConversationSessionCommand {
            name: "review".to_string(),
            description: "Review my current changes".to_string(),
            input_hint: Some("what to review".to_string()),
        }]),
        context_usage: Some(ConversationContextUsage {
            used: 26_832,
            size: 258_400,
        }),
    }
}

pub fn conversation_detail_fixture() -> ConversationDetail {
    let conversation = conversation_summary_fixture();
    let run = conversation_run_summary_fixture();
    ConversationDetail {
        conversation: conversation.clone(),
        workspace: workspace_summary_fixture(),
        projects: vec![ConversationProjectLink {
            project_id: ProjectID("019b0000-0000-7000-8000-000000000001".to_string()),
            project_source_id: ProjectSourceID("019b0000-0000-7000-8000-000000000002".to_string()),
            role: ConversationProjectRole::Primary,
        }],
        latest_message_page: ConversationMessagePage {
            messages: vec![
                ConversationMessage {
                    message_id: ConversationMessageID("message_system_fixture".to_string()),
                    conversation_id: conversation.conversation_id.clone(),
                    role: ConversationRole::System,
                    body: "review-bot finished.".to_string(),
                    position: 1,
                    created_at: "2026-07-03T22:00:01.000Z".to_string(),
                    attachments: Vec::new(),
                    source: MessageSource::User,
                    note: Some(DelegationNote {
                        child_conversation_id: ConversationID("conversation_child_fixture".to_string()),
                        child_name: "review-bot".to_string(),
                        kind: DelegationNoteKind::Report,
                        state: DelegationNoteState::Completed,
                        excerpt: "No correctness issues in app/Package.swift.".to_string(),
                        waits_for_user: false,
                        deferred: false,
                        run_id: None,
                        stop_cause: None,
                    }),
                    task: None,
                    note_delivered_at: None,
                },
                ConversationMessage {
                    message_id: ConversationMessageID("message_user_fixture".to_string()),
                    conversation_id: conversation.conversation_id.clone(),
                    role: ConversationRole::User,
                    body: "Discuss how engine routing should work.".to_string(),
                    position: 0,
                    created_at: "2026-07-03T22:00:00.000Z".to_string(),
                    attachments: Vec::new(),
                    source: MessageSource::User,
                    note: None,
                    task: None,
                    note_delivered_at: None,
                },
            ],
            next_cursor: None,
        },
        latest_run_page: ConversationRunPage {
            runs: vec![run],
            next_cursor: None,
        },
        latest_run_input_page: ConversationRunInputPage {
            run_inputs: vec![conversation_run_input_fixture()],
            next_cursor: None,
        },
        latest_agent_session_page: AgentSessionPage {
            sessions: vec![agent_session_summary_fixture()],
            next_cursor: None,
        },
    }
}

pub fn conversation_create_request_fixture() -> ConversationCreateRequest {
    ProtocolRequest::new(
        RequestID::Number(41),
        METHOD_CONVERSATION_CREATE,
        ConversationCreateParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            working_root: WorkingRootSelection::Project {
                project_id: ProjectID("019b0000-0000-7000-8000-000000000001".to_string()),
                source_id: ProjectSourceID("019b0000-0000-7000-8000-000000000002".to_string()),
            },
            context_project_ids: Vec::new(),
            title: Some("Chat engine routing".to_string()),
            message: Some(ConversationInitialMessage {
                body: "Discuss how engine routing should work.".to_string(),
                idempotency_key: "client-key-1".to_string(),
                attachment_ids: Vec::new(),
            }),
        },
    )
}

pub fn conversation_create_response_fixture() -> ConversationCreateResponse {
    ProtocolResponse::success(
        RequestID::Number(41),
        ConversationCreateResult {
            conversation: conversation_detail_fixture(),
            initial_input: Some(pending_run_input_fixture()),
        },
    )
}

pub fn conversation_delete_request_fixture() -> ConversationDeleteRequest {
    ProtocolRequest::new(
        RequestID::Number(50),
        METHOD_CONVERSATION_DELETE,
        ConversationDeleteParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            children: ConversationDeleteChildren::Delete,
        },
    )
}

pub fn conversation_delete_response_fixture() -> ConversationDeleteResponse {
    ProtocolResponse::success(
        RequestID::Number(50),
        ConversationDeleteResult {
            conversation_id: ConversationID("conversation_fixture".to_string()),
            harness_session_cleanup: vec![HarnessSessionCleanup {
                engine_id: EngineID("codex".to_string()),
                agent_session_id: AgentSessionID("session_fixture".to_string()),
                status: HarnessSessionCleanupStatus::Deleted,
                message: None,
            }],
        },
    )
}

pub fn conversation_list_request_fixture() -> ConversationListRequest {
    ProtocolRequest::new(
        RequestID::Number(42),
        METHOD_CONVERSATION_LIST,
        ConversationListParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
        },
    )
}

pub fn conversation_list_response_fixture() -> ConversationListResponse {
    ProtocolResponse::success(
        RequestID::Number(42),
        ConversationListResult {
            conversations: vec![conversation_summary_fixture()],
        },
    )
}

pub fn conversation_get_request_fixture() -> ConversationGetRequest {
    ProtocolRequest::new(
        RequestID::Number(43),
        METHOD_CONVERSATION_GET,
        ConversationGetParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
        },
    )
}

pub fn conversation_get_response_fixture() -> ConversationGetResponse {
    ProtocolResponse::success(
        RequestID::Number(43),
        ConversationGetResult {
            conversation: conversation_detail_fixture(),
        },
    )
}

pub fn conversation_attachment_fixture() -> ConversationAttachment {
    ConversationAttachment {
        attachment_id: ConversationAttachmentID("attachment_fixture".to_string()),
        kind: ConversationAttachmentKind::Image,
        mime_type: "image/png".to_string(),
        byte_count: 20_480,
        width: 1024,
        height: 640,
        path: "/Users/example/.ottam/workspaces/019b0000-0000-7000-8000-000000000003/attachments/attachment_fixture.png".to_string(),
        source_kind: ConversationAttachmentSourceKind::Pasted,
        origin_path: None,
        created_at: "2026-07-03T22:00:02.000Z".to_string(),
    }
}

pub fn conversation_post_message_request_fixture() -> ConversationPostMessageRequest {
    ProtocolRequest::new(
        RequestID::Number(44),
        METHOD_CONVERSATION_POST_MESSAGE,
        ConversationPostMessageParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            message: "Add the UI workbench wiring.".to_string(),
            idempotency_key: "client-key-2".to_string(),
            attachment_ids: vec![ConversationAttachmentID("attachment_fixture".to_string())],
        },
    )
}

pub fn conversation_create_attachment_request_fixture() -> ConversationCreateAttachmentRequest {
    ProtocolRequest::new(
        RequestID::Number(52),
        METHOD_CONVERSATION_CREATE_ATTACHMENT,
        ConversationCreateAttachmentParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            source_path: "/var/folders/tmp/ottam-attachment-1.png".to_string(),
            mime_type: "image/png".to_string(),
            width: 1024,
            height: 640,
            source_kind: ConversationAttachmentSourceKind::Pasted,
            origin_path: None,
            idempotency_key: "client-key-attachment-1".to_string(),
        },
    )
}

pub fn conversation_create_attachment_response_fixture() -> ConversationCreateAttachmentResponse {
    ProtocolResponse::success(
        RequestID::Number(52),
        ConversationCreateAttachmentResult {
            attachment: conversation_attachment_fixture(),
        },
    )
}

pub fn conversation_discard_attachment_request_fixture() -> ConversationDiscardAttachmentRequest {
    ProtocolRequest::new(
        RequestID::Number(53),
        METHOD_CONVERSATION_DISCARD_ATTACHMENT,
        ConversationDiscardAttachmentParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            attachment_id: ConversationAttachmentID("attachment_fixture".to_string()),
        },
    )
}

pub fn conversation_discard_attachment_response_fixture() -> ConversationDiscardAttachmentResponse {
    ProtocolResponse::success(
        RequestID::Number(53),
        ConversationDiscardAttachmentResult { discarded: true },
    )
}

pub fn conversation_post_message_response_fixture() -> ConversationPostMessageResponse {
    ProtocolResponse::success(
        RequestID::Number(44),
        ConversationPostMessageResult {
            message: ConversationMessage {
                message_id: ConversationMessageID("message_user_fixture".to_string()),
                conversation_id: ConversationID("conversation_fixture".to_string()),
                role: ConversationRole::User,
                body: "Add the UI workbench wiring.".to_string(),
                position: 2,
                created_at: "2026-07-03T22:00:02.000Z".to_string(),
                attachments: vec![conversation_attachment_fixture()],
                source: MessageSource::User,
                note: None,
                task: None,
                note_delivered_at: None,
            },
            input: pending_run_input_fixture(),
        },
    )
}

pub fn conversation_start_run_request_fixture() -> ConversationStartRunRequest {
    ProtocolRequest::new(
        RequestID::Number(45),
        METHOD_CONVERSATION_START_RUN,
        ConversationStartRunParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            engine_id: EngineID("codex".to_string()),
            model_id: Some("gpt-5".to_string()),
            mode_id: Some("auto".to_string()),
            thought_level_id: Some("medium".to_string()),
            run_input_id: ConversationRunInputID("run_input_fixture".to_string()),
        },
    )
}

pub fn conversation_start_run_response_fixture() -> ConversationStartRunResponse {
    let mut run = conversation_run_summary_fixture();
    run.status = ConversationRunStatus::Running;
    run.completed_at = None;
    run.stop_reason = None;
    let mut input = conversation_run_input_fixture();
    input.status = RunInputStatus::Sending;
    input.delivery_evidence = None;
    ProtocolResponse::success(
        RequestID::Number(45),
        ConversationStartRunResult { run, input },
    )
}

pub fn conversation_cancel_run_request_fixture() -> ConversationCancelRunRequest {
    ProtocolRequest::new(
        RequestID::Number(49),
        METHOD_CONVERSATION_CANCEL_RUN,
        ConversationCancelRunParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            run_id: ConversationRunID("conversation_run_fixture".to_string()),
        },
    )
}

pub fn conversation_cancel_run_response_fixture() -> ConversationCancelRunResponse {
    let mut run = conversation_run_summary_fixture();
    run.status = ConversationRunStatus::Canceled;
    run.stop_reason = Some(AgentStopReason::Cancelled);
    ProtocolResponse::success(RequestID::Number(49), ConversationCancelRunResult { run })
}

pub fn conversation_steer_run_request_fixture() -> ConversationSteerRunRequest {
    ProtocolRequest::new(
        RequestID::Number(51),
        METHOD_CONVERSATION_STEER_RUN,
        ConversationSteerRunParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            run_id: ConversationRunID("conversation_run_fixture".to_string()),
            message: "Focus on the adapter boundary first.".to_string(),
            idempotency_key: "client-key-3".to_string(),
            attachment_ids: Vec::new(),
        },
    )
}

pub fn conversation_steer_run_response_fixture() -> ConversationSteerRunResponse {
    let mut run = conversation_run_summary_fixture();
    run.status = ConversationRunStatus::Running;
    run.completed_at = None;
    run.stop_reason = None;
    let mut input = conversation_run_input_fixture();
    input.kind = RunInputKind::Steer;
    input.requested_against_run_id = Some(run.run_id.clone());
    ProtocolResponse::success(
        RequestID::Number(51),
        ConversationSteerRunResult { input, run },
    )
}

pub fn conversation_stop_background_task_request_fixture() -> ConversationStopBackgroundTaskRequest
{
    ProtocolRequest::new(
        RequestID::Number(453),
        METHOD_CONVERSATION_STOP_BACKGROUND_TASK,
        ConversationStopBackgroundTaskParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            run_id: ConversationRunID("conversation_run_fixture".to_string()),
            async_task_id: "b7x1k2".to_string(),
        },
    )
}

pub fn conversation_stop_background_task_response_fixture() -> ConversationStopBackgroundTaskResponse
{
    ProtocolResponse::success(
        RequestID::Number(453),
        ConversationStopBackgroundTaskResult { stopped: true },
    )
}

pub fn conversation_set_mode_request_fixture() -> ConversationSetModeRequest {
    ProtocolRequest::new(
        RequestID::Number(454),
        METHOD_CONVERSATION_SET_MODE,
        ConversationSetModeParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            agent_session_id: Some(AgentSessionID("agent_session_fixture".to_string())),
            mode_id: "acceptEdits".to_string(),
        },
    )
}

pub fn conversation_set_mode_response_fixture() -> ConversationSetModeResponse {
    ProtocolResponse::success(
        RequestID::Number(454),
        ConversationSetModeResult {
            outcome: ConversationSetModeOutcome::Applied,
            current_mode_id: Some("acceptEdits".to_string()),
        },
    )
}

pub fn conversation_set_always_allow_tool_actions_request_fixture(
) -> ConversationSetAlwaysAllowToolActionsRequest {
    ProtocolRequest::new(
        RequestID::Number(455),
        METHOD_CONVERSATION_SET_ALWAYS_ALLOW_TOOL_ACTIONS,
        ConversationSetAlwaysAllowToolActionsParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            enabled: true,
        },
    )
}

pub fn conversation_set_always_allow_tool_actions_response_fixture(
) -> ConversationSetAlwaysAllowToolActionsResponse {
    ProtocolResponse::success(
        RequestID::Number(455),
        ToolPermissionsState {
            always_allow_tool_actions: true,
        },
    )
}

pub fn tool_permissions_get_request_fixture() -> ToolPermissionsGetRequest {
    ProtocolRequest::new(
        RequestID::Number(456),
        METHOD_TOOL_PERMISSIONS_GET,
        ToolPermissionsGetParams {},
    )
}

pub fn tool_permissions_get_response_fixture() -> ToolPermissionsGetResponse {
    ProtocolResponse::success(
        RequestID::Number(456),
        ToolPermissionsState {
            always_allow_tool_actions: false,
        },
    )
}

pub fn tool_permissions_set_request_fixture() -> ToolPermissionsSetRequest {
    ProtocolRequest::new(
        RequestID::Number(457),
        METHOD_TOOL_PERMISSIONS_SET,
        ToolPermissionsState {
            always_allow_tool_actions: true,
        },
    )
}

pub fn tool_permissions_set_response_fixture() -> ToolPermissionsSetResponse {
    ProtocolResponse::success(
        RequestID::Number(457),
        ToolPermissionsState {
            always_allow_tool_actions: true,
        },
    )
}

pub fn conversation_resolve_run_input_delivery_request_fixture(
) -> ConversationResolveRunInputDeliveryRequest {
    ProtocolRequest::new(
        RequestID::Number(52),
        METHOD_CONVERSATION_RESOLVE_RUN_INPUT_DELIVERY,
        ConversationResolveRunInputDeliveryParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            run_input_id: ConversationRunInputID("run_input_fixture".to_string()),
            resolution: RunInputDeliveryResolution::Retry {
                idempotency_key: "client-key-4".to_string(),
            },
        },
    )
}

pub fn conversation_resolve_run_input_delivery_response_fixture(
) -> ConversationResolveRunInputDeliveryResponse {
    let mut resolved = conversation_run_input_fixture();
    resolved.status = RunInputStatus::Superseded;
    resolved.delivery_evidence = None;
    let mut retry = pending_run_input_fixture();
    retry.run_input_id = ConversationRunInputID("run_input_retry_fixture".to_string());
    retry.kind = RunInputKind::Retry;
    retry.retry_of_input_id = Some(ConversationRunInputID("run_input_fixture".to_string()));
    retry.idempotency_key = "client-key-4".to_string();
    ProtocolResponse::success(
        RequestID::Number(52),
        ConversationResolveRunInputDeliveryResult {
            resolved_input: resolved,
            retry_input: Some(retry),
        },
    )
}

pub fn conversation_list_messages_request_fixture() -> ConversationListMessagesRequest {
    ProtocolRequest::new(
        RequestID::Number(53),
        METHOD_CONVERSATION_LIST_MESSAGES,
        ConversationListMessagesParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            limit: Some(100),
            cursor: None,
        },
    )
}

pub fn conversation_list_messages_response_fixture() -> ConversationListMessagesResponse {
    ProtocolResponse::success(
        RequestID::Number(53),
        ConversationListMessagesResult {
            page: conversation_detail_fixture().latest_message_page,
        },
    )
}

pub fn conversation_list_runs_request_fixture() -> ConversationListRunsRequest {
    ProtocolRequest::new(
        RequestID::Number(54),
        METHOD_CONVERSATION_LIST_RUNS,
        ConversationListRunsParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            limit: Some(100),
            cursor: None,
        },
    )
}

pub fn conversation_list_runs_response_fixture() -> ConversationListRunsResponse {
    let mut page = conversation_detail_fixture().latest_run_page;
    // The agent's own follow-up after the user's run: it continues the same
    // session and ends without a prompt's stop reason.
    let mut followup = conversation_run_summary_fixture();
    followup.run_id = ConversationRunID("conversation_run_autonomous_fixture".to_string());
    followup.origin = RunOrigin::Autonomous;
    followup.session_origin = Some(SessionOrigin::Continued);
    followup.started_at = "2026-07-03T22:00:05.000Z".to_string();
    followup.completed_at = Some("2026-07-03T22:00:09.000Z".to_string());
    followup.stop_reason = None;
    page.runs.push(followup);
    ProtocolResponse::success(RequestID::Number(54), ConversationListRunsResult { page })
}

pub fn conversation_list_run_inputs_request_fixture() -> ConversationListRunInputsRequest {
    ProtocolRequest::new(
        RequestID::Number(55),
        METHOD_CONVERSATION_LIST_RUN_INPUTS,
        ConversationListRunInputsParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            limit: Some(100),
            cursor: None,
        },
    )
}

pub fn conversation_list_run_inputs_response_fixture() -> ConversationListRunInputsResponse {
    ProtocolResponse::success(
        RequestID::Number(55),
        ConversationListRunInputsResult {
            page: conversation_detail_fixture().latest_run_input_page,
        },
    )
}

pub fn conversation_list_agent_sessions_request_fixture() -> ConversationListAgentSessionsRequest {
    ProtocolRequest::new(
        RequestID::Number(56),
        METHOD_CONVERSATION_LIST_AGENT_SESSIONS,
        ConversationListAgentSessionsParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            limit: Some(100),
            cursor: None,
        },
    )
}

pub fn conversation_list_agent_sessions_response_fixture() -> ConversationListAgentSessionsResponse
{
    ProtocolResponse::success(
        RequestID::Number(56),
        ConversationListAgentSessionsResult {
            page: conversation_detail_fixture().latest_agent_session_page,
        },
    )
}

pub fn conversation_list_events_request_fixture() -> ConversationListEventsRequest {
    ProtocolRequest::new(
        RequestID::Number(57),
        METHOD_CONVERSATION_LIST_EVENTS,
        ConversationListEventsParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            run_id: Some(ConversationRunID("conversation_run_fixture".to_string())),
            limit: Some(500),
            cursor: None,
        },
    )
}

pub fn conversation_list_events_response_fixture() -> ConversationListEventsResponse {
    let notification = conversation_event_delta_notification_fixture();
    ProtocolResponse::success(
        RequestID::Number(57),
        ConversationListEventsResult {
            events: vec![notification.params],
            next_cursor: None,
        },
    )
}

pub fn conversation_event_delta_notification_fixture() -> ConversationEventDeltaNotification {
    ProtocolNotification::new(
        METHOD_CONVERSATION_EVENT_DELTA,
        ConversationEventDeltaParams {
            event_id: ConversationEventID("conversation_event_fixture".to_string()),
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            project_id: Some(ProjectID(
                "019b0000-0000-7000-8000-000000000001".to_string(),
            )),
            conversation_id: ConversationID("conversation_fixture".to_string()),
            run_id: ConversationRunID("conversation_run_fixture".to_string()),
            sequence: 1,
            event_kind: ConversationEventKind::RunStatus,
            payload: serde_json::json!({"status":"running"}),
            created_at: "2026-07-03T22:00:01.000Z".to_string(),
        },
    )
}

pub fn conversation_transcript_item_fixture() -> ConversationTranscriptItem {
    ConversationTranscriptItem {
        adapter_metadata: Default::default(),
        subagent_thread_id: None,
        committed_revision: Some(1),
        run_status: None,
        item_id: "local-text-1".into(),
        conversation_id: ConversationID("conversation_fixture".into()),
        run_id: Some(ConversationRunID("conversation_run_fixture".into())),
        position: 1,
        revision: 2,
        kind: TranscriptItemKind::Reasoning,
        completion: TranscriptCompletion::Open,
        created_at: "2026-09-07T10:00:00Z".into(),
        updated_at: "2026-09-07T10:00:01Z".into(),
        provider_message_id: Some("provider-message-1".into()),
        body_preview: "Shared reasoning".into(),
        body_byte_count: 16,
        preview_omitted: false,
        message: None,
        tool: None,
        payloads: Vec::new(),
        plan: None,
        permission: None,
        gap: None,
    }
}

/// A backgrounded command: the call completed when it handed off, and its
/// task is still running.
pub fn conversation_transcript_background_tool_item_fixture() -> ConversationTranscriptItem {
    ConversationTranscriptItem {
        adapter_metadata: Default::default(),
        subagent_thread_id: None,
        committed_revision: Some(3),
        run_status: None,
        item_id: "local-tool-2".into(),
        conversation_id: ConversationID("conversation_fixture".into()),
        run_id: Some(ConversationRunID("conversation_run_fixture".into())),
        position: 2,
        revision: 3,
        kind: TranscriptItemKind::Tool,
        completion: TranscriptCompletion::Complete,
        created_at: "2026-09-07T10:00:02Z".into(),
        updated_at: "2026-09-07T10:00:03Z".into(),
        provider_message_id: None,
        body_preview: String::new(),
        body_byte_count: 0,
        preview_omitted: false,
        message: None,
        tool: Some(TranscriptTool {
            provider_tool_call_id: Some("toolu_fixture".into()),
            name: Some("Bash".into()),
            kind: Some("execute".into()),
            title: Some("npm run dev".into()),
            status: Some("completed".into()),
            arguments: serde_json::json!({"command": "npm run dev", "run_in_background": true}),
            locations: Vec::new(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            partial_metadata: false,
            payloads: Vec::new(),
            background_task: Some(TranscriptBackgroundTask {
                id: Some("task_fixture".into()),
                state: AsyncTaskState::Running,
                output_file_path: Some("/tmp/task_fixture.output".into()),
            }),
            subagent_thread: None,
            subagent_follow_up: None,
            question: None,
        }),
        payloads: Vec::new(),
        plan: None,
        permission: None,
        gap: None,
    }
}

/// A question the agent asked through a tool call, answered.
pub fn conversation_transcript_question_tool_item_fixture() -> ConversationTranscriptItem {
    ConversationTranscriptItem {
        adapter_metadata: Default::default(),
        subagent_thread_id: None,
        committed_revision: Some(2),
        run_status: None,
        item_id: "local-tool-5".into(),
        conversation_id: ConversationID("conversation_fixture".into()),
        run_id: Some(ConversationRunID("conversation_run_fixture".into())),
        position: 5,
        revision: 2,
        kind: TranscriptItemKind::Tool,
        completion: TranscriptCompletion::Complete,
        created_at: "2026-09-07T10:00:06Z".into(),
        updated_at: "2026-09-07T10:00:07Z".into(),
        provider_message_id: None,
        body_preview: String::new(),
        body_byte_count: 0,
        preview_omitted: false,
        message: None,
        tool: Some(TranscriptTool {
            provider_tool_call_id: Some("toolu_question_fixture".into()),
            name: Some("AskUserQuestion".into()),
            kind: Some("other".into()),
            title: Some("Approach".into()),
            status: Some("completed".into()),
            arguments: serde_json::json!({}),
            locations: Vec::new(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            partial_metadata: false,
            payloads: Vec::new(),
            background_task: None,
            subagent_thread: None,
            subagent_follow_up: None,
            question: Some(TranscriptQuestion {
                request_id: "019b0000-0000-7000-8000-000000000010".into(),
                state: TranscriptQuestionState::Answered,
                answers: vec![
                    TranscriptQuestionAnswer {
                        prompt: "Which approach should I take?".into(),
                        values: vec!["Refactor".into()],
                        secure: false,
                    },
                    TranscriptQuestionAnswer {
                        prompt: "Token".into(),
                        values: Vec::new(),
                        secure: true,
                    },
                ],
            }),
        }),
        payloads: Vec::new(),
        plan: None,
        permission: None,
        gap: None,
    }
}

/// A subagent's control call: the launch returned at once, and the thread
/// runs until its run ends.
pub fn conversation_transcript_subagent_control_item_fixture() -> ConversationTranscriptItem {
    ConversationTranscriptItem {
        adapter_metadata: Default::default(),
        subagent_thread_id: None,
        committed_revision: Some(2),
        run_status: None,
        item_id: "local-tool-3".into(),
        conversation_id: ConversationID("conversation_fixture".into()),
        run_id: Some(ConversationRunID("conversation_run_fixture".into())),
        position: 3,
        revision: 4,
        kind: TranscriptItemKind::Tool,
        completion: TranscriptCompletion::Complete,
        created_at: "2026-09-07T10:00:04Z".into(),
        updated_at: "2026-09-07T10:00:05Z".into(),
        provider_message_id: None,
        body_preview: String::new(),
        body_byte_count: 0,
        preview_omitted: false,
        message: None,
        tool: Some(TranscriptTool {
            provider_tool_call_id: Some("toolu_agent_fixture".into()),
            name: Some("Agent".into()),
            kind: Some("think".into()),
            title: Some("Find the parser".into()),
            status: Some("completed".into()),
            arguments: serde_json::Value::Null,
            locations: Vec::new(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            partial_metadata: false,
            payloads: Vec::new(),
            background_task: None,
            subagent_thread: Some(TranscriptSubagentThread {
                id: "subagent:toolu_agent_fixture".into(),
                parent_thread_id: None,
                name: Some("Explore".into()),
                task: Some("Find the parser".into()),
                prompt: None,
                state: SubagentThreadState::Running,
                source: SubagentThreadSource::LegacyParentId,
                asynchronous: true,
            }),
            subagent_follow_up: None,
            question: None,
        }),
        payloads: Vec::new(),
        plan: None,
        permission: None,
        gap: None,
    }
}

/// A tool call the subagent made, grouped by its thread.
pub fn conversation_transcript_subagent_child_item_fixture() -> ConversationTranscriptItem {
    ConversationTranscriptItem {
        adapter_metadata: TranscriptAdapterMetadata {
            parent_tool_call_id: Some("toolu_agent_fixture".into()),
            ..Default::default()
        },
        subagent_thread_id: Some("subagent:toolu_agent_fixture".into()),
        committed_revision: Some(1),
        run_status: None,
        item_id: "local-tool-4".into(),
        conversation_id: ConversationID("conversation_fixture".into()),
        run_id: Some(ConversationRunID("conversation_run_fixture".into())),
        position: 4,
        revision: 2,
        kind: TranscriptItemKind::Tool,
        completion: TranscriptCompletion::Complete,
        created_at: "2026-09-07T10:00:06Z".into(),
        updated_at: "2026-09-07T10:00:07Z".into(),
        provider_message_id: None,
        body_preview: String::new(),
        body_byte_count: 0,
        preview_omitted: false,
        message: None,
        tool: Some(TranscriptTool {
            provider_tool_call_id: Some("toolu_child_fixture".into()),
            name: Some("Bash".into()),
            kind: Some("execute".into()),
            title: Some("grep -rn parse src".into()),
            status: Some("completed".into()),
            arguments: serde_json::Value::Null,
            locations: Vec::new(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            partial_metadata: false,
            payloads: Vec::new(),
            background_task: None,
            subagent_thread: None,
            subagent_follow_up: None,
            question: None,
        }),
        payloads: Vec::new(),
        plan: None,
        permission: None,
        gap: None,
    }
}

pub fn conversation_list_transcript_request_fixture(
) -> ProtocolRequest<ConversationListTranscriptParams> {
    ProtocolRequest::new(
        RequestID::Number(90),
        METHOD_CONVERSATION_LIST_TRANSCRIPT,
        ConversationListTranscriptParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".into()),
            conversation_id: ConversationID("conversation_fixture".into()),
            limit: Some(50),
            cursor: None,
            subagent_thread_id: None,
        },
    )
}

pub fn conversation_list_transcript_response_fixture(
) -> ProtocolResponse<ConversationListTranscriptResult> {
    ProtocolResponse::success(
        RequestID::Number(90),
        ConversationListTranscriptResult {
            session_states: Vec::new(),
            core_instance_id: "core-fixture".into(),
            sequence: 2,
            items: vec![
                conversation_transcript_item_fixture(),
                conversation_transcript_background_tool_item_fixture(),
                conversation_transcript_subagent_control_item_fixture(),
                conversation_transcript_subagent_child_item_fixture(),
                conversation_transcript_question_tool_item_fixture(),
            ],
            next_cursor: Some("1".into()),
            capture: vec![],
            coverage: Some(TranscriptCoverage {
                start_position: 1,
                end_position: 5,
            }),
            background_tasks: vec![ConversationRunBackgroundTasks {
                run_id: ConversationRunID("conversation_run_fixture".into()),
                live: 1,
            }],
            subagent_threads: vec![ConversationRunSubagentThreads {
                run_id: ConversationRunID("conversation_run_fixture".into()),
                live: 1,
            }],
        },
    )
}

pub fn conversation_read_transcript_body_request_fixture(
) -> ProtocolRequest<ConversationReadTranscriptBodyParams> {
    ProtocolRequest::new(
        RequestID::Number(91),
        METHOD_CONVERSATION_READ_TRANSCRIPT_BODY,
        ConversationReadTranscriptBodyParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".into()),
            conversation_id: ConversationID("conversation_fixture".into()),
            item_id: "local-text-1".into(),
            revision: 2,
            offset: 0,
        },
    )
}

pub fn conversation_read_transcript_body_response_fixture(
) -> ProtocolResponse<ConversationReadTranscriptBodyResult> {
    ProtocolResponse::success(
        RequestID::Number(91),
        ConversationReadTranscriptBodyResult {
            item_id: "local-text-1".into(),
            revision: 2,
            text: "Shared reasoning".into(),
            next_offset: None,
        },
    )
}

pub fn conversation_read_transcript_payload_request_fixture(
) -> ProtocolRequest<ConversationReadTranscriptPayloadParams> {
    ProtocolRequest::new(
        RequestID::Number(92),
        METHOD_CONVERSATION_READ_TRANSCRIPT_PAYLOAD,
        ConversationReadTranscriptPayloadParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".into()),
            conversation_id: ConversationID("conversation_fixture".into()),
            payload_id: "019b0000-0000-7000-8000-000000000092".into(),
            revision: 1,
            part: 0,
            offset: 0,
            length: Some(65536),
        },
    )
}

pub fn conversation_read_transcript_payload_response_fixture(
) -> ProtocolResponse<ConversationReadTranscriptPayloadResult> {
    ProtocolResponse::success(
        RequestID::Number(92),
        ConversationReadTranscriptPayloadResult {
            payload_id: "019b0000-0000-7000-8000-000000000092".into(),
            revision: 1,
            part: 0,
            offset: 0,
            data: "b3V0cHV0".into(),
            next_offset: None,
            readable_length: 6,
            availability: "pending".into(),
            reason: None,
            unsaved: true,
        },
    )
}

pub fn conversation_transcript_delta_notification_fixture(
) -> ProtocolNotification<ConversationTranscriptDeltaParams> {
    ProtocolNotification::new(
        NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
        ConversationTranscriptDeltaParams {
            session_states: Vec::new(),
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".into()),
            conversation_id: ConversationID("conversation_fixture".into()),
            core_instance_id: "core-fixture".into(),
            sequence: 2,
            sequence_start: None,
            items: vec![
                conversation_transcript_item_fixture(),
                conversation_transcript_background_tool_item_fixture(),
                conversation_transcript_subagent_control_item_fixture(),
                conversation_transcript_subagent_child_item_fixture(),
                conversation_transcript_question_tool_item_fixture(),
            ],
            body_appends: vec![],
            capture: vec![],
            recovery_required: false,
            background_tasks: vec![ConversationRunBackgroundTasks {
                run_id: ConversationRunID("conversation_run_fixture".into()),
                live: 1,
            }],
            subagent_threads: vec![ConversationRunSubagentThreads {
                run_id: ConversationRunID("conversation_run_fixture".into()),
                live: 1,
            }],
        },
    )
}

pub fn terminal_open_request_fixture() -> TerminalOpenRequest {
    ProtocolRequest::new(
        RequestID::Number(4),
        METHOD_TERMINAL_OPEN,
        TerminalOpenParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            working_root: WorkingRootSelection::Project {
                project_id: ProjectID("019b0000-0000-7000-8000-000000000001".to_string()),
                source_id: ProjectSourceID("019b0000-0000-7000-8000-000000000002".to_string()),
            },
            rows: 24,
            cols: 80,
            cwd: None,
        },
    )
}

pub fn terminal_open_response_fixture() -> TerminalOpenResponse {
    ProtocolResponse::success(
        RequestID::Number(4),
        TerminalSessionSummary {
            terminal_id: TerminalID("019b0000-0000-7000-8000-000000000004".to_string()),
            session_id: TerminalSessionID("terminal_local_fixture".to_string()),
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            working_root: project_working_root_fixture(),
            cwd: "/Users/example/Developer/ottam".to_string(),
            shell: "/bin/zsh".to_string(),
            rows: 24,
            cols: 80,
        },
    )
}

pub fn terminal_output_delta_notification_fixture() -> TerminalOutputDeltaNotification {
    ProtocolNotification::new(
        METHOD_TERMINAL_OUTPUT_DELTA,
        TerminalOutputDeltaParams {
            session_id: TerminalSessionID("terminal_local_fixture".to_string()),
            workspace_id: Some(WorkspaceID(
                "019b0000-0000-7000-8000-000000000003".to_string(),
            )),
            terminal_id: Some(TerminalID("terminal_record_fixture".to_string())),
            sequence: 1,
            byte_start: Some(0),
            byte_end: Some(5),
            data_base64: "aGVsbG8=".to_string(),
            cap_reached: false,
            replay: false,
            delivery_state: None,
            dropped_bytes: None,
            dropped_chunks: None,
            replay_suggested: false,
        },
    )
}

fn mcp_server_summary_fixture() -> McpServerSummary {
    McpServerSummary {
        server_id: McpServerID("019b0000-0000-7000-8000-0000000000a1".to_string()),
        config: McpServerConfig {
            name: "github".to_string(),
            transport: McpServerTransport::Stdio {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                env: vec![
                    McpConfigValue {
                        name: "GITHUB_TOKEN".to_string(),
                        value: None,
                        secret: true,
                    },
                    McpConfigValue {
                        name: "GITHUB_HOST".to_string(),
                        value: Some("github.com".to_string()),
                        secret: false,
                    },
                ],
            },
        },
        disabled_workspace_ids: vec![WorkspaceID(
            "019b0000-0000-7000-8000-000000000003".to_string(),
        )],
        state: McpServerState {
            status: McpServerStatus::NotStarted,
            error: None,
        },
        signed_in: false,
    }
}

fn mcp_remote_server_summary_fixture() -> McpServerSummary {
    McpServerSummary {
        server_id: McpServerID("019b0000-0000-7000-8000-0000000000a2".to_string()),
        config: McpServerConfig {
            name: "linear".to_string(),
            transport: McpServerTransport::Http {
                url: "https://mcp.linear.app/mcp".to_string(),
                headers: Vec::new(),
            },
        },
        disabled_workspace_ids: Vec::new(),
        state: McpServerState {
            status: McpServerStatus::SignedOut,
            error: None,
        },
        signed_in: true,
    }
}

pub fn mcp_list_request_fixture() -> McpListRequest {
    ProtocolRequest::new(RequestID::Number(460), METHOD_MCP_LIST, McpListParams {})
}

pub fn mcp_list_response_fixture() -> McpListResponse {
    ProtocolResponse::success(
        RequestID::Number(460),
        McpListResult {
            servers: vec![
                mcp_server_summary_fixture(),
                mcp_remote_server_summary_fixture(),
            ],
            redirect_uri: "http://127.0.0.1:47391/oauth/callback".to_string(),
        },
    )
}

pub fn mcp_add_request_fixture() -> McpAddRequest {
    ProtocolRequest::new(
        RequestID::Number(461),
        METHOD_MCP_ADD,
        McpAddParams {
            config: McpServerConfig {
                name: "linear".to_string(),
                transport: McpServerTransport::Http {
                    url: "https://mcp.linear.app/mcp".to_string(),
                    headers: vec![McpConfigValue {
                        name: "X-Api-Key".to_string(),
                        value: Some("lin_fixture".to_string()),
                        secret: true,
                    }],
                },
            },
        },
    )
}

pub fn mcp_add_response_fixture() -> McpAddResponse {
    ProtocolResponse::success(
        RequestID::Number(461),
        McpServerResult {
            server: mcp_server_summary_fixture(),
        },
    )
}

pub fn mcp_update_request_fixture() -> McpUpdateRequest {
    let server = mcp_server_summary_fixture();
    ProtocolRequest::new(
        RequestID::Number(462),
        METHOD_MCP_UPDATE,
        McpUpdateParams {
            server_id: server.server_id,
            config: server.config,
        },
    )
}

pub fn mcp_update_response_fixture() -> McpUpdateResponse {
    ProtocolResponse::success(
        RequestID::Number(462),
        McpServerResult {
            server: mcp_server_summary_fixture(),
        },
    )
}

pub fn mcp_remove_request_fixture() -> McpRemoveRequest {
    ProtocolRequest::new(
        RequestID::Number(463),
        METHOD_MCP_REMOVE,
        McpRemoveParams {
            server_id: mcp_server_summary_fixture().server_id,
        },
    )
}

pub fn mcp_remove_response_fixture() -> McpRemoveResponse {
    ProtocolResponse::success(RequestID::Number(463), McpRemoveResult {})
}

pub fn mcp_set_workspace_enabled_request_fixture() -> McpSetWorkspaceEnabledRequest {
    ProtocolRequest::new(
        RequestID::Number(464),
        METHOD_MCP_SET_WORKSPACE_ENABLED,
        McpSetWorkspaceEnabledParams {
            server_id: mcp_server_summary_fixture().server_id,
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            enabled: false,
        },
    )
}

pub fn mcp_set_workspace_enabled_response_fixture() -> McpSetWorkspaceEnabledResponse {
    ProtocolResponse::success(
        RequestID::Number(464),
        McpServerResult {
            server: mcp_server_summary_fixture(),
        },
    )
}

pub fn mcp_server_state_changed_notification_fixture() -> McpServerStateChangedNotification {
    ProtocolNotification::new(
        METHOD_MCP_SERVER_STATE_CHANGED,
        McpServerStateChangedParams {
            server_id: mcp_server_summary_fixture().server_id,
            state: McpServerState {
                status: McpServerStatus::Failed,
                error: Some("exited with status 1".to_string()),
            },
        },
    )
}

pub fn mcp_sign_in_request_fixture() -> McpSignInRequest {
    ProtocolRequest::new(
        RequestID::Number(466),
        METHOD_MCP_SIGN_IN,
        McpSignInParams {
            server_id: mcp_remote_server_summary_fixture().server_id,
            client_id: Some("ottam-desktop".to_string()),
        },
    )
}

pub fn mcp_sign_in_response_fixture() -> McpSignInResponse {
    ProtocolResponse::success(
        RequestID::Number(466),
        McpSignInResult {
            authorization_url: "https://mcp.linear.app/authorize?response_type=code&client_id=ottam-desktop&state=abc".to_string(),
            redirect_uri: "http://127.0.0.1:47391/oauth/callback".to_string(),
        },
    )
}

pub fn mcp_cancel_sign_in_request_fixture() -> McpCancelSignInRequest {
    ProtocolRequest::new(
        RequestID::Number(467),
        METHOD_MCP_CANCEL_SIGN_IN,
        McpCancelSignInParams {
            server_id: mcp_remote_server_summary_fixture().server_id,
        },
    )
}

pub fn mcp_cancel_sign_in_response_fixture() -> McpCancelSignInResponse {
    ProtocolResponse::success(RequestID::Number(467), McpCancelSignInResult {})
}

pub fn mcp_sign_out_request_fixture() -> McpSignOutRequest {
    ProtocolRequest::new(
        RequestID::Number(468),
        METHOD_MCP_SIGN_OUT,
        McpSignOutParams {
            server_id: mcp_remote_server_summary_fixture().server_id,
        },
    )
}

pub fn mcp_sign_out_response_fixture() -> McpSignOutResponse {
    let mut server = mcp_remote_server_summary_fixture();
    server.signed_in = false;
    ProtocolResponse::success(RequestID::Number(468), McpServerResult { server })
}

pub fn mcp_sign_in_finished_notification_fixture() -> McpSignInFinishedNotification {
    ProtocolNotification::new(
        METHOD_MCP_SIGN_IN_FINISHED,
        McpSignInFinishedParams {
            server_id: mcp_remote_server_summary_fixture().server_id,
            outcome: McpSignInOutcome::Failed,
            error: Some("The sign-in was denied in the browser".to_string()),
        },
    )
}

pub fn mcp_sign_in_needed_notification_fixture() -> McpSignInNeededNotification {
    ProtocolNotification::new(
        METHOD_MCP_SIGN_IN_NEEDED,
        McpSignInNeededParams {
            server_id: mcp_remote_server_summary_fixture().server_id,
            server_name: "linear".to_string(),
            conversation_id: ConversationID("019b0000-0000-7000-8000-000000000010".to_string()),
        },
    )
}

fn named_agent_fixture() -> NamedAgent {
    NamedAgent {
        named_agent_id: NamedAgentID("019b0000-0000-7000-8000-0000000000a1".to_string()),
        name: "review-bot".to_string(),
        engine_id: EngineID("codex".to_string()),
        model_id: Some("gpt-6.1-sol".to_string()),
        mode_id: Some("read-only".to_string()),
        instructions: "Review for correctness. Report findings, most severe first.".to_string(),
        avatar_seed: Some(2_654_435_769),
        created_at: "2026-10-27T10:00:00.000Z".to_string(),
        updated_at: "2026-10-27T10:00:00.000Z".to_string(),
    }
}

fn named_agent_config_fixture() -> NamedAgentConfig {
    NamedAgentConfig {
        name: "review-bot".to_string(),
        engine_id: EngineID("codex".to_string()),
        model_id: Some("gpt-6.1-sol".to_string()),
        mode_id: Some("read-only".to_string()),
        instructions: "Review for correctness. Report findings, most severe first.".to_string(),
        avatar_seed: Some(2_654_435_769),
    }
}

pub fn named_agent_list_request_fixture() -> NamedAgentListRequest {
    ProtocolRequest::new(RequestID::Number(120), METHOD_NAMED_AGENT_LIST, NamedAgentListParams {})
}

pub fn named_agent_list_response_fixture() -> NamedAgentListResponse {
    ProtocolResponse::success(
        RequestID::Number(120),
        NamedAgentListResult {
            named_agents: vec![named_agent_fixture()],
        },
    )
}

pub fn named_agent_create_request_fixture() -> NamedAgentCreateRequest {
    ProtocolRequest::new(
        RequestID::Number(121),
        METHOD_NAMED_AGENT_CREATE,
        NamedAgentCreateParams {
            config: named_agent_config_fixture(),
        },
    )
}

pub fn named_agent_create_response_fixture() -> NamedAgentCreateResponse {
    ProtocolResponse::success(
        RequestID::Number(121),
        NamedAgentResult {
            named_agent: named_agent_fixture(),
        },
    )
}

pub fn named_agent_update_request_fixture() -> NamedAgentUpdateRequest {
    ProtocolRequest::new(
        RequestID::Number(122),
        METHOD_NAMED_AGENT_UPDATE,
        NamedAgentUpdateParams {
            named_agent_id: named_agent_fixture().named_agent_id,
            config: named_agent_config_fixture(),
        },
    )
}

pub fn named_agent_update_response_fixture() -> NamedAgentUpdateResponse {
    ProtocolResponse::success(
        RequestID::Number(122),
        NamedAgentResult {
            named_agent: named_agent_fixture(),
        },
    )
}

pub fn named_agent_delete_request_fixture() -> NamedAgentDeleteRequest {
    ProtocolRequest::new(
        RequestID::Number(123),
        METHOD_NAMED_AGENT_DELETE,
        NamedAgentDeleteParams {
            named_agent_id: named_agent_fixture().named_agent_id,
        },
    )
}

pub fn named_agent_delete_response_fixture() -> NamedAgentDeleteResponse {
    ProtocolResponse::success(RequestID::Number(123), NamedAgentDeleteResult {})
}

pub fn conversation_continue_delegation_request_fixture() -> ConversationContinueDelegationRequest
{
    ProtocolRequest::new(
        RequestID::Number(124),
        METHOD_CONVERSATION_CONTINUE_DELEGATION,
        ConversationContinueDelegationParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
        },
    )
}

pub fn conversation_continue_delegation_response_fixture() -> ConversationContinueDelegationResponse
{
    ProtocolResponse::success(
        RequestID::Number(124),
        ConversationContinueDelegationResult { run: None },
    )
}

pub fn conversation_send_to_parent_request_fixture() -> ConversationSendToParentRequest {
    ProtocolRequest::new(
        RequestID::Number(125),
        METHOD_CONVERSATION_SEND_TO_PARENT,
        ConversationSendToParentParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
            conversation_id: ConversationID("conversation_fixture".to_string()),
        },
    )
}

pub fn conversation_send_to_parent_response_fixture() -> ConversationSendToParentResponse {
    ProtocolResponse::success(
        RequestID::Number(125),
        ConversationSendToParentResult { run: None },
    )
}

pub fn conversation_list_changed_notification_fixture() -> ConversationListChangedNotification {
    ProtocolNotification::new(
        METHOD_CONVERSATION_LIST_CHANGED,
        ConversationListChangedParams {
            workspace_id: WorkspaceID("019b0000-0000-7000-8000-000000000003".to_string()),
        },
    )
}

pub fn mcp_servers_changed_notification_fixture() -> McpServersChangedNotification {
    ProtocolNotification::new(METHOD_MCP_SERVERS_CHANGED, McpServersChangedParams {})
}

pub fn mcp_import_candidates_request_fixture() -> McpImportCandidatesRequest {
    ProtocolRequest::new(
        RequestID::Number(465),
        METHOD_MCP_IMPORT_CANDIDATES,
        McpImportCandidatesParams {
            source: McpImportSource::Setup {
                harness_id: "claude".to_string(),
            },
        },
    )
}

pub fn mcp_import_candidates_response_fixture() -> McpImportCandidatesResponse {
    ProtocolResponse::success(
        RequestID::Number(465),
        McpImportCandidatesResult {
            path: "/Users/example/.claude.json".to_string(),
            candidates: vec![
                McpImportCandidate {
                    config: McpServerConfig {
                        name: "docs_search".to_string(),
                        transport: McpServerTransport::Stdio {
                            command: "uvx".to_string(),
                            args: vec!["docs-mcp".to_string()],
                            env: vec![McpConfigValue {
                                name: "DOCS_API_KEY".to_string(),
                                value: Some("${DOCS_API_KEY}".to_string()),
                                secret: true,
                            }],
                        },
                    },
                    source_name: Some("docs search".to_string()),
                    existing_server_id: None,
                    notes: vec![
                        "DOCS_API_KEY uses ${…}, which Ottam does not expand; replace it with the value"
                            .to_string(),
                    ],
                    disabled_in_source: false,
                },
                McpImportCandidate {
                    config: McpServerConfig {
                        name: "github".to_string(),
                        transport: McpServerTransport::Http {
                            url: "https://api.githubcopilot.com/mcp/".to_string(),
                            headers: Vec::new(),
                        },
                    },
                    source_name: None,
                    existing_server_id: Some(mcp_server_summary_fixture().server_id),
                    notes: Vec::new(),
                    disabled_in_source: true,
                },
            ],
            skipped: vec![McpImportSkipped {
                name: "legacy".to_string(),
                reason: "It uses SSE, which Ottam does not support".to_string(),
            }],
        },
    )
}
