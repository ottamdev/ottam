import OttamCore
import Foundation

public protocol AgentConnection: Sendable {
    func request<Params: Encodable & Sendable, Result: Decodable & Sendable>(
        method: String,
        params: Params
    ) async throws -> Result

    func notify<Params: Codable & Sendable>(
        method: String,
        params: Params
    ) async throws

    func setNotificationHandler(_ handler: (@Sendable (AgentNotification) async -> Void)?) async

    func setClientRequestHandler(_ handler: AgentClientRequestHandler?) async

    func close() async
}

public struct AgentClientRequest: Sendable {
    public var id: RequestID
    public var method: String
    public var payload: Data

    public init(id: RequestID, method: String, payload: Data) {
        self.id = id
        self.method = method
        self.payload = payload
    }
}

public enum AgentClientResponse: Sendable {
    case success(JSONValue)
    case failure(AgentProtocolError)
}

public typealias AgentClientRequestHandler = @Sendable (AgentClientRequest) async -> AgentClientResponse
public typealias AgentPermissionHandler = @Sendable (AgentRequestPermissionParams) async -> AgentPermissionOutcome
/// Answers an agent's questions. Core withdrawing the request cancels the
/// calling task; the handler then returns `.cancelled`.
public typealias AgentQuestionHandler = @Sendable (AgentAskQuestionParams) async -> AgentQuestionOutcome

public struct AgentPermissionRegistrationToken: Hashable, Sendable {
    fileprivate let rawValue = UUID()

    public init() {}
}

public enum AgentConnectionError: Error, LocalizedError, Equatable, Sendable {
    case executableNotFound(String)
    case failedToStart(String)
    case invalidResponse(String)
    case responseTooLarge(Int)
    case requestTooLarge(Int)
    case processTerminated
    case coreExited(CoreExitError)
    case rpcError(code: Int, message: String, data: JSONValue?)

    public var errorDescription: String? {
        switch self {
        case .executableNotFound(let path):
            return "ottam-core was not found at \(path)"
        case .failedToStart(let message):
            return "Failed to start ottam-core: \(message)"
        case .invalidResponse(let message):
            return "ottam-core returned an invalid response: \(message)"
        case .responseTooLarge(let limit):
            return "ottam-core response exceeded \(limit) bytes"
        case .requestTooLarge(let limit):
            return "This is too large to send to ottam-core, which accepts at most \(limit / 1_048_576) MB at a time."
        case .processTerminated:
            return "ottam-core terminated before returning a response"
        case .coreExited(let error):
            return [error.message, error.details].compactMap { $0 }.joined(separator: "\n")
        case .rpcError(_, let message, _):
            return message
        }
    }
}

public typealias OttamCoreProcessError = AgentConnectionError

public final class OttamCoreProcessClient: @unchecked Sendable {
    private let connection: LocalProcessConnection
    private let eventHub: AgentEventHub
    private let permissionRouter: AgentPermissionRouter

    public init(executableURL: URL, maximumResponseBytes: Int = 16 * 1024 * 1024) {
        let eventHub = AgentEventHub()
        let permissionRouter = AgentPermissionRouter()
        self.eventHub = eventHub
        self.permissionRouter = permissionRouter
        connection = LocalProcessConnection(
            executableURL: executableURL,
            maximumMessageBytes: maximumResponseBytes,
            notificationHandler: { notification in
                await eventHub.publish(notification)
            },
            clientRequestHandler: { request in
                await permissionRouter.handle(request)
            }
        )
    }

    deinit {
        let connection = self.connection
        Task {
            await connection.close()
        }
    }

    public func diffSnapshot(projectSourceID: ProjectSourceID) async throws -> CoreDiffSnapshot {
        try await connection.diffSnapshot(projectSourceID: projectSourceID)
    }

    public func initializeResult() async throws -> InitializeResult {
        try await connection.initializeResult()
    }

    public func setLifecycleHandler(
        _ handler: (@Sendable (CoreProcessLifecycleEvent) async -> Void)?
    ) async {
        await connection.setLifecycleHandler(handler)
    }

    public func listAgents(refreshRuntime: Bool = false) async throws -> AgentListResult {
        try await connection.listAgents(refreshRuntime: refreshRuntime)
    }

    public func detectAgentSetups() async throws -> AgentDetectSetupsResult {
        try await connection.request(method: "agent.detectSetups", params: AgentListParams())
    }

    public func createAgent(harnessID: String, layout: AgentHomeKind, label: String? = nil) async throws -> AgentInstanceResult {
        try await connection.request(method: "agent.create", params: AgentCreateParams(harnessID: harnessID, layout: layout, label: label))
    }

    public func renameAgent(agentID: AgentID, label: String) async throws -> AgentInstanceResult {
        try await connection.request(method: "agent.rename", params: AgentRenameParams(agentID: agentID, label: label))
    }

    public func deleteAgent(agentID: AgentID, purge: Bool, operationKey: String) async throws {
        let _: AgentEmptyResult = try await connection.request(method: "agent.delete", params: AgentDeleteParams(agentID: agentID, purge: purge, operationKey: operationKey))
    }

    public func setAgentLayout(agentID: AgentID, layout: AgentHomeKind) async throws -> AgentInstanceResult {
        try await connection.request(method: "agent.setLayout", params: AgentSetLayoutParams(agentID: agentID, layout: layout))
    }

    public func agentShutdownReadiness() async throws -> AgentShutdownReadinessResult {
        try await connection.request(method: "agent.shutdownReadiness", params: AgentListParams())
    }

