#![recursion_limit = "1024"]

mod agent;
mod agent_install;
mod agent_plan_usage;
mod conversation;
mod core;
mod delegation;
mod diff;
mod engine;
mod fixtures;
mod ids;
mod mcp;
mod project;
mod runtime;
mod schema;
mod storage;
pub use storage::*;
pub mod swift;
mod terminal;
mod transcript;
mod workspace;

pub use agent::*;
pub use agent_install::*;
pub use agent_plan_usage::*;
pub use conversation::*;
pub use core::*;
pub use delegation::*;
pub use diff::*;
pub use engine::*;
pub use fixtures::*;
pub use ids::*;
pub use mcp::*;
pub use project::*;
pub use runtime::*;
pub use schema::*;
pub use terminal::*;
pub use transcript::*;
pub use workspace::*;

pub type InitializeRequest = ProtocolRequest<InitializeParams>;
pub type InitializeResponse = ProtocolResponse<InitializeResult>;
pub type InitializedNotification = ProtocolNotification<InitializedParams>;
pub type CoreExitErrorNotification = ProtocolNotification<CoreExitError>;
pub type WorkspaceListRequest = ProtocolRequest<WorkspaceListParams>;
pub type WorkspaceListResponse = ProtocolResponse<WorkspaceListResult>;
pub type WorkspaceCreateRequest = ProtocolRequest<WorkspaceCreateParams>;
pub type WorkspaceCreateResponse = ProtocolResponse<WorkspaceDetail>;
pub type WorkspaceOpenRequest = ProtocolRequest<WorkspaceOpenParams>;
pub type WorkspaceOpenResponse = ProtocolResponse<WorkspaceDetail>;
pub type WorkspaceGetRequest = ProtocolRequest<WorkspaceGetParams>;
pub type WorkspaceGetResponse = ProtocolResponse<WorkspaceDetail>;
pub type WorkspaceUpdateRequest = ProtocolRequest<WorkspaceUpdateParams>;
pub type WorkspaceUpdateResponse = ProtocolResponse<WorkspaceDetail>;
pub type WorkspaceArchiveRequest = ProtocolRequest<WorkspaceArchiveParams>;
pub type WorkspaceArchiveResponse = ProtocolResponse<WorkspaceSummary>;
pub type WorkspaceRestoreRequest = ProtocolRequest<WorkspaceRestoreParams>;
pub type WorkspaceRestoreResponse = ProtocolResponse<WorkspaceSummary>;
pub type WorkspaceAddProjectRequest = ProtocolRequest<WorkspaceAddProjectParams>;
pub type WorkspaceAddProjectResponse = ProtocolResponse<WorkspaceAddProjectResult>;
pub type WorkspaceRemoveProjectRequest = ProtocolRequest<WorkspaceRemoveProjectParams>;
pub type WorkspaceRemoveProjectResponse = ProtocolResponse<WorkspaceDetail>;
pub type ProjectOpenPathRequest = ProtocolRequest<ProjectOpenPathParams>;
pub type ProjectOpenPathResponse = ProtocolResponse<ProjectOpenPathResult>;
pub type GitDiffSnapshotRequest = ProtocolRequest<GitDiffSnapshotParams>;
pub type GitDiffSnapshotResponse = ProtocolResponse<DiffSnapshot>;
pub type EngineListRequest = ProtocolRequest<EngineListParams>;
pub type EngineListResponse = ProtocolResponse<EngineListResult>;
pub type EngineChangedNotification = ProtocolNotification<EngineChangedParams>;
pub type AgentListRequest = ProtocolRequest<AgentListParams>;
pub type AgentListResponse = ProtocolResponse<AgentListResult>;
pub type AgentHomeInfoRequest = ProtocolRequest<AgentHomeInfoParams>;
pub type AgentHomeInfoResponse = ProtocolResponse<AgentHomeInfoResult>;
pub type AgentAuthStateRequest = ProtocolRequest<AgentAuthStateParams>;
pub type AgentAuthStateResponse = ProtocolResponse<AgentAuthStateResult>;
pub type AgentSignInRequest = ProtocolRequest<AgentSignInParams>;
pub type AgentSignInResponse = ProtocolResponse<AgentSignInResult>;
pub type AgentSignInCancelRequest = ProtocolRequest<AgentSignInCancelParams>;
pub type AgentSignInCancelResponse = ProtocolResponse<AgentEmptyResult>;
pub type AgentSignInFinishRequest = ProtocolRequest<AgentSignInFinishParams>;
pub type AgentSignInFinishResponse = ProtocolResponse<AgentEmptyResult>;
pub type AgentSignInEventNotification = ProtocolNotification<AgentSignInEventParams>;
pub type AgentRequestPermissionRequest = ProtocolRequest<AgentRequestPermissionParams>;
pub type AgentRequestPermissionResponse = ProtocolResponse<AgentRequestPermissionResult>;
pub type AgentAskQuestionRequest = ProtocolRequest<AgentAskQuestionParams>;
pub type AgentAskQuestionResponse = ProtocolResponse<AgentAskQuestionResult>;
pub type CancelRequestNotification = ProtocolNotification<CancelRequestParams>;
pub type AgentInstallRequest = ProtocolRequest<AgentInstallParams>;
pub type AgentInstallResponse = ProtocolResponse<AgentInstallResult>;
pub type AgentInstallCancelRequest = ProtocolRequest<AgentInstallCancelParams>;
pub type AgentInstallCancelResponse = ProtocolResponse<AgentEmptyResult>;
pub type AgentInstallEventNotification = ProtocolNotification<AgentInstallEventParams>;
pub type AgentPlanUsageRequest = ProtocolRequest<AgentPlanUsageParams>;
pub type AgentPlanUsageResponse = ProtocolResponse<AgentPlanUsageResult>;
pub type AgentPlanUsageChangedNotification = ProtocolNotification<AgentPlanUsageChangedParams>;
pub type AgentUninstallRequest = ProtocolRequest<AgentUninstallParams>;
pub type AgentUninstallResponse = ProtocolResponse<AgentEmptyResult>;
pub type RuntimeInstallRequest = ProtocolRequest<RuntimeInstallParams>;
pub type RuntimeInstallResponse = ProtocolResponse<RuntimeInstallResult>;
pub type RuntimeInstallCancelRequest = ProtocolRequest<RuntimeInstallCancelParams>;
pub type RuntimeInstallCancelResponse = ProtocolResponse<RuntimeEmptyResult>;
pub type RuntimeInstallEventNotification = ProtocolNotification<RuntimeInstallEventParams>;
pub type ConversationCreateRequest = ProtocolRequest<ConversationCreateParams>;
pub type ConversationCreateResponse = ProtocolResponse<ConversationCreateResult>;
pub type ConversationDeleteRequest = ProtocolRequest<ConversationDeleteParams>;
pub type ConversationDeleteResponse = ProtocolResponse<ConversationDeleteResult>;
pub type ConversationListRequest = ProtocolRequest<ConversationListParams>;
pub type ConversationListResponse = ProtocolResponse<ConversationListResult>;
pub type ConversationGetRequest = ProtocolRequest<ConversationGetParams>;
pub type ConversationGetResponse = ProtocolResponse<ConversationGetResult>;
pub type ConversationListMessagesRequest = ProtocolRequest<ConversationListMessagesParams>;
pub type ConversationListMessagesResponse = ProtocolResponse<ConversationListMessagesResult>;
pub type ConversationListRunsRequest = ProtocolRequest<ConversationListRunsParams>;
pub type ConversationListRunsResponse = ProtocolResponse<ConversationListRunsResult>;
pub type ConversationListRunInputsRequest = ProtocolRequest<ConversationListRunInputsParams>;
pub type ConversationListRunInputsResponse = ProtocolResponse<ConversationListRunInputsResult>;
pub type ConversationListAgentSessionsRequest =
    ProtocolRequest<ConversationListAgentSessionsParams>;
