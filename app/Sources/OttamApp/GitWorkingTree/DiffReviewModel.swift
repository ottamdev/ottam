import Foundation
import Observation
import OttamBackendClient
import OttamCore
import OSLog
import SwiftUI

/// Observable state for the diff reviewer. Owns the connection to `ottam-core`
/// and the derived groupings the SwiftUI views render.
@MainActor
@Observable
final class DiffReviewModel {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "GitDiff"
    )

    private let client: OttamCoreProcessClient
    let workspaceID: WorkspaceID?
    /// Subscriptions and permission handlers taken through this backend and not
    /// yet released. A closed window must hold none; tests check that it does.
    @ObservationIgnored private(set) var liveEventSubscriptions: Set<AgentEventSubscriptionToken> = []
    @ObservationIgnored private(set) var livePermissionRegistrations: Set<AgentPermissionRegistrationToken> = []
    private(set) var projectSummary: ResolvedProject?

    var snapshot: CoreDiffSnapshot?
    var selectedFilePath: String?
    var isLoading = false
    var errorText: String?
    private(set) var visibleFiles: [CoreDiffFile] = []

    // Memoization, not state. These are filled *during* view updates — a row body
    // asks for its rows, which reads the cache and writes it on a miss. Left
    // observable, that read registers every row as a dependent and the write then
    // invalidates all of them, so one cache miss re-renders the whole diff. They
    // are derived entirely from `snapshot`, so nothing needs to observe them; the
    // resets below keep them honest.
    @ObservationIgnored private var unifiedRowCache: [String: [DiffRowBuilder.UnifiedRow]] = [:]

    /// Rows take this directly rather than reaching through the model. Same
    /// reasoning as the caches above, made structural — see `DiffHighlightStore`.
    @ObservationIgnored let highlighter = DiffHighlightStore()

    init(
        projectSummary: ResolvedProject? = nil,
        workspaceID: WorkspaceID? = nil,
        client: OttamCoreProcessClient
    ) {
        self.projectSummary = projectSummary
        self.workspaceID = workspaceID
        self.client = client
    }

    // MARK: - Derived header values

    var branchTitle: String {
        guard let snapshot else { return isLoading ? "Loading…" : "Diff unavailable" }
        var title = snapshot.branch
        if snapshot.ahead > 0 { title += " ↑\(snapshot.ahead)" }
        if snapshot.behind > 0 { title += " ↓\(snapshot.behind)" }
        return title
    }

    var additions: Int { snapshot?.additions ?? 0 }
    var deletions: Int { snapshot?.deletions ?? 0 }
    var repoDisplayPath: String {
        snapshot?.repoPath
            ?? projectSummary?.sourcePathForDisplay
            ?? projectSummary?.canonicalRoot
            ?? "Project unavailable"
    }
    var hasSnapshot: Bool { snapshot != nil }
    var hasOpenProject: Bool { projectSummary != nil }
    var omittedFiles: [CoreDiffOmittedFile] { snapshot?.omittedFiles ?? [] }

    /// Repo (project) name, e.g. "ottam" — shown Xcode-style next to the branch.
    var projectName: String {
        if let displayName = projectSummary?.displayName, !displayName.isEmpty {
            return displayName
        }
        return repoDisplayPath.split(separator: "/").last.map(String.init) ?? repoDisplayPath
    }

    /// The file the breadcrumb / scroll currently focuses on.
    var selectedFile: CoreDiffFile? {
        guard let selectedFilePath else { return snapshot?.files.first }
        return snapshot?.files.first { $0.path == selectedFilePath } ?? snapshot?.files.first
    }

    // MARK: - Actions

    func refresh() {
        Task {
            await load()
        }
    }

    func openProject(path: String) async throws -> ProjectOpenPathResult {
        try await client.openProject(path: path)
    }

    func probeAgent(engineID: EngineID) async {
        _ = try? await client.agentAuthState(agentID: AgentID(engineID.rawValue), refresh: true)
    }

    func planUsage(engineID: EngineID, refresh: Bool = false) async -> AgentPlanUsage? {
        try? await client.agentPlanUsage(agentID: AgentID(engineID.rawValue), refresh: refresh).usage
    }

    func listEngines(
        workspaceID: WorkspaceID,
        refreshRuntime: Bool = false
    ) async throws -> EngineListResult {
        try await client.listEngines(
            workspaceID: workspaceID,
            refreshRuntime: refreshRuntime
        )
    }

    /// Adapter installation is separate from engine readiness (which also includes Node).
    func uninstalledEngineIDs() async throws -> Set<EngineID> {
        let agents = try await client.listAgents().agents
        return Set(agents.compactMap { agent in
            guard let package = agent.adapterPackage,
                  package.installState != .installed else { return nil }
            return EngineID(agent.agentID.rawValue)
        })
    }

    func createConversation(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSelection,
        contextProjectIDs: [ProjectID] = [],
        title: String? = nil,
        message: ConversationInitialMessage? = nil
    ) async throws -> ConversationCreateResult {
        try await client.createConversation(
            workspaceID: workspaceID,
            workingRoot: workingRoot,
            contextProjectIDs: contextProjectIDs,
            title: title,
            message: message
        )
    }

    func listConversations(workspaceID: WorkspaceID) async throws -> ConversationListResult {
        try await client.listConversations(workspaceID: workspaceID)
    }

    func continueDelegation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationContinueDelegationResult {
        try await client.continueDelegation(workspaceID: workspaceID, conversationID: conversationID)
    }

    func sendToParent(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationSendToParentResult {
        try await client.sendToParent(workspaceID: workspaceID, conversationID: conversationID)
    }

    func deleteConversation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        children: ConversationDeleteChildren = .keep
    ) async throws -> ConversationDeleteResult {
        try await client.deleteConversation(
            workspaceID: workspaceID,
            conversationID: conversationID,
            children: children
        )
    }

    func getConversation(
        workspaceID: WorkspaceID,
        conversationID: ConversationID
    ) async throws -> ConversationGetResult {
        try await client.getConversation(workspaceID: workspaceID, conversationID: conversationID)
    }

    func listConversationTranscript(workspaceID: WorkspaceID, conversationID: ConversationID, cursor: String? = nil, subagentThreadID: String? = nil) async throws -> ConversationListTranscriptResult {
        try await client.listConversationTranscript(workspaceID: workspaceID, conversationID: conversationID, cursor: cursor, subagentThreadID: subagentThreadID)
    }

    func readConversationTranscriptBody(workspaceID: WorkspaceID, conversationID: ConversationID, itemID: String, revision: UInt64, offset: UInt64) async throws -> ConversationReadTranscriptBodyResult {
        try await client.readConversationTranscriptBody(workspaceID: workspaceID, conversationID: conversationID, itemID: itemID, revision: revision, offset: offset)
    }

    func readConversationTranscriptPayload(_ params: ConversationReadTranscriptPayloadParams) async throws -> ConversationReadTranscriptPayloadResult {
        try await client.readConversationTranscriptPayload(params)
    }

    func listConversationRuns(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        limit: Int? = nil,
        cursor: String? = nil
    ) async throws -> ConversationListRunsResult {
        try await client.listConversationRuns(
            workspaceID: workspaceID,
            conversationID: conversationID,
            limit: limit,
            cursor: cursor
        )
    }

    func listConversationRunInputs(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        limit: Int? = nil,
        cursor: String? = nil
    ) async throws -> ConversationListRunInputsResult {
        try await client.listConversationRunInputs(
            workspaceID: workspaceID,
            conversationID: conversationID,
            limit: limit,
            cursor: cursor
        )
    }

    func postConversationMessage(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        message: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        idempotencyKey: String
    ) async throws -> ConversationPostMessageResult {
        try await client.postConversationMessage(
            workspaceID: workspaceID,
            conversationID: conversationID,
            message: message,
            attachmentIDs: attachmentIDs,
            idempotencyKey: idempotencyKey
        )
    }

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
        try await client.createConversationAttachment(
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

    func discardConversationAttachment(
        workspaceID: WorkspaceID,
        attachmentID: ConversationAttachmentID
    ) async throws -> ConversationDiscardAttachmentResult {
        try await client.discardConversationAttachment(
            workspaceID: workspaceID,
            attachmentID: attachmentID
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
        try await client.startConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            engineID: engineID,
            modelID: modelID,
            modeID: modeID,
            thoughtLevelID: thoughtLevelID,
            runInputID: runInputID
        )
    }

    func cancelConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID
    ) async throws -> ConversationCancelRunResult {
        try await client.cancelConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: runID
        )
    }

    func stopBackgroundTask(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID,
        asyncTaskID: String
    ) async throws -> ConversationStopBackgroundTaskResult {
        try await client.stopBackgroundTask(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: runID,
            asyncTaskID: asyncTaskID
        )
    }

    func setConversationMode(
        _ params: ConversationSetModeParams
    ) async throws -> ConversationSetModeResult {
        try await client.setConversationMode(params)
    }

    func setConversationAlwaysAllowToolActions(
        _ params: ConversationSetAlwaysAllowToolActionsParams
    ) async throws -> ToolPermissionsState {
        try await client.setConversationAlwaysAllowToolActions(params)
    }

    func toolPermissions() async throws -> ToolPermissionsState {
        try await client.toolPermissions()
    }

    func setToolPermissions(_ state: ToolPermissionsState) async throws -> ToolPermissionsState {
        try await client.setToolPermissions(state)
    }

    func steerConversationRun(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runID: ConversationRunID,
        message: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        idempotencyKey: String
    ) async throws -> ConversationSteerRunResult {
        try await client.steerConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: runID,
            message: message,
            attachmentIDs: attachmentIDs,
            idempotencyKey: idempotencyKey
        )
    }

    func resolveConversationRunInputDelivery(
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        runInputID: ConversationRunInputID,
        resolution: RunInputDeliveryResolution
    ) async throws -> ConversationResolveRunInputDeliveryResult {
        try await client.resolveConversationRunInputDelivery(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runInputID: runInputID,
            resolution: resolution
        )
    }

    func registerAgentPermissionHandler(
        workspaceID: WorkspaceID,
        conversationID: ConversationID?,
        handler: @escaping AgentPermissionHandler,
        questionHandler: @escaping AgentQuestionHandler
    ) async -> AgentPermissionRegistrationToken {
        let token = await client.registerPermissionHandler(
            workspaceID: workspaceID,
            conversationID: conversationID,
            handler: handler,
            questionHandler: questionHandler
        )
        livePermissionRegistrations.insert(token)
        return token
    }

    func updateAgentPermissionRegistration(
        _ token: AgentPermissionRegistrationToken,
        workspaceID: WorkspaceID,
        conversationID: ConversationID?
    ) async {
        await client.updatePermissionRegistration(
            token,
            workspaceID: workspaceID,
            conversationID: conversationID
        )
    }

    func focusAgentPermissionRegistration(_ token: AgentPermissionRegistrationToken) async {
        await client.focusPermissionRegistration(token)
    }

    func unregisterAgentPermissionHandler(_ token: AgentPermissionRegistrationToken) async {
        livePermissionRegistrations.remove(token)
        await client.unregisterPermissionHandler(token)
    }

    func signInToAgent(
        agentID: AgentID,
        workspaceID: WorkspaceID? = nil,
        methodID: String? = nil
    ) async throws -> AgentSignInResult {
        try await client.signInToAgent(
            agentID: agentID,
            workspaceID: workspaceID,
            methodID: methodID
        )
    }

    func cancelAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws {
        try await client.cancelAgentSignIn(agentID: agentID, authSessionID: authSessionID)
    }

    func finishAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws {
        try await client.finishAgentSignIn(agentID: agentID, authSessionID: authSessionID)
    }

    func subscribeToBackendEvents(
        filter: AgentEventFilter = .all,
        handler: @escaping @Sendable (AgentNotification) async -> Void
    ) async -> AgentEventSubscriptionToken {
        let token = await client.subscribeToEvents(filter: filter, handler: handler)
        liveEventSubscriptions.insert(token)
        return token
    }

    func unsubscribeFromBackendEvents(_ token: AgentEventSubscriptionToken) async {
        liveEventSubscriptions.remove(token)
        await client.unsubscribeFromEvents(token)
    }

    func openTerminal(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSelection,
        rows: UInt16,
        cols: UInt16,
        cwd: String? = nil
    ) async throws -> TerminalSessionSummary {
        try await client.openTerminal(
            workspaceID: workspaceID,
            workingRoot: workingRoot,
            rows: rows,
            cols: cols,
            cwd: cwd
        )
    }

    func writeTerminal(sessionID: TerminalSessionID, data: Data) async throws {
        try await client.writeTerminal(sessionID: sessionID, data: data)
    }

    func resizeTerminal(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) async throws {
        try await client.resizeTerminal(sessionID: sessionID, rows: rows, cols: cols)
    }

    func closeTerminal(sessionID: TerminalSessionID) async throws {
        try await client.closeTerminal(sessionID: sessionID)
    }

    func deleteTerminal(workspaceID: WorkspaceID, terminalID: TerminalID) async throws {
        try await client.deleteTerminal(workspaceID: workspaceID, terminalID: terminalID)
    }

    func replayTerminal(workspaceID: WorkspaceID? = nil, terminalID: TerminalID, maxBytes: UInt64? = nil) async throws -> TerminalReplayResult {
        try await client.replayTerminal(workspaceID: workspaceID, terminalID: terminalID, maxBytes: maxBytes)
    }

    func updateTerminalMetadata(workspaceID: WorkspaceID, terminalID: TerminalID, title: String?) async throws {
        try await client.updateTerminalMetadata(workspaceID: workspaceID, terminalID: terminalID, title: title)
    }

    func configure(projectSummary: ResolvedProject) {
        let oldProjectSourceID = self.projectSummary?.projectSourceID
        self.projectSummary = projectSummary
        errorText = nil

        if oldProjectSourceID != nil && oldProjectSourceID != projectSummary.projectSourceID {
            clearSnapshot()
        }
    }

    func markProjectUnavailable(_ message: String) {
        projectSummary = nil
        isLoading = false
        clearSnapshot()
        errorText = message
    }

    func load() async {
        guard let projectSourceID = projectSummary?.projectSourceID else {
            markProjectUnavailable("Project is unavailable. Open a project before loading the Working Tree.")
            Self.logger.error("refusing to load git diff without an open project")
            return
        }

        isLoading = true
        errorText = nil
        let client = self.client
        let displayPath = repoDisplayPath
        let started = Date()
        Self.logger.info(
            "loading git diff snapshot projectSourceID=\(projectSourceID.rawValue, privacy: .public) path=\(displayPath, privacy: .private)"
        )

        do {
            let snapshot = try await client.diffSnapshot(projectSourceID: projectSourceID)
            isLoading = false
            // An unchanged tree keeps its rendered rows and highlighting.
            if snapshot != self.snapshot { apply(snapshot) }
            Self.logger.info(
                "loaded git diff snapshot files=\(snapshot.files.count, privacy: .public) omitted=\(snapshot.omittedFiles.count, privacy: .public) additions=\(snapshot.additions, privacy: .public) deletions=\(snapshot.deletions, privacy: .public) elapsedMs=\(started.elapsedMilliseconds, privacy: .public)"
            )
        } catch {
            isLoading = false
            Self.logger.error("failed to load git diff snapshot: \(error.localizedDescription, privacy: .private)")
            // A failed refresh keeps the snapshot on screen; the next show
            // retries.
            guard snapshot == nil else { return }
            selectedFilePath = nil
            errorText = error.localizedDescription
        }
    }

    private func clearSnapshot() {
        snapshot = nil
        selectedFilePath = nil
        visibleFiles = []
        unifiedRowCache.removeAll(keepingCapacity: true)
        highlighter.invalidate()
    }

    private func apply(_ snapshot: CoreDiffSnapshot) {
        self.snapshot = snapshot
        errorText = nil
        visibleFiles = snapshot.files
        unifiedRowCache.removeAll(keepingCapacity: true)
        highlighter.invalidate()
        if selectedFilePath == nil || !snapshot.files.contains(where: { $0.path == selectedFilePath }) {
            selectedFilePath = defaultSelection(in: snapshot.files)
        }
    }

    func unifiedRows(for file: CoreDiffFile) -> [DiffRowBuilder.UnifiedRow] {
        if let cached = unifiedRowCache[file.path] {
            return cached
        }
        let rows = DiffRowBuilder.unifiedRows(for: file)
        unifiedRowCache[file.path] = rows
        return rows
    }

    func hasRenderableDiff(_ file: CoreDiffFile) -> Bool {
        file.hunks.contains { hunk in
            hunk.kind != .metadata && !hunk.lines.isEmpty
        }
    }

    private func defaultSelection(in files: [CoreDiffFile]) -> String? {
        files.max { ($0.additions + $0.deletions) < ($1.additions + $1.deletions) }?.path
    }
}