    public func shutdownReadinessIfRunning() async throws -> AgentShutdownReadinessResult? {
        try await connection.shutdownReadinessIfRunning()
    }

    public func prepareShutdownIfRunning(operationKey: String) async throws {
        try await connection.prepareShutdownIfRunning(operationKey: operationKey)
    }

    public func prepareAgentShutdown(operationKey: String) async throws {
        let _: AgentEmptyResult = try await connection.request(method: "agent.prepareShutdown", params: AgentShutdownOperationParams(operationKey: operationKey))
    }

    public func cancelAgentShutdown(operationKey: String) async throws {
        try await connection.cancelShutdownIfRunning(operationKey: operationKey)
    }

    public func agentAuthState(agentID: AgentID, refresh: Bool = false) async throws -> AgentAuthStateResult {
        try await connection.agentAuthState(agentID: agentID, refresh: refresh)
    }

    public func agentHomeInfo(agentID: AgentID) async throws -> AgentHomeInfoResult {
        try await connection.agentHomeInfo(agentID: agentID)
    }

    /// The agent's last-known plan usage; a stale snapshot refreshes and
    /// arrives on `agent.planUsageChanged`.
    public func agentPlanUsage(agentID: AgentID, refresh: Bool = false) async throws -> AgentPlanUsageResult {
        try await connection.agentPlanUsage(agentID: agentID, refresh: refresh)
    }

    public func storageStatus() async throws -> StorageStatusResult {
        try await connection.storageRequest(method: "storage.status")
    }

    public func updateStorageLimit(gb: UInt64) async throws -> StorageStatusResult {
        try await connection.storageRequest(method: "storage.update", limitGB: gb)
    }

    public func clearStorageCache() async throws -> StorageStatusResult {
        try await connection.storageRequest(method: "storage.clearCache")
    }

    /// Every configured MCP server, secret values left out.
    public func listMcpServers() async throws -> McpListResult {
        try await connection.request(method: "mcp.list", params: McpListParams())
    }

    public func addMcpServer(_ config: McpServerConfig) async throws -> McpServerSummary {
        let result: McpServerResult = try await connection.request(method: "mcp.add", params: McpAddParams(config: config))
        return result.server
    }

    /// A secret sent without a value keeps the one stored under its name.
    public func updateMcpServer(serverID: McpServerID, config: McpServerConfig) async throws -> McpServerSummary {
        let result: McpServerResult = try await connection.request(
            method: "mcp.update",
            params: McpUpdateParams(serverID: serverID, config: config)
        )
        return result.server
    }

    public func removeMcpServer(serverID: McpServerID) async throws {
        let _: McpRemoveResult = try await connection.request(method: "mcp.remove", params: McpRemoveParams(serverID: serverID))
    }

    public func setMcpServerEnabled(
        serverID: McpServerID,
        workspaceID: WorkspaceID,
        enabled: Bool
    ) async throws -> McpServerSummary {
        let result: McpServerResult = try await connection.request(
            method: "mcp.setWorkspaceEnabled",
            params: McpSetWorkspaceEnabledParams(serverID: serverID, workspaceID: workspaceID, enabled: enabled)
        )
        return result.server
    }

    /// The servers a detected setup or `.mcp.json` defines, read but not added.
    public func importMcpServerCandidates(source: McpImportSource) async throws -> McpImportCandidatesResult {
        try await connection.request(method: "mcp.importCandidates", params: McpImportCandidatesParams(source: source))
    }

    /// Starts a remote server's sign-in; the browser goes to the result's
    /// authorization URL, and `mcp.signInFinished` says how it ended.
    public func signInMcpServer(serverID: McpServerID, clientID: String? = nil) async throws -> McpSignInResult {
        try await connection.request(method: "mcp.signIn", params: McpSignInParams(serverID: serverID, clientID: clientID))
    }

    public func cancelMcpSignIn(serverID: McpServerID) async throws {
        let _: McpCancelSignInResult = try await connection.request(
            method: "mcp.cancelSignIn",
            params: McpCancelSignInParams(serverID: serverID)
        )
    }

    public func signOutMcpServer(serverID: McpServerID) async throws -> McpServerSummary {
        let result: McpServerResult = try await connection.request(
            method: "mcp.signOut",
            params: McpSignOutParams(serverID: serverID)
        )
        return result.server
    }

    /// The named agents a conversation can delegate to.
    public func listNamedAgents() async throws -> [NamedAgent] {
        let result: NamedAgentListResult = try await connection.request(method: "namedAgent.list", params: NamedAgentListParams())
        return result.namedAgents
    }

    public func createNamedAgent(_ config: NamedAgentConfig) async throws -> NamedAgent {
        let result: NamedAgentResult = try await connection.request(
            method: "namedAgent.create",
            params: NamedAgentCreateParams(config: config)
        )
        return result.namedAgent
    }

    public func updateNamedAgent(namedAgentID: NamedAgentID, config: NamedAgentConfig) async throws -> NamedAgent {
        let result: NamedAgentResult = try await connection.request(
            method: "namedAgent.update",
            params: NamedAgentUpdateParams(namedAgentID: namedAgentID, config: config)
        )
        return result.namedAgent
    }

    public func deleteNamedAgent(namedAgentID: NamedAgentID) async throws {
        let _: NamedAgentDeleteResult = try await connection.request(
            method: "namedAgent.delete",
            params: NamedAgentDeleteParams(namedAgentID: namedAgentID)
        )
    }