pub type ConversationListAgentSessionsResponse =
    ProtocolResponse<ConversationListAgentSessionsResult>;
pub type ConversationListEventsRequest = ProtocolRequest<ConversationListEventsParams>;
pub type ConversationListEventsResponse = ProtocolResponse<ConversationListEventsResult>;
pub type ConversationPostMessageRequest = ProtocolRequest<ConversationPostMessageParams>;
pub type ConversationPostMessageResponse = ProtocolResponse<ConversationPostMessageResult>;
pub type ConversationCreateAttachmentRequest = ProtocolRequest<ConversationCreateAttachmentParams>;
pub type ConversationCreateAttachmentResponse =
    ProtocolResponse<ConversationCreateAttachmentResult>;
pub type ConversationDiscardAttachmentRequest =
    ProtocolRequest<ConversationDiscardAttachmentParams>;
pub type ConversationDiscardAttachmentResponse =
    ProtocolResponse<ConversationDiscardAttachmentResult>;
pub type ConversationStartRunRequest = ProtocolRequest<ConversationStartRunParams>;
pub type ConversationStartRunResponse = ProtocolResponse<ConversationStartRunResult>;
pub type ConversationCancelRunRequest = ProtocolRequest<ConversationCancelRunParams>;
pub type ConversationCancelRunResponse = ProtocolResponse<ConversationCancelRunResult>;
pub type ConversationSteerRunRequest = ProtocolRequest<ConversationSteerRunParams>;
pub type ConversationSteerRunResponse = ProtocolResponse<ConversationSteerRunResult>;
pub type ConversationStopBackgroundTaskRequest =
    ProtocolRequest<ConversationStopBackgroundTaskParams>;