private extension Date {
    var elapsedMilliseconds: Int {
        Int(Date().timeIntervalSince(self) * 1_000)
    }
}

enum DiffFileDisplayState {
    case added
    case deleted
    case renamed(oldPath: String?)
    case modified
    case other(String)

    init(file: CoreDiffFile) {
        switch file.status.lowercased() {
        case "added":
            self = .added
        case "deleted":
            self = .deleted
        case "renamed":
            self = .renamed(oldPath: file.oldPath)
        case "modified":
            self = .modified
        default:
            self = .other(file.status)
        }
    }

    var label: String? {
        switch self {
        case .added:
            return "Added"
        case .deleted:
            return "Deleted"
        case .renamed:
            return "Renamed"
        case .modified:
            return nil
        case .other(let status):
            return status.isEmpty ? nil : status.capitalized
        }
    }

    var secondaryText: String? {
        switch self {
        case .renamed(let oldPath):
            guard let oldPath, !oldPath.isEmpty else { return nil }
            return "from \(oldPath)"
        default:
            return nil
        }
    }
}

/// Resolves the `ottam-core` executable, and in development builds the
/// working directory. A release build only ever runs the core in its own
/// bundle: no environment variable, working directory, or checkout can swap
/// it, and a missing core surfaces as the normal core-launch failure.
enum ReviewEnvironment {
#if !DEBUG
    static var ottamCoreExecutableURL: URL {
        Bundle.main.url(forAuxiliaryExecutable: "ottam-core")
            ?? Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/ottam-core")
    }
#else
    static var defaultWorkingDirectory: String {
        let fileManager = FileManager.default
        if let configured = ProcessInfo.processInfo.environment["OTTAM_WORKSPACE"],
           !configured.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
           fileManager.fileExists(atPath: configured) {
            return URL(fileURLWithPath: configured).standardized.path
        }

        let launchDirectory = fileManager.currentDirectoryPath
        if let gitRoot = gitRoot(for: launchDirectory) {
            return gitRoot
        }
        if isXcodeDerivedDataLaunch(), let sourceRepoRoot = sourceCheckoutRepoRoot() {
            return sourceRepoRoot
        }
        if let bundledRepoRoot = bundledDebugRepoRoot() {
            return bundledRepoRoot
        }
        return fileManager.homeDirectoryForCurrentUser.path
    }