    /// Continue on a note card: delivers the conversation's waiting notes
    /// in a run, or returns no run when none are waiting.
    public func continueDelegation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationContinueDelegationResult {
        try await connection.request(
            method: "conversation.continueDelegation",
            params: ConversationContinueDelegationParams(workspaceID: workspaceID, conversationID: conversationID)
        )
    }

    /// Send to Parent in a delegated child: its deferred reports wake the
    /// parent, at once or when the parent's running turn ends.
    public func sendToParent(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationSendToParentResult {
        try await connection.request(
            method: "conversation.sendToParent",
            params: ConversationSendToParentParams(workspaceID: workspaceID, conversationID: conversationID)
        )
    }

    /// Begin a sign-in, or re-join the one already running for this agent.
    /// Progress arrives as `agent.signInEvent` notifications; the result only
    /// says the flow was accepted.
    /// Omit `workspaceID` to run the login in the agent's own home — Settings
    /// has no workspace, and a fresh install may have none at all.
    public func signInToAgent(
        agentID: AgentID,
        workspaceID: WorkspaceID? = nil,
        methodID: String? = nil
    ) async throws -> AgentSignInResult {
        try await connection.signInToAgent(
            agentID: agentID,
            workspaceID: workspaceID,
            methodID: methodID
        )
    }

    public func cancelAgentSignIn(
        agentID: AgentID,
        authSessionID: AgentAuthSessionID
    ) async throws {
        try await connection.cancelAgentSignIn(agentID: agentID, authSessionID: authSessionID)
    }

    /// Declare a terminal login complete. Core closes the login terminal,
    /// restarts the adapter, and reports the outcome via `agent.signInEvent`.
    public func finishAgentSignIn(
        agentID: AgentID,
        authSessionID: AgentAuthSessionID
    ) async throws {
        try await connection.finishAgentSignIn(agentID: agentID, authSessionID: authSessionID)
    }

    /// Install the Node runtime Ottam owns, or join the install already
    /// running. Progress arrives as `runtime.installEvent`; the result only
    /// says the work was accepted.
    public func installRuntime() async throws -> RuntimeInstallResult {
        try await connection.installRuntime()
    }

    public func cancelRuntimeInstall(installID: RuntimeInstallID) async throws {
        try await connection.cancelRuntimeInstall(installID: installID)
    }

    /// Install one agent's pinned adapter into Ottam's managed store, or
    /// join the install already running for it. Progress arrives as
    /// `agent.installEvent`; the result only says the work was accepted.
    public func installAgentAdapter(agentID: AgentID) async throws -> AgentInstallResult {
        try await connection.installAgentAdapter(agentID: agentID)
    }

    public func cancelAgentAdapterInstall(installID: AgentInstallID) async throws {
        try await connection.cancelAgentAdapterInstall(installID: installID)
    }

    /// Remove one agent's installed adapter, and with `removeHome` its
    /// Ottam-managed home too. Core shuts the adapter down first; the
    /// agent reports `notInstalled` afterwards.
    public func uninstallAgentAdapter(agentID: AgentID, removeHome: Bool, operationKey: String? = nil) async throws {
        try await connection.uninstallAgentAdapter(agentID: agentID, removeHome: removeHome, operationKey: operationKey)
    }

    /// `workspaceID` nil lists engines before any workspace exists.
    public func listEngines(
        workspaceID: WorkspaceID?,
        refreshRuntime: Bool = false
    ) async throws -> EngineListResult {
        try await connection.listEngines(
            workspaceID: workspaceID,
            refreshRuntime: refreshRuntime
        )
    }

    public func createConversation(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSelection,
        contextProjectIDs: [ProjectID] = [],
        title: String? = nil,
        message: ConversationInitialMessage? = nil
    ) async throws -> ConversationCreateResult {
        try await connection.createConversation(
            workspaceID: workspaceID,
            workingRoot: workingRoot,
            contextProjectIDs: contextProjectIDs,
            title: title,
            message: message
        )
    }

    public func listConversations(workspaceID: WorkspaceID) async throws -> ConversationListResult {
        try await connection.listConversations(workspaceID: workspaceID)
    }

    public func deleteConversation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        children: ConversationDeleteChildren = .keep
    ) async throws -> ConversationDeleteResult {
        try await connection.deleteConversation(
            workspaceID: workspaceID,
            conversationID: conversationID,
            children: children
        )
    }

    public func getConversation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationGetResult {
        try await connection.getConversation(workspaceID: workspaceID, conversationID: conversationID)
    }

    public func listConversationTranscript(workspaceID: WorkspaceID, conversationID: ConversationID, cursor: String? = nil, subagentThreadID: String? = nil) async throws -> ConversationListTranscriptResult {
        try await connection.listConversationTranscript(workspaceID: workspaceID, conversationID: conversationID, cursor: cursor, subagentThreadID: subagentThreadID)
    }

    public func readConversationTranscriptBody(workspaceID: WorkspaceID, conversationID: ConversationID, itemID: String, revision: UInt64, offset: UInt64) async throws -> ConversationReadTranscriptBodyResult {
        try await connection.readConversationTranscriptBody(workspaceID: workspaceID, conversationID: conversationID, itemID: itemID, revision: revision, offset: offset)
    }

    public func readConversationTranscriptPayload(_ params: ConversationReadTranscriptPayloadParams) async throws -> ConversationReadTranscriptPayloadResult {
        try await connection.readConversationTranscriptPayload(params)
    }