pub type ConversationStopBackgroundTaskResponse =
    ProtocolResponse<ConversationStopBackgroundTaskResult>;
pub type ConversationSetModeRequest = ProtocolRequest<ConversationSetModeParams>;
pub type ConversationSetModeResponse = ProtocolResponse<ConversationSetModeResult>;
pub type ConversationSetAlwaysAllowToolActionsRequest =
    ProtocolRequest<ConversationSetAlwaysAllowToolActionsParams>;
pub type ConversationSetAlwaysAllowToolActionsResponse = ProtocolResponse<ToolPermissionsState>;
pub type ToolPermissionsGetRequest = ProtocolRequest<ToolPermissionsGetParams>;
pub type McpListRequest = ProtocolRequest<McpListParams>;
pub type McpListResponse = ProtocolResponse<McpListResult>;
pub type McpAddRequest = ProtocolRequest<McpAddParams>;
pub type McpAddResponse = ProtocolResponse<McpServerResult>;
pub type McpUpdateRequest = ProtocolRequest<McpUpdateParams>;
pub type McpUpdateResponse = ProtocolResponse<McpServerResult>;
pub type McpRemoveRequest = ProtocolRequest<McpRemoveParams>;
pub type McpRemoveResponse = ProtocolResponse<McpRemoveResult>;
pub type McpSetWorkspaceEnabledRequest = ProtocolRequest<McpSetWorkspaceEnabledParams>;
pub type McpSetWorkspaceEnabledResponse = ProtocolResponse<McpServerResult>;
pub type McpServerStateChangedNotification = ProtocolNotification<McpServerStateChangedParams>;
pub type McpImportCandidatesRequest = ProtocolRequest<McpImportCandidatesParams>;
pub type McpImportCandidatesResponse = ProtocolResponse<McpImportCandidatesResult>;
pub type McpSignInRequest = ProtocolRequest<McpSignInParams>;
pub type McpSignInResponse = ProtocolResponse<McpSignInResult>;
pub type McpCancelSignInRequest = ProtocolRequest<McpCancelSignInParams>;
pub type McpCancelSignInResponse = ProtocolResponse<McpCancelSignInResult>;
pub type McpSignOutRequest = ProtocolRequest<McpSignOutParams>;
pub type McpSignOutResponse = ProtocolResponse<McpServerResult>;
pub type McpSignInFinishedNotification = ProtocolNotification<McpSignInFinishedParams>;
pub type McpSignInNeededNotification = ProtocolNotification<McpSignInNeededParams>;
pub type McpServersChangedNotification = ProtocolNotification<McpServersChangedParams>;
pub type NamedAgentListRequest = ProtocolRequest<NamedAgentListParams>;
pub type NamedAgentListResponse = ProtocolResponse<NamedAgentListResult>;
pub type NamedAgentCreateRequest = ProtocolRequest<NamedAgentCreateParams>;
pub type NamedAgentCreateResponse = ProtocolResponse<NamedAgentResult>;
pub type NamedAgentUpdateRequest = ProtocolRequest<NamedAgentUpdateParams>;
pub type NamedAgentUpdateResponse = ProtocolResponse<NamedAgentResult>;
pub type NamedAgentDeleteRequest = ProtocolRequest<NamedAgentDeleteParams>;
pub type NamedAgentDeleteResponse = ProtocolResponse<NamedAgentDeleteResult>;
pub type ConversationContinueDelegationRequest =
    ProtocolRequest<ConversationContinueDelegationParams>;
pub type ConversationContinueDelegationResponse =
    ProtocolResponse<ConversationContinueDelegationResult>;