    static var ottamCoreExecutableURL: URL {
        let fileManager = FileManager.default
        if let configured = ProcessInfo.processInfo.environment["OTTAM_CORE_PATH"],
           !configured.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
           fileManager.isExecutableFile(atPath: configured) {
            return URL(fileURLWithPath: configured).standardized
        }

        if let bundledURL = Bundle.main.executableURL?
            .deletingLastPathComponent()
            .appendingPathComponent("ottam-core"),
           fileManager.isExecutableFile(atPath: bundledURL.path) {
            return bundledURL
        }

        let candidateRoots = [
            fileManager.currentDirectoryPath,
            defaultWorkingDirectory,
            isXcodeDerivedDataLaunch() ? sourceCheckoutRepoRoot() : nil
        ].compactMap { $0 }
        for root in candidateRoots {
            guard let gitRoot = gitRoot(for: root) else { continue }
            let candidate = URL(fileURLWithPath: gitRoot)
                .appendingPathComponent(".build/ottam-core/ottam-core")
            if fileManager.isExecutableFile(atPath: candidate.path) {
                return candidate
            }
        }

        return URL(fileURLWithPath: fileManager.currentDirectoryPath)
            .appendingPathComponent(".build/ottam-core/ottam-core")
    }

    private static func gitRoot(for path: String) -> String? {
        let process = Process()
        let outputPipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["-C", path, "rev-parse", "--show-toplevel"]
        process.standardOutput = outputPipe
        process.standardError = Pipe()

        do {
            try process.run()
        } catch {
            return nil
        }

        process.waitUntilExit()
        guard process.terminationStatus == 0 else { return nil }

        let output = outputPipe.fileHandleForReading.readDataToEndOfFile()
        guard let root = String(data: output, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !root.isEmpty else {
            return nil
        }
        return root
    }

    private static func isXcodeDerivedDataLaunch() -> Bool {
        Bundle.main.executableURL?.path.contains("/Library/Developer/Xcode/DerivedData/") ?? false
    }

    /// The checkout an Xcode launch was built from: Xcode records it as
    /// `WorkspacePath` in the DerivedData folder's `info.plist`. Read at run
    /// time so no build, Debug or Release, embeds a source path.
    static func sourceCheckoutRepoRoot(executableURL: URL? = Bundle.main.executableURL) -> String? {
        let components = executableURL?.standardized.pathComponents ?? []
        guard let index = components.firstIndex(of: "DerivedData"), index + 1 < components.count,
              let derivedData = NSURL.fileURL(withPathComponents: Array(components[...(index + 1)])),
              let info = NSDictionary(contentsOf: derivedData.appendingPathComponent("info.plist")),
              let workspace = info["WorkspacePath"] as? String else {
            return nil
        }
        let fileManager = FileManager.default
        var candidate = URL(fileURLWithPath: workspace).standardized

        while true {
            let package = candidate.appendingPathComponent("app/Package.swift")
            let backend = candidate.appendingPathComponent("backend/Cargo.toml")
            if fileManager.fileExists(atPath: package.path),
               fileManager.fileExists(atPath: backend.path) {
                return candidate.path
            }

            let parent = candidate.deletingLastPathComponent()
            if parent.path == candidate.path {
                return nil
            }
            candidate = parent
        }
    }

    private static func bundledDebugRepoRoot() -> String? {
        let repoCandidate = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .standardized
        return gitRoot(for: repoCandidate.path)
    }
#endif
}