    public func listConversationRuns(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        limit: Int? = nil,
        cursor: String? = nil
    ) async throws -> ConversationListRunsResult {
        try await connection.listConversationRuns(
            workspaceID: workspaceID,
            conversationID: conversationID,
            limit: limit,
            cursor: cursor
        )
    }

    public func listConversationRunInputs(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        limit: Int? = nil,
        cursor: String? = nil
    ) async throws -> ConversationListRunInputsResult {
        try await connection.listConversationRunInputs(
            workspaceID: workspaceID,
            conversationID: conversationID,
            limit: limit,
            cursor: cursor
        )
    }

    public func postConversationMessage(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        message: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        idempotencyKey: String
    ) async throws -> ConversationPostMessageResult {
        try await connection.postConversationMessage(
            workspaceID: workspaceID,
            conversationID: conversationID,
            message: message,
            attachmentIDs: attachmentIDs,
            idempotencyKey: idempotencyKey
        )
    }

    public func createConversationAttachment(
        workspaceID: WorkspaceID,
        sourcePath: String,
        mimeType: String,
        width: Int,
        height: Int,
        sourceKind: ConversationAttachmentSourceKind,
        originPath: String? = nil,
        idempotencyKey: String
    ) async throws -> ConversationCreateAttachmentResult {
        try await connection.createConversationAttachment(
            workspaceID: workspaceID,
            sourcePath: sourcePath,
            mimeType: mimeType,
            width: width,
            height: height,
            sourceKind: sourceKind,
            originPath: originPath,
            idempotencyKey: idempotencyKey
        )
    }

    public func discardConversationAttachment(
        workspaceID: WorkspaceID,
        attachmentID: ConversationAttachmentID
    ) async throws -> ConversationDiscardAttachmentResult {
        try await connection.discardConversationAttachment(
            workspaceID: workspaceID,
            attachmentID: attachmentID
        )
    }

    public func startConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        engineID: EngineID,
        modelID: String? = nil,
        modeID: String? = nil,
        thoughtLevelID: String? = nil,
        runInputID: ConversationRunInputID
    ) async throws -> ConversationStartRunResult {
        try await connection.startConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            engineID: engineID,
            modelID: modelID,
            modeID: modeID,
            thoughtLevelID: thoughtLevelID,
            runInputID: runInputID
        )
    }

    public func cancelConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID
    ) async throws -> ConversationCancelRunResult {
        try await connection.cancelConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: runID
        )
    }

    public func stopBackgroundTask(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID,
        asyncTaskID: String
    ) async throws -> ConversationStopBackgroundTaskResult {
        try await connection.stopBackgroundTask(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: runID,
            asyncTaskID: asyncTaskID
        )
    }

    public func setConversationMode(
        _ params: ConversationSetModeParams
    ) async throws -> ConversationSetModeResult {
        try await connection.setConversationMode(params)
    }

    public func setConversationAlwaysAllowToolActions(
        _ params: ConversationSetAlwaysAllowToolActionsParams
    ) async throws -> ToolPermissionsState {
        try await connection.setConversationAlwaysAllowToolActions(params)
    }

    public func toolPermissions() async throws -> ToolPermissionsState {
        try await connection.toolPermissions()
    }

    public func setToolPermissions(_ state: ToolPermissionsState) async throws -> ToolPermissionsState {
        try await connection.setToolPermissions(state)
    }

    public func steerConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID,
        message: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        idempotencyKey: String
    ) async throws -> ConversationSteerRunResult {
        try await connection.steerConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: runID,
            message: message,
            attachmentIDs: attachmentIDs,
            idempotencyKey: idempotencyKey
        )
    }

    public func resolveConversationRunInputDelivery(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runInputID: ConversationRunInputID,
        resolution: RunInputDeliveryResolution
    ) async throws -> ConversationResolveRunInputDeliveryResult {
        try await connection.resolveConversationRunInputDelivery(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runInputID: runInputID,
            resolution: resolution
        )
    }

    public func openProject(path: String) async throws -> ProjectOpenPathResult {
        try await connection.openProject(path: path)
    }

    public func listWorkspaces(includeArchived: Bool = false) async throws -> WorkspaceListResult {
        try await connection.listWorkspaces(includeArchived: includeArchived)
    }

    public func createWorkspace(displayName: String) async throws -> WorkspaceDetail {
        try await connection.createWorkspace(displayName: displayName)
    }

    public func openWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceDetail {
        try await connection.openWorkspace(workspaceID: workspaceID)
    }

    public func getWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceDetail {
        try await connection.getWorkspace(workspaceID: workspaceID)
    }

    public func updateWorkspace(workspaceID: WorkspaceID, displayName: String) async throws -> WorkspaceDetail {
        try await connection.updateWorkspace(workspaceID: workspaceID, displayName: displayName)
    }

    public func archiveWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceSummary {
        try await connection.archiveWorkspace(workspaceID: workspaceID)
    }

    public func restoreWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceSummary {
        try await connection.restoreWorkspace(workspaceID: workspaceID)
    }

    public func addProject(workspaceID: WorkspaceID, path: String) async throws -> WorkspaceAddProjectResult {
        try await connection.addProject(workspaceID: workspaceID, path: path)
    }

    public func removeProject(workspaceID: WorkspaceID, projectID: ProjectID) async throws -> WorkspaceDetail {
        try await connection.removeProject(workspaceID: workspaceID, projectID: projectID)
    }

    public func openTerminal(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSelection,
        rows: UInt16,
        cols: UInt16,
        cwd: String? = nil
    ) async throws -> TerminalSessionSummary {
        try await connection.openTerminal(
            workspaceID: workspaceID,
            workingRoot: workingRoot,
            rows: rows,
            cols: cols,
            cwd: cwd
        )
    }

    public func writeTerminal(sessionID: TerminalSessionID, data: Data) async throws {
        try await connection.writeTerminal(sessionID: sessionID, data: data)
    }

    public func resizeTerminal(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) async throws {
        try await connection.resizeTerminal(sessionID: sessionID, rows: rows, cols: cols)
    }

    public func closeTerminal(sessionID: TerminalSessionID) async throws {
        try await connection.closeTerminal(sessionID: sessionID)
    }

    public func deleteTerminal(workspaceID: WorkspaceID, terminalID: TerminalID) async throws {
        try await connection.deleteTerminal(workspaceID: workspaceID, terminalID: terminalID)
    }

    public func updateTerminalMetadata(workspaceID: WorkspaceID, terminalID: TerminalID, title: String?) async throws {
        try await connection.updateTerminalMetadata(workspaceID: workspaceID, terminalID: terminalID, title: title)
    }

    /// `workspaceID` scopes the lookup when given; a login terminal belongs to
    /// no workspace and is addressed by terminal id alone.
    public func replayTerminal(workspaceID: WorkspaceID? = nil, terminalID: TerminalID, maxBytes: UInt64? = nil) async throws -> TerminalReplayResult {
        try await connection.replayTerminal(workspaceID: workspaceID, terminalID: terminalID, maxBytes: maxBytes)
    }

    /// Registers the handlers for an agent's permission requests and, when
    /// given, its questions; both route the same way.
    public func registerPermissionHandler(
        workspaceID: WorkspaceID,
        conversationID: ConversationID? = nil,
        handler: @escaping AgentPermissionHandler,
        questionHandler: AgentQuestionHandler? = nil
    ) async -> AgentPermissionRegistrationToken {
        await permissionRouter.register(
            workspaceID: workspaceID,
            conversationID: conversationID,
            handler: handler,
            questionHandler: questionHandler
        )
    }

    public func updatePermissionRegistration(
        _ token: AgentPermissionRegistrationToken,
        workspaceID: WorkspaceID,
        conversationID: ConversationID?
    ) async {
        await permissionRouter.update(
            token,
            workspaceID: workspaceID,
            conversationID: conversationID
        )
    }

    public func focusPermissionRegistration(_ token: AgentPermissionRegistrationToken) async {
        await permissionRouter.focus(token)
    }

    public func unregisterPermissionHandler(_ token: AgentPermissionRegistrationToken) async {
        await permissionRouter.unregister(token)
    }

    public func subscribeToEvents(
        filter: AgentEventFilter = .all,
        handler: @escaping @Sendable (AgentNotification) async -> Void
    ) async -> AgentEventSubscriptionToken {
        await eventHub.subscribe(filter: filter, handler: handler)
    }

    public func unsubscribeFromEvents(_ token: AgentEventSubscriptionToken) async {
        await eventHub.unsubscribe(token)
    }
}