pub type ConversationSendToParentRequest = ProtocolRequest<ConversationSendToParentParams>;
pub type ConversationSendToParentResponse = ProtocolResponse<ConversationSendToParentResult>;
pub type ConversationListChangedNotification = ProtocolNotification<ConversationListChangedParams>;
pub type ToolPermissionsGetResponse = ProtocolResponse<ToolPermissionsState>;
pub type ToolPermissionsSetRequest = ProtocolRequest<ToolPermissionsState>;
pub type ToolPermissionsSetResponse = ProtocolResponse<ToolPermissionsState>;
pub type ConversationResolveRunInputDeliveryRequest =
    ProtocolRequest<ConversationResolveRunInputDeliveryParams>;
pub type ConversationResolveRunInputDeliveryResponse =
    ProtocolResponse<ConversationResolveRunInputDeliveryResult>;
pub type ConversationEventDeltaNotification = ProtocolNotification<ConversationEventDeltaParams>;
pub type TerminalOpenRequest = ProtocolRequest<TerminalOpenParams>;
pub type TerminalOpenResponse = ProtocolResponse<TerminalSessionSummary>;
pub type TerminalWriteRequest = ProtocolRequest<TerminalWriteParams>;
pub type TerminalResizeRequest = ProtocolRequest<TerminalResizeParams>;
pub type TerminalCloseRequest = ProtocolRequest<TerminalCloseParams>;
pub type TerminalListRequest = ProtocolRequest<TerminalListParams>;
pub type TerminalDeleteRequest = ProtocolRequest<TerminalDeleteParams>;
pub type TerminalUpdateMetadataRequest = ProtocolRequest<TerminalUpdateMetadataParams>;
pub type TerminalReplayRequest = ProtocolRequest<TerminalReplayParams>;
pub type TerminalOutputDeltaNotification = ProtocolNotification<TerminalOutputDeltaParams>;
pub type TerminalExitedNotification = ProtocolNotification<TerminalExitedParams>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_omits_jsonrpc_but_decodes_legacy_field() {
        let request = initialize_request_fixture();
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(!encoded.contains("jsonrpc"));

        let decoded: InitializeRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"OttamApp","version":"0.1.0"},"capabilities":{"experimentalMethods":[]}}}"#,
        )
        .unwrap();
        assert_eq!(decoded.jsonrpc.as_deref(), Some("2.0"));
        assert_eq!(decoded.method, METHOD_INITIALIZE);
    }

    #[test]
    fn response_round_trips_initialize_result() {
        let response = initialize_response_fixture();
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: InitializeResponse = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn response_rejects_both_result_and_error() {
        let malformed = r#"{"id":1,"result":{"coreInstanceID":"019b0000-0000-7000-8000-000000000000","coreVersion":"0.1.0","serverInfo":{"name":"ottam-core","version":"0.1.0"}},"error":{"code":-32000,"message":"bad"}}"#;
        let error = serde_json::from_str::<InitializeResponse>(malformed).unwrap_err();
        assert!(error.to_string().contains("both result and error"));
    }

    #[test]
    fn response_rejects_missing_result_and_error() {
        let malformed = r#"{"id":1}"#;
        let error = serde_json::from_str::<InitializeResponse>(malformed).unwrap_err();
        assert!(error.to_string().contains("exactly one"));
    }

    #[test]
    fn initialized_notification_has_no_id() {
        let notification = initialized_notification_fixture();
        let encoded = serde_json::to_string(&notification).unwrap();
        assert!(!encoded.contains(r#""id""#));
        let decoded: InitializedNotification = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.method, METHOD_INITIALIZED);
    }

    #[test]
    fn project_summary_uses_project_id_acronym() {
        let summary = ProjectSummary {
            project_id: ProjectID("019b0000-0000-7000-8000-000000000001".to_string()),
            display_name: "ottam".to_string(),
            created_at: 1,
            updated_at: 1,
            last_opened_at: 1,
            archived_at: None,
        };
        let encoded = serde_json::to_string(&summary).unwrap();
        assert!(encoded.contains("projectID"));
    }

    #[test]
    fn git_diff_snapshot_uses_project_source_id_acronym_field() {
        let request = git_diff_snapshot_request_fixture();
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("projectSourceID"));
        assert!(!encoded.contains("projectSourceId"));
    }

    #[test]
    fn terminal_protocol_uses_session_id_acronym_and_base64_fields() {
        let request = terminal_open_request_fixture();
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("projectID"));
        assert!(encoded.contains("terminal.open"));

        let response = terminal_open_response_fixture();
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(encoded.contains("sessionID"));
        assert!(!encoded.contains("sessionId"));

        let notification = terminal_output_delta_notification_fixture();
        let encoded = serde_json::to_string(&notification).unwrap();
        assert!(encoded.contains("terminal.outputDelta"));
        assert!(encoded.contains("dataBase64"));
    }
}
