import OttamBackendClient
import OttamCore
import Foundation

/// Systematic protocol gate: every wired fixture in
/// `backend/crates/ottam-protocol/fixtures` decodes into its Swift type and
/// (unless noted) round-trips back to the same JSON. Comparison is on parsed
/// `JSONValue`, never bytes, so key order and number formatting don't matter;
/// a fixture `null` and an omitted key are treated as equivalent, matching
/// serde `Option` ↔ Swift `Optional` wire semantics.
struct FixtureGateChecks {
    /// How a fixture's payload is verified after decoding.
    enum Mode {
        /// Re-encode and require JSON equality with the fixture payload.
        case exact
        /// Re-encode; the encoded value may add exactly these keys (tolerant
        /// decoders materialize compat defaults that older fixtures omit).
        case supersetKeys(Set<String>)
        /// Decoding succeeding is the whole check (deliberate leniencies make
        /// re-encoding lossy).
        case decodeOnly
    }

    struct FixtureCase {
        var file: String
        var run: (Data) throws -> Void
    }

    func run() throws {
        try schemaDefaultsDecodeWhenOmitted()
        let cases = allCases()
        try fixtureDirectoryIsFullyCovered(by: cases)
        for fixtureCase in cases {
            let data = try fixtureData(fixtureCase.file)
            do {
                try fixtureCase.run(data)
            } catch {
                throw GateFailure("\(fixtureCase.file): \(error)")
            }
        }
    }