public extension AgentConnection {
    func listWorkspaces(includeArchived: Bool = false) async throws -> WorkspaceListResult {
        try await request(
            method: "workspace.list",
            params: WorkspaceListParams(includeArchived: includeArchived)
        )
    }

    func createWorkspace(displayName: String) async throws -> WorkspaceDetail {
        try await request(
            method: "workspace.create",
            params: WorkspaceCreateParams(displayName: displayName)
        )
    }

    func openWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceDetail {
        try await request(
            method: "workspace.open",
            params: WorkspaceIDParams(workspaceID: workspaceID)
        )
    }

    func getWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceDetail {
        try await request(
            method: "workspace.get",
            params: WorkspaceIDParams(workspaceID: workspaceID)
        )
    }

    func updateWorkspace(workspaceID: WorkspaceID, displayName: String) async throws -> WorkspaceDetail {
        try await request(
            method: "workspace.update",
            params: WorkspaceUpdateParams(workspaceID: workspaceID, displayName: displayName)
        )
    }

    func archiveWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceSummary {
        try await request(
            method: "workspace.archive",
            params: WorkspaceIDParams(workspaceID: workspaceID)
        )
    }

    func restoreWorkspace(workspaceID: WorkspaceID) async throws -> WorkspaceSummary {
        try await request(
            method: "workspace.restore",
            params: WorkspaceIDParams(workspaceID: workspaceID)
        )
    }

    func addProject(workspaceID: WorkspaceID, path: String) async throws -> WorkspaceAddProjectResult {
        try await request(
            method: "workspace.addProject",
            params: WorkspaceAddProjectParams(workspaceID: workspaceID, path: path)
        )
    }

    func removeProject(workspaceID: WorkspaceID, projectID: ProjectID) async throws -> WorkspaceDetail {
        try await request(
            method: "workspace.removeProject",
            params: WorkspaceRemoveProjectParams(
                workspaceID: workspaceID,
                projectID: projectID
            )
        )
    }

    func listAgents(refreshRuntime: Bool = false) async throws -> AgentListResult {
        try await request(
            method: "agent.list",
            params: AgentListParams(refreshRuntime: refreshRuntime ? true : nil)
        )
    }

    func agentAuthState(agentID: AgentID, refresh: Bool = false) async throws -> AgentAuthStateResult {
        try await request(method: "agent.authState", params: AgentAuthStateParams(agentID: agentID, refresh: refresh ? true : nil))
    }

    func agentHomeInfo(agentID: AgentID) async throws -> AgentHomeInfoResult {
        try await request(method: "agent.homeInfo", params: AgentHomeInfoParams(agentID: agentID))
    }

    func agentPlanUsage(agentID: AgentID, refresh: Bool = false) async throws -> AgentPlanUsageResult {
        try await request(
            method: "agent.planUsage",
            params: AgentPlanUsageParams(agentID: agentID, refresh: refresh ? true : nil)
        )
    }

    func storageRequest(method: String, limitGB: UInt64? = nil) async throws -> StorageStatusResult {
        try await request(method: method, params: StorageUpdateParams(limitGb: limitGB))
    }

    func signInToAgent(
        agentID: AgentID,
        workspaceID: WorkspaceID?,
        methodID: String?
    ) async throws -> AgentSignInResult {
        try await request(
            method: "agent.signIn",
            params: AgentSignInParams(
                agentID: agentID,
                workspaceID: workspaceID,
                methodID: methodID
            )
        )
    }

    func cancelAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws {
        let _: AgentEmptyResult = try await request(
            method: "agent.signInCancel",
            params: AgentSignInCancelParams(agentID: agentID, authSessionID: authSessionID)
        )
    }

    func installRuntime() async throws -> RuntimeInstallResult {
        try await request(method: "runtime.install", params: RuntimeInstallParams())
    }

    func cancelRuntimeInstall(installID: RuntimeInstallID) async throws {
        let _: RuntimeEmptyResult = try await request(
            method: "runtime.installCancel",
            params: RuntimeInstallCancelParams(installID: installID)
        )
    }

    func installAgentAdapter(agentID: AgentID) async throws -> AgentInstallResult {
        try await request(
            method: "agent.install",
            params: AgentInstallParams(agentID: agentID)
        )
    }

    func cancelAgentAdapterInstall(installID: AgentInstallID) async throws {
        let _: AgentEmptyResult = try await request(
            method: "agent.installCancel",
            params: AgentInstallCancelParams(installID: installID)
        )
    }

    func uninstallAgentAdapter(agentID: AgentID, removeHome: Bool, operationKey: String? = nil) async throws {
        let _: AgentEmptyResult = try await request(
            method: "agent.uninstall",
            params: AgentUninstallParams(operationKey: operationKey, agentID: agentID, removeHome: removeHome ? true : nil)
        )
    }

    func finishAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws {
        let _: AgentEmptyResult = try await request(
            method: "agent.signInFinish",
            params: AgentSignInFinishParams(agentID: agentID, authSessionID: authSessionID)
        )
    }

    func openProject(path: String) async throws -> ProjectOpenPathResult {
        try await request(
            method: "project.openPath",
            params: ProjectOpenPathParams(path: path)
        )
    }

    func diffSnapshot(projectSourceID: ProjectSourceID) async throws -> CoreDiffSnapshot {
        try await request(
            method: "git.diffSnapshot",
            params: GitDiffSnapshotParams(projectSourceID: projectSourceID)
        )
    }

    func listEngines(
        workspaceID: WorkspaceID?,
        refreshRuntime: Bool = false
    ) async throws -> EngineListResult {
        try await request(
            method: "engine.list",
            params: EngineListParams(
                workspaceID: workspaceID,
                refreshRuntime: refreshRuntime ? true : nil
            )
        )
    }

    func createConversation(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSelection,
        contextProjectIDs: [ProjectID] = [],
        title: String? = nil,
        message: ConversationInitialMessage? = nil
    ) async throws -> ConversationCreateResult {
        try await request(
            method: "conversation.create",
            params: ConversationCreateParams(
                workspaceID: workspaceID,
                workingRoot: workingRoot,
                contextProjectIDs: contextProjectIDs,
                title: title,
                message: message
            )
        )
    }

    func listConversations(workspaceID: WorkspaceID) async throws -> ConversationListResult {
        try await request(
            method: "conversation.list",
            params: ConversationListParams(workspaceID: workspaceID)
        )
    }