    private func schemaDefaultsDecodeWhenOmitted() throws {
        let capabilities = try decode(Capabilities.self, "{}")
        try require(
            capabilities.experimentalMethods.isEmpty,
            "Capabilities should materialize schema defaults"
        )

        let adapter = try decode(
            AgentAdapterStatus.self,
            """
            {
                "lifecycle": "ready",
                "authState": "ready",
                "generation": 1
            }
            """
        )
        try require(adapter.authMethods.isEmpty, "AgentAdapterStatus.authMethods should default to []")
        try require(
            !adapter.credentialsSeen,
            "AgentAdapterStatus.credentialsSeen should default to false"
        )

        let authMethod = try decode(
            AgentAuthMethod.self,
            """
            {
                "methodID": "chatgpt",
                "name": "Sign in with ChatGPT"
            }
            """
        )
        try require(authMethod.kind == .agent, "AgentAuthMethod.kind should default to agent")

        let signInEvent = try decode(
            AgentSignInEventParams.self,
            """
            {
                "agentID": "codex",
                "authSessionID": "agent_auth_fixture",
                "delivery": "adapterBrowser",
                "phase": "awaitingUser"
            }
            """
        )
        try require(
            !signInEvent.revealTerminal && signInEvent.terminal == nil,
            "AgentSignInEventParams should not reveal a terminal it was never told about"
        )

        let run = try decode(
            ConversationRunSummary.self,
            """
            {
                "runID": "conversation_run_fixture",
                "conversationID": "conversation_fixture",
                "engineID": "codex",
                "status": "completed",
                "startedAt": "2026-07-03T22:00:00.500Z"
            }
            """
        )
        try require(
            run.origin == .user,
            "ConversationRunSummary.origin should default to user for runs that predate autonomous runs"
        )

        let message = try decode(
            ConversationMessage.self,
            """
            {
                "messageID": "message_fixture",
                "conversationID": "conversation_fixture",
                "role": "user",
                "body": "Submitted input",
                "position": 3,
                "createdAt": "2026-07-17T00:00:00Z"
            }
            """
        )
        try require(message.position == 3, "ConversationMessage.position should decode")
        try require(
            message.attachments.isEmpty,
            "ConversationMessage.attachments should default to [] for messages that predate attachments"
        )

        let postMessage = try decode(
            ConversationPostMessageParams.self,
            """
            {
                "workspaceID": "workspace_fixture",
                "conversationID": "conversation_fixture",
                "message": "No attachments here",
                "idempotencyKey": "client-key"
            }
            """
        )
        try require(
            postMessage.attachmentIDs.isEmpty,
            "ConversationPostMessageParams.attachmentIDs should default to []"
        )

        let summary = try decode(
            ConversationSummary.self,
            """
            {
                "conversationID": "conversation_fixture",
                "workspaceID": "workspace_fixture",
                "workingRoot": {
                    "kind": "workspaceFiles",
                    "displayName": "Workspace files",
                    "canonicalPath": "/tmp/ottam",
                    "availability": "available"
                },
                "title": "Defaults",
                "createdAt": "2026-07-17T00:00:00Z",
                "updatedAt": "2026-07-17T00:00:00Z"
            }
            """
        )
        try require(summary.title == "Defaults", "ConversationSummary should decode retained fields")

        let event = try decode(
            ConversationEventDeltaParams.self,
            """
            {
                "eventID": "event_fixture",
                "workspaceID": "workspace_fixture",
                "projectID": "project_fixture",
                "conversationID": "conversation_fixture",
                "runID": "run_fixture",
                "sequence": 1,
                "eventKind": "runStatus",
                "createdAt": "2026-07-17T00:00:00Z"
            }
            """
        )
        try require(event.payload == .null, "ConversationEventDeltaParams.payload should default to null")

    }

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    private func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        guard condition() else {
            throw GateFailure(message)
        }
    }

    private func allCases() -> [FixtureCase] {
        [
            // Lifecycle
            request("initialize-request.json", InitializeParams.self),
            response("initialize-response.json", InitializeResult.self),
            errorResponse("protocol-error-response.json"),
            notification("initialized-notification.json", InitializedParams.self),
            notification("core-exit-error-notification.json", CoreExitError.self),
            // Workspaces
            request("workspace-list-request.json", WorkspaceListParams.self),
            response("workspace-list-response.json", WorkspaceListResult.self),
            request("workspace-create-request.json", WorkspaceCreateParams.self),
            response("workspace-create-response.json", WorkspaceDetail.self),
            request("workspace-open-request.json", WorkspaceIDParams.self),
            response("workspace-open-response.json", WorkspaceDetail.self),
            request("workspace-get-request.json", WorkspaceIDParams.self),
            response("workspace-get-response.json", WorkspaceDetail.self),
            request("workspace-update-request.json", WorkspaceUpdateParams.self),
            response("workspace-update-response.json", WorkspaceDetail.self),
            request("workspace-archive-request.json", WorkspaceIDParams.self),
            response("workspace-archive-response.json", WorkspaceSummary.self),
            request("workspace-restore-request.json", WorkspaceIDParams.self),
            response("workspace-restore-response.json", WorkspaceSummary.self),
            request("workspace-add-project-request.json", WorkspaceAddProjectParams.self),
            response("workspace-add-project-response.json", WorkspaceAddProjectResult.self),
            request("workspace-remove-project-request.json", WorkspaceRemoveProjectParams.self),
            response("workspace-remove-project-response.json", WorkspaceDetail.self),
            // Project + git
            request("project-open-path-request.json", ProjectOpenPathParams.self),
            response("project-open-path-response.json", ProjectOpenPathResult.self),
            request("git-diff-snapshot-request.json", GitDiffSnapshotParams.self),
            response("git-diff-snapshot-response.json", CoreDiffSnapshot.self),
            // Engines + agents
            request("engine-list-request.json", EngineListParams.self),
            response("engine-list-response.json", EngineListResult.self),
            notification("engine-changed-notification.json", EngineChangedParams.self),
            request("agent-list-request.json", AgentListParams.self),
            request("agent-detect-setups-request.json", AgentListParams.self),
            response("agent-detect-setups-response.json", AgentDetectSetupsResult.self),
            request("agent-create-request.json", AgentCreateParams.self),
            request("agent-create-managed-request.json", AgentCreateParams.self),
            request("agent-rename-request.json", AgentRenameParams.self),
            request("agent-delete-request.json", AgentDeleteParams.self),
            response("agent-create-response.json", AgentInstanceResult.self),
            request("agent-set-layout-request.json", AgentSetLayoutParams.self),
            response("agent-set-layout-response.json", AgentInstanceResult.self),
            request("agent-shutdown-readiness-request.json", AgentListParams.self),
            response("agent-shutdown-readiness-response.json", AgentShutdownReadinessResult.self),
            request("agent-prepare-shutdown-request.json", AgentShutdownOperationParams.self),
            response("agent-prepare-shutdown-response.json", AgentEmptyResult.self),
            request("agent-cancel-shutdown-request.json", AgentShutdownOperationParams.self),
            response("agent-cancel-shutdown-response.json", AgentEmptyResult.self),
            request("storage-status-request.json", StorageUpdateParams.self),
            request("storage-update-request.json", StorageUpdateParams.self),
            request("storage-clear-cache-request.json", StorageUpdateParams.self),
            response("storage-status-response.json", StorageStatusResult.self),
            response("agent-list-response.json", AgentListResult.self),
            request("agent-home-info-request.json", AgentHomeInfoParams.self),
            response("agent-home-info-response.json", AgentHomeInfoResult.self),
            request("agent-auth-state-request.json", AgentAuthStateParams.self),
            response("agent-auth-state-response.json", AgentAuthStateResult.self),
            request("agent-sign-in-request.json", AgentSignInParams.self),
            response("agent-sign-in-response.json", AgentSignInResult.self),
            request("agent-sign-in-cancel-request.json", AgentSignInCancelParams.self),
            response("agent-sign-in-cancel-response.json", AgentEmptyResult.self),
            request("agent-sign-in-finish-request.json", AgentSignInFinishParams.self),
            response("agent-sign-in-finish-response.json", AgentEmptyResult.self),
            notification("agent-sign-in-event-notification.json", AgentSignInEventParams.self),
            request("agent-request-permission-request.json", AgentRequestPermissionParams.self),
            response("agent-request-permission-response.json", AgentPermissionOutcome.self),
            request("agent-ask-question-request.json", AgentAskQuestionParams.self),
            response("agent-ask-question-response.json", AgentQuestionOutcome.self),
            notification("cancel-request-notification.json", CancelRequestParams.self),
            // Managed adapter installs
            request("agent-install-request.json", AgentInstallParams.self),
            response("agent-install-response.json", AgentInstallResult.self),
            request("agent-install-cancel-request.json", AgentInstallCancelParams.self),
            response("agent-install-cancel-response.json", AgentEmptyResult.self),
            notification("agent-install-event-notification.json", AgentInstallEventParams.self),
            request("agent-uninstall-request.json", AgentUninstallParams.self),
            response("agent-uninstall-response.json", AgentEmptyResult.self),
            // Subscription plan usage
            request("agent-plan-usage-request.json", AgentPlanUsageParams.self),
            response("agent-plan-usage-response.json", AgentPlanUsageResult.self),
            notification("agent-plan-usage-changed-notification.json", AgentPlanUsageChangedParams.self),
            // Managed Node runtime
            request("runtime-install-request.json", RuntimeInstallParams.self),
            response("runtime-install-response.json", RuntimeInstallResult.self),
            request("runtime-install-cancel-request.json", RuntimeInstallCancelParams.self),
            response("runtime-install-cancel-response.json", RuntimeEmptyResult.self),
            notification("runtime-install-event-notification.json", RuntimeInstallEventParams.self),
            // Conversations
            request("conversation-create-request.json", ConversationCreateParams.self),
            response("conversation-create-response.json", ConversationCreateResult.self),
            request("conversation-delete-request.json", ConversationDeleteParams.self),
            response("conversation-delete-response.json", ConversationDeleteResult.self),
            request("conversation-list-request.json", ConversationListParams.self),
            response("conversation-list-response.json", ConversationListResult.self),
            request("conversation-get-request.json", ConversationGetParams.self),
            response("conversation-get-response.json", ConversationGetResult.self),
            request("conversation-post-message-request.json", ConversationPostMessageParams.self),
            response("conversation-post-message-response.json", ConversationPostMessageResult.self),
            request(
                "conversation-create-attachment-request.json",
                ConversationCreateAttachmentParams.self
            ),
            response(
                "conversation-create-attachment-response.json",
                ConversationCreateAttachmentResult.self
            ),
            request(
                "conversation-discard-attachment-request.json",
                ConversationDiscardAttachmentParams.self
            ),
            response(
                "conversation-discard-attachment-response.json",
                ConversationDiscardAttachmentResult.self
            ),
            request("conversation-start-run-request.json", ConversationStartRunParams.self),
            response("conversation-start-run-response.json", ConversationStartRunResult.self),
            request("conversation-cancel-run-request.json", ConversationCancelRunParams.self),
            response("conversation-cancel-run-response.json", ConversationCancelRunResult.self),
            request("conversation-steer-run-request.json", ConversationSteerRunParams.self),
            response("conversation-steer-run-response.json", ConversationSteerRunResult.self),
            request("conversation-set-mode-request.json", ConversationSetModeParams.self),
            response("conversation-set-mode-response.json", ConversationSetModeResult.self),
            request(
                "conversation-set-always-allow-tool-actions-request.json",
                ConversationSetAlwaysAllowToolActionsParams.self
            ),
            response(
                "conversation-set-always-allow-tool-actions-response.json",
                ToolPermissionsState.self
            ),
            request("tool-permissions-get-request.json", ToolPermissionsGetParams.self),
            response("tool-permissions-get-response.json", ToolPermissionsState.self),
            request("tool-permissions-set-request.json", ToolPermissionsState.self),
            response("tool-permissions-set-response.json", ToolPermissionsState.self),
            request(
                "conversation-stop-background-task-request.json",
                ConversationStopBackgroundTaskParams.self
            ),
            response(
                "conversation-stop-background-task-response.json",
                ConversationStopBackgroundTaskResult.self
            ),
            request("mcp-list-request.json", McpListParams.self),
            response("mcp-list-response.json", McpListResult.self),
            request("mcp-add-request.json", McpAddParams.self),
            response("mcp-add-response.json", McpServerResult.self),
            request("mcp-update-request.json", McpUpdateParams.self),
            response("mcp-update-response.json", McpServerResult.self),
            request("mcp-remove-request.json", McpRemoveParams.self),
            response("mcp-remove-response.json", McpRemoveResult.self),
            request("mcp-set-workspace-enabled-request.json", McpSetWorkspaceEnabledParams.self),
            response("mcp-set-workspace-enabled-response.json", McpServerResult.self),
            request("mcp-import-candidates-request.json", McpImportCandidatesParams.self),
            // Core omits `disabledInSource` when false; the decoder fills it in.
            response(
                "mcp-import-candidates-response.json",
                McpImportCandidatesResult.self,
                mode: .supersetKeys(["disabledInSource"])
            ),
            notification(
                "mcp-server-state-changed-notification.json",
                McpServerStateChangedParams.self
            ),
            request("mcp-sign-in-request.json", McpSignInParams.self),
            response("mcp-sign-in-response.json", McpSignInResult.self),
            request("mcp-cancel-sign-in-request.json", McpCancelSignInParams.self),
            response("mcp-cancel-sign-in-response.json", McpCancelSignInResult.self),
            request("mcp-sign-out-request.json", McpSignOutParams.self),
            response("mcp-sign-out-response.json", McpServerResult.self),
            notification("mcp-sign-in-finished-notification.json", McpSignInFinishedParams.self),
            notification("mcp-sign-in-needed-notification.json", McpSignInNeededParams.self),
            notification("mcp-servers-changed-notification.json", McpServersChangedParams.self),
            request("named-agent-list-request.json", NamedAgentListParams.self),
            response("named-agent-list-response.json", NamedAgentListResult.self),
            request("named-agent-create-request.json", NamedAgentCreateParams.self),
            response("named-agent-create-response.json", NamedAgentResult.self),
            request("named-agent-update-request.json", NamedAgentUpdateParams.self),
            response("named-agent-update-response.json", NamedAgentResult.self),
            request("named-agent-delete-request.json", NamedAgentDeleteParams.self),
            response("named-agent-delete-response.json", NamedAgentDeleteResult.self),
            request(
                "conversation-continue-delegation-request.json",
                ConversationContinueDelegationParams.self
            ),
            response(
                "conversation-continue-delegation-response.json",
                ConversationContinueDelegationResult.self
            ),
            request("conversation-send-to-parent-request.json", ConversationSendToParentParams.self),
            response("conversation-send-to-parent-response.json", ConversationSendToParentResult.self),
            notification("conversation-list-changed-notification.json", ConversationListChangedParams.self),
            request(
                "conversation-resolve-run-input-delivery-request.json",
                ConversationResolveRunInputDeliveryParams.self
            ),
            response(
                "conversation-resolve-run-input-delivery-response.json",
                ConversationResolveRunInputDeliveryResult.self
            ),
            request("conversation-list-messages-request.json", ConversationListMessagesParams.self),
            response("conversation-list-messages-response.json", ConversationListMessagesResult.self),
            request("conversation-list-runs-request.json", ConversationListRunsParams.self),
            response("conversation-list-runs-response.json", ConversationListRunsResult.self),
            request("conversation-list-run-inputs-request.json", ConversationListRunInputsParams.self),
            response("conversation-list-run-inputs-response.json", ConversationListRunInputsResult.self),
            request(
                "conversation-list-agent-sessions-request.json",
                ConversationListAgentSessionsParams.self
            ),
            response(
                "conversation-list-agent-sessions-response.json",
                ConversationListAgentSessionsResult.self
            ),
            request("conversation-list-transcript-request.json", ConversationListTranscriptParams.self),
            response("conversation-list-transcript-response.json", ConversationListTranscriptResult.self),
            request("conversation-read-transcript-body-request.json", ConversationReadTranscriptBodyParams.self),
            response("conversation-read-transcript-body-response.json", ConversationReadTranscriptBodyResult.self),
            request("conversation-read-transcript-payload-request.json", ConversationReadTranscriptPayloadParams.self),
            response("conversation-read-transcript-payload-response.json", ConversationReadTranscriptPayloadResult.self),
            notification("conversation-transcript-delta-notification.json", ConversationTranscriptDeltaParams.self),
            request("conversation-list-events-request.json", ConversationListEventsParams.self),
            response("conversation-list-events-response.json", ConversationListEventsResult.self),
            notification("conversation-event-delta-notification.json", ConversationEventDeltaParams.self),
            // Terminals
            request("terminal-open-request.json", TerminalOpenParams.self),
            response("terminal-open-response.json", TerminalSessionSummary.self),
            // The tolerant decoder materializes `replaySuggested`, which the
            // fixture (like an older core) omits.
            notification(
                "terminal-output-delta-notification.json",
                TerminalOutputDeltaParams.self,
                mode: .supersetKeys(["replaySuggested"])
            ),
        ]
    }

    // MARK: - Case factories

    private func request<P: Codable>(_ file: String, _ type: P.Type, mode: Mode = .exact) -> FixtureCase {
        FixtureCase(file: file) { data in
            let envelope = try JSONDecoder().decode(RequestFixtureEnvelope<P>.self, from: data)
            try verifyRoundTrip(envelope.params, against: data, key: "params", mode: mode)
        }
    }

    private func notification<P: Codable>(_ file: String, _ type: P.Type, mode: Mode = .exact) -> FixtureCase {
        FixtureCase(file: file) { data in
            let envelope = try JSONDecoder().decode(ProtocolNotification<P>.self, from: data)
            try verifyRoundTrip(envelope.params, against: data, key: "params", mode: mode)
        }
    }

    private func response<R: Codable>(_ file: String, _ type: R.Type, mode: Mode = .exact) -> FixtureCase {
        FixtureCase(file: file) { data in
            let envelope = try JSONDecoder().decode(ProtocolResponse<R>.self, from: data)
            guard let result = envelope.result else {
                throw GateFailure("response fixture carries an error, not a result")
            }
            try verifyRoundTrip(result, against: data, key: "result", mode: mode)
        }
    }

    private func errorResponse(_ file: String) -> FixtureCase {
        FixtureCase(file: file) { data in
            let envelope = try JSONDecoder().decode(ProtocolResponse<JSONValue>.self, from: data)
            guard let error = envelope.error else {
                throw GateFailure("error fixture carries a result, not an error")
            }
            try verifyRoundTrip(error, against: data, key: "error", mode: .exact)
        }
    }

    // MARK: - Round-trip comparison

    private func verifyRoundTrip<P: Encodable>(
        _ payload: P,
        against data: Data,
        key: String,
        mode: Mode
    ) throws {
        let allowedExtraKeys: Set<String>
        switch mode {
        case .decodeOnly:
            return
        case .exact:
            allowedExtraKeys = []
        case .supersetKeys(let keys):
            allowedExtraKeys = keys
        }
        guard case .object(let fixtureObject) = try JSONDecoder().decode(JSONValue.self, from: data),
              let fixtureValue = fixtureObject[key] else {
            throw GateFailure("fixture has no `\(key)` payload")
        }
        let encoded = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(payload))
        try compare(fixtureValue, encoded, allowedExtraKeys: allowedExtraKeys, path: key)
    }

    private func compare(
        _ fixture: JSONValue,
        _ encoded: JSONValue,
        allowedExtraKeys: Set<String>,
        path: String
    ) throws {
        switch (fixture, encoded) {
        case (.object(let fixtureObject), .object(let encodedObject)):
            for (key, fixtureValue) in fixtureObject {
                guard let encodedValue = encodedObject[key] else {
                    if fixtureValue == .null { continue }
                    throw GateFailure("`\(path).\(key)` missing from re-encoded value")
                }
                try compare(fixtureValue, encodedValue, allowedExtraKeys: allowedExtraKeys, path: "\(path).\(key)")
            }
            for (key, encodedValue) in encodedObject where fixtureObject[key] == nil {
                if encodedValue == .null { continue }
                guard allowedExtraKeys.contains(key) else {
                    throw GateFailure("`\(path).\(key)` unexpectedly added by re-encoding")
                }
            }
        case (.array(let fixtureItems), .array(let encodedItems)):
            guard fixtureItems.count == encodedItems.count else {
                throw GateFailure("`\(path)` array length \(fixtureItems.count) != \(encodedItems.count)")
            }
            for (index, pair) in zip(fixtureItems, encodedItems).enumerated() {
                try compare(pair.0, pair.1, allowedExtraKeys: allowedExtraKeys, path: "\(path)[\(index)]")
            }
        default:
            guard fixture == encoded else {
                throw GateFailure("`\(path)` mismatch: fixture \(fixture) vs re-encoded \(encoded)")
            }
        }
    }

    // MARK: - Fixture directory

    /// Every fixture file must have a table entry — a newly wired fixture
    /// cannot land without a Swift-side decode gate.
    private func fixtureDirectoryIsFullyCovered(by cases: [FixtureCase]) throws {
        let files = try FileManager.default
            .contentsOfDirectory(atPath: fixturesDirectory().path)
            .filter { $0.hasSuffix(".json") }
            .sorted()
        let covered = Set(cases.map(\.file))
        let uncovered = files.filter { !covered.contains($0) }
        guard uncovered.isEmpty else {
            throw GateFailure("fixtures missing from the gate table: \(uncovered.joined(separator: ", "))")
        }
    }

    private func fixturesDirectory() throws -> URL {
        try gateRepositoryRoot().appendingPathComponent("backend/crates/ottam-protocol/fixtures")
    }

    private func fixtureData(_ name: String) throws -> Data {
        try Data(contentsOf: fixturesDirectory().appendingPathComponent(name))
    }
}

private struct RequestFixtureEnvelope<P: Decodable>: Decodable {
    var id: RequestID
    var method: String
    var params: P
}

private struct GateFailure: Error, CustomStringConvertible {
    var description: String

    init(_ description: String) {
        self.description = description
    }
}

private func gateRepositoryRoot() throws -> URL {
    if let configured = ProcessInfo.processInfo.environment["OTTAM_REPO_ROOT"],
       !configured.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        return URL(fileURLWithPath: configured).standardized
    }

    var candidate = URL(fileURLWithPath: FileManager.default.currentDirectoryPath).standardized
    while true {
        let package = candidate.appendingPathComponent("app/Package.swift")
        let backend = candidate.appendingPathComponent("backend/Cargo.toml")
        if FileManager.default.fileExists(atPath: package.path),
           FileManager.default.fileExists(atPath: backend.path) {
            return candidate
        }

        let parent = candidate.deletingLastPathComponent()
        if parent.path == candidate.path {
            throw GateFailure("could not locate repository root")
        }
        candidate = parent
    }
}