    func deleteConversation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        children: ConversationDeleteChildren
    ) async throws -> ConversationDeleteResult {
        try await request(
            method: "conversation.delete",
            params: ConversationDeleteParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                children: children
            )
        )
    }

    func getConversation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationGetResult {
        try await request(
            method: "conversation.get",
            params: ConversationGetParams(workspaceID: workspaceID, conversationID: conversationID)
        )
    }

    func listConversationTranscript(workspaceID: WorkspaceID, conversationID: ConversationID, cursor: String? = nil, subagentThreadID: String? = nil) async throws -> ConversationListTranscriptResult {
        try await request(method: "conversation.listTranscript", params: ConversationListTranscriptParams(workspaceID: workspaceID, conversationID: conversationID, cursor: cursor, subagentThreadID: subagentThreadID))
    }

    func readConversationTranscriptBody(workspaceID: WorkspaceID, conversationID: ConversationID, itemID: String, revision: UInt64, offset: UInt64) async throws -> ConversationReadTranscriptBodyResult {
        try await request(method: "conversation.readTranscriptBody", params: ConversationReadTranscriptBodyParams(workspaceID: workspaceID, conversationID: conversationID, itemID: itemID, revision: revision, offset: offset))
    }

    func readConversationTranscriptPayload(_ params: ConversationReadTranscriptPayloadParams) async throws -> ConversationReadTranscriptPayloadResult {
        try await request(method: "conversation.readTranscriptPayload", params: params)
    }

    func listConversationRuns(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        limit: Int? = nil,
        cursor: String? = nil
    ) async throws -> ConversationListRunsResult {
        try await request(
            method: "conversation.listRuns",
            params: ConversationListRunsParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                limit: limit,
                cursor: cursor
            )
        )
    }

    func listConversationRunInputs(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        limit: Int? = nil,
        cursor: String? = nil
    ) async throws -> ConversationListRunInputsResult {
        try await request(
            method: "conversation.listRunInputs",
            params: ConversationListRunInputsParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                limit: limit,
                cursor: cursor
            )
        )
    }

    func postConversationMessage(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        message: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        idempotencyKey: String
    ) async throws -> ConversationPostMessageResult {
        try await request(
            method: "conversation.postMessage",
            params: ConversationPostMessageParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                message: message,
                idempotencyKey: idempotencyKey,
                attachmentIDs: attachmentIDs
            )
        )
    }

    /// Hands Core the path to a file the client wrote. Image bytes cannot cross
    /// JSONL — the frame cap is 1 MiB on both sides — so Core copies from disk
    /// and the caller deletes its temp file afterwards.
    func createConversationAttachment(
        workspaceID: WorkspaceID,
        sourcePath: String,
        mimeType: String,
        width: Int,
        height: Int,
        sourceKind: ConversationAttachmentSourceKind,
        originPath: String? = nil,
        idempotencyKey: String
    ) async throws -> ConversationCreateAttachmentResult {
        try await request(
            method: "conversation.createAttachment",
            params: ConversationCreateAttachmentParams(
                workspaceID: workspaceID,
                sourcePath: sourcePath,
                mimeType: mimeType,
                width: width,
                height: height,
                sourceKind: sourceKind,
                originPath: originPath,
                idempotencyKey: idempotencyKey
            )
        )
    }

    func discardConversationAttachment(
        workspaceID: WorkspaceID,
        attachmentID: ConversationAttachmentID
    ) async throws -> ConversationDiscardAttachmentResult {
        try await request(
            method: "conversation.discardAttachment",
            params: ConversationDiscardAttachmentParams(
                workspaceID: workspaceID,
                attachmentID: attachmentID
            )
        )
    }

    func startConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        engineID: EngineID,
        modelID: String? = nil,
        modeID: String? = nil,
        thoughtLevelID: String? = nil,
        runInputID: ConversationRunInputID
    ) async throws -> ConversationStartRunResult {
        try await request(
            method: "conversation.startRun",
            params: ConversationStartRunParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                engineID: engineID,
                modelID: modelID,
                modeID: modeID,
                thoughtLevelID: thoughtLevelID,
                runInputID: runInputID
            )
        )
    }

    func cancelConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID
    ) async throws -> ConversationCancelRunResult {
        try await request(
            method: "conversation.cancelRun",
            params: ConversationCancelRunParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                runID: runID
            )
        )
    }

    /// Asks the agent to stop a background task `runID` started. The task's
    /// end arrives as a transcript revision; `stopped` says only whether the
    /// agent found it running.
    func stopBackgroundTask(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID,
        asyncTaskID: String
    ) async throws -> ConversationStopBackgroundTaskResult {
        try await request(
            method: "conversation.stopBackgroundTask",
            params: ConversationStopBackgroundTaskParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                runID: runID,
                asyncTaskID: asyncTaskID
            )
        )
    }

    func setConversationMode(
        _ params: ConversationSetModeParams
    ) async throws -> ConversationSetModeResult {
        try await request(method: "conversation.setMode", params: params)
    }

    func setConversationAlwaysAllowToolActions(
        _ params: ConversationSetAlwaysAllowToolActionsParams
    ) async throws -> ToolPermissionsState {
        try await request(method: "conversation.setAlwaysAllowToolActions", params: params)
    }

    func toolPermissions() async throws -> ToolPermissionsState {
        try await request(method: "toolPermissions.get", params: ToolPermissionsGetParams())
    }

    func setToolPermissions(_ state: ToolPermissionsState) async throws -> ToolPermissionsState {
        try await request(method: "toolPermissions.set", params: state)
    }

    func steerConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID,
        message: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        idempotencyKey: String
    ) async throws -> ConversationSteerRunResult {
        try await request(
            method: "conversation.steerRun",
            params: ConversationSteerRunParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                runID: runID,
                message: message,
                idempotencyKey: idempotencyKey,
                attachmentIDs: attachmentIDs
            )
        )
    }

    func resolveConversationRunInputDelivery(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runInputID: ConversationRunInputID,
        resolution: RunInputDeliveryResolution
    ) async throws -> ConversationResolveRunInputDeliveryResult {
        try await request(
            method: "conversation.resolveRunInputDelivery",
            params: ConversationResolveRunInputDeliveryParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                runInputID: runInputID,
                resolution: resolution
            )
        )
    }

    func openTerminal(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSelection,
        rows: UInt16,
        cols: UInt16,
        cwd: String? = nil
    ) async throws -> TerminalSessionSummary {
        try await request(
            method: "terminal.open",
            params: TerminalOpenParams(
                workspaceID: workspaceID,
                workingRoot: workingRoot,
                rows: rows,
                cols: cols,
                cwd: cwd
            )
        )
    }

    func writeTerminal(sessionID: TerminalSessionID, data: Data) async throws {
        _ = try await request(
            method: "terminal.write",
            params: TerminalWriteParams(sessionID: sessionID, dataBase64: data.base64EncodedString())
        ) as TerminalEmptyResult
    }

    func resizeTerminal(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) async throws {
        _ = try await request(
            method: "terminal.resize",
            params: TerminalResizeParams(sessionID: sessionID, rows: rows, cols: cols)
        ) as TerminalEmptyResult
    }

    func closeTerminal(sessionID: TerminalSessionID) async throws {
        _ = try await request(
            method: "terminal.close",
            params: TerminalCloseParams(sessionID: sessionID)
        ) as TerminalEmptyResult
    }

    func deleteTerminal(workspaceID: WorkspaceID, terminalID: TerminalID) async throws {
        _ = try await request(
            method: "terminal.delete",
            params: TerminalDeleteParams(workspaceID: workspaceID, terminalID: terminalID)
        ) as TerminalEmptyResult
    }

    func updateTerminalMetadata(workspaceID: WorkspaceID, terminalID: TerminalID, title: String?) async throws {
        _ = try await request(
            method: "terminal.updateMetadata",
            params: TerminalUpdateMetadataParams(workspaceID: workspaceID, terminalID: terminalID, title: title)
        ) as TerminalEmptyResult
    }

    func replayTerminal(workspaceID: WorkspaceID?, terminalID: TerminalID, maxBytes: UInt64? = nil) async throws -> TerminalReplayResult {
        try await request(
            method: "terminal.replay",
            params: TerminalReplayParams(workspaceID: workspaceID, terminalID: terminalID, maxBytes: maxBytes)
        )
    }

    func initialize(
        clientName: String = "OttamApp",
        clientVersion: String = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.0.0"
    ) async throws -> InitializeResult {
        let params = InitializeParams(
            clientInfo: ClientInfo(name: clientName, version: clientVersion),
            capabilities: Capabilities.app
        )
        return try await request(method: "initialize", params: params)
    }

    func sendInitialized() async throws {
        try await notify(method: "initialized", params: InitializedParams())
    }
}

private actor AgentPermissionRouter {
    private struct Registration: Sendable {
        var workspaceID: WorkspaceID
        var conversationID: ConversationID?
        var focusOrder: UInt64
        var handler: AgentPermissionHandler
        var questionHandler: AgentQuestionHandler?
    }

    private var registrations: [AgentPermissionRegistrationToken: Registration] = [:]
    private var focusOrder: UInt64 = 0

    func register(
        workspaceID: WorkspaceID,
        conversationID: ConversationID?,
        handler: @escaping AgentPermissionHandler,
        questionHandler: AgentQuestionHandler?
    ) -> AgentPermissionRegistrationToken {
        focusOrder &+= 1
        let token = AgentPermissionRegistrationToken()
        registrations[token] = Registration(
            workspaceID: workspaceID,
            conversationID: conversationID,
            focusOrder: focusOrder,
            handler: handler,
            questionHandler: questionHandler
        )
        return token
    }

    func update(
        _ token: AgentPermissionRegistrationToken,
        workspaceID: WorkspaceID,
        conversationID: ConversationID?
    ) {
        guard var registration = registrations[token] else { return }
        registration.workspaceID = workspaceID
        registration.conversationID = conversationID
        registrations[token] = registration
    }

    func focus(_ token: AgentPermissionRegistrationToken) {
        guard var registration = registrations[token] else { return }
        focusOrder &+= 1
        registration.focusOrder = focusOrder
        registrations[token] = registration
    }

    func unregister(_ token: AgentPermissionRegistrationToken) {
        registrations.removeValue(forKey: token)
    }

    func handle(_ request: AgentClientRequest) async -> AgentClientResponse {
        switch request.method {
        case "agent.requestPermission":
            guard let envelope = try? JSONDecoder().decode(
                AgentClientRequestEnvelope<AgentRequestPermissionParams>.self,
                from: request.payload
            ) else {
                return .failure(
                    AgentProtocolError(code: -32602, message: "invalid permission request")
                )
            }
            let params = envelope.params
            let registration = route(workspaceID: params.workspaceID, conversationID: params.conversationID)
            let outcome = if let registration {
                await registration.handler(params)
            } else {
                AgentPermissionOutcome.cancelled
            }
            return .success(outcome.jsonValue)
        case "agent.askQuestion":
            guard let envelope = try? JSONDecoder().decode(
                AgentClientRequestEnvelope<AgentAskQuestionParams>.self,
                from: request.payload
            ) else {
                return .failure(
                    AgentProtocolError(code: -32602, message: "invalid question request")
                )
            }
            let params = envelope.params
            let handler = route(workspaceID: params.workspaceID, conversationID: params.conversationID)?
                .questionHandler
            let outcome = if let handler {
                await handler(params)
            } else {
                AgentQuestionOutcome.cancelled
            }
            return .success(outcome.jsonValue)
        default:
            return .failure(AgentProtocolError(code: -32601, message: "method not found"))
        }
    }

    /// The registration for a request: the most recently focused one in its
    /// workspace, preferring one bound to its conversation.
    private func route(workspaceID: WorkspaceID?, conversationID: ConversationID?) -> Registration? {
        registrations.values
            .filter { registration in
                workspaceID == nil || workspaceID == registration.workspaceID
            }
            .max { lhs, rhs in
                let lhsExact = lhs.conversationID != nil && lhs.conversationID == conversationID
                let rhsExact = rhs.conversationID != nil && rhs.conversationID == conversationID
                if lhsExact != rhsExact {
                    return !lhsExact && rhsExact
                }
                return lhs.focusOrder < rhs.focusOrder
            }
    }
}

private struct AgentClientRequestEnvelope<Params: Decodable>: Decodable {
    var params: Params
}
