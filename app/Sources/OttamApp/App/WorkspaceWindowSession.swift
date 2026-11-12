import AppKit
import OttamBackendClient
import OttamWorkspaceLogic
import Observation
import OSLog
import SwiftUI

private let commandLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Commands"
)

private let navigationLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "WorkspaceNavigation"
)

/// What a workspace window restores on relaunch. The session keeps it current;
/// the window controller persists it. Column widths and the sidebar's collapsed
/// state are the split view's autosave. Decoding ignores retired keys, such as
/// `route`.
struct WorkspaceRestorationState: Codable, Equatable {
    var projectID = ""
    var chatRoot = ""
    var conversationID = ""
    /// Comma-separated `SidebarSectionKind.persistenceKey`s.
    var collapsedSections = ""
}

/// One workspace window's models and the behavior that drives them, independent
/// of the view tree that renders them.
@MainActor
@Observable
final class WorkspaceWindowSession {
    let runtime: OttamAppRuntime
    let workspaceID: WorkspaceID
    let workspace: WorkspaceStore
    let diffModel: DiffReviewModel
    let terminalWorkspace = TerminalWorkspaceModel()
    let chatWorkspace = ChatWorkspaceModel()
    let tools: ConversationToolsStore
#if DEBUG
    let reloadState = DevReloadState()
#endif
    let windowActions = OttamWindowActions()
    var restored = WorkspaceRestorationState()
    var didResolveLaunchProject = false
    var readyCoreInstanceID: CoreInstanceID?
    var isRehydratingAfterCoreReplacement = false
    var coreFailure: CoreFailurePresentation?
    var didRestoreWorkingRoots = false
    var didRestoreConversationSelection = false
    var isArchiveConfirmationPresented = false
    /// Set as the window closes. Actions still in flight check it after each
    /// await, so none lands on a closed window's models.
    private(set) var isClosed = false
    @ObservationIgnored private var tasks: [UUID: Task<Void, Never>] = [:]
    /// Shows the workspace chooser. Supplied by the window's content.
    @ObservationIgnored var openWorkspaceChooser: () -> Void = {}

    init(workspaceID: WorkspaceID, runtime: OttamAppRuntime) {
        self.workspaceID = workspaceID
        self.runtime = runtime
        workspace = WorkspaceStore(workspaceID: workspaceID)
        diffModel = DiffReviewModel(workspaceID: workspaceID, client: runtime.client)
        tools = ConversationToolsStore { project in
            DiffReviewModel(projectSummary: project, workspaceID: workspaceID, client: runtime.client)
        }
        configureWindowActions()
        chatWorkspace.mcpSignIn = runtime.mcpSignIn
        chatWorkspace.didDeleteConversation = { [weak self] in self?.closeTools(of: $0) }
        chatWorkspace.openSubagentThread = { [weak self] conversationID, thread in
            self?.tools.showSubagent(thread, in: conversationID)
        }
        chatWorkspace.openPlan = { [weak self] conversationID, plan in
            self?.tools.showPlan(plan, in: conversationID)
        }
        chatWorkspace.openFile = { [weak self] conversationID, url in
            self?.tools.open(url, in: conversationID)
        }
    }

    var coreAccessibilityState: String {
        switch runtime.connectionState {
        case .starting:
            "core starting"
        case .ready:
            "core ready"
        case .recovering(let attempt):
            "core recovering attempt \(attempt)"
        case .failed:
            "core recovery failed"
        }
    }

    /// Names the Workspace in the confirmation so a user with several Workspace
    /// windows open can tell which one the sheet belongs to.
    var archiveConfirmationWorkspaceName: String {
        workspace.workspaceSummary?.displayName ?? "this Workspace"
    }

    /// The window's content populates piecemeal on launch (workspace detail →
    /// sidebar, engine catalog → composer pickers); first paint waits for
    /// both so the layout lands settled instead of flickering together in view.
    var isPreparingFirstPaint: Bool {
        !didResolveLaunchProject || chatWorkspace.isPreparingEngines
    }

    /// The sidebar footer names the Workspace, so the title is the content's.
    var windowTitle: String {
        chatWorkspace.selectedDetail?.summary.title ?? chatWorkspace.selectedSummaryTitle ?? "Chat"
    }

    /// The Window menu lists every window, so its item keeps the Workspace
    /// name to tell windows apart.
    var windowsMenuTitle: String {
        guard let workspaceName = workspace.workspaceSummary?.displayName else { return windowTitle }
        return "\(workspaceName) — \(windowTitle)"
    }

    var selectedConversation: ConversationSummary? {
        guard let conversationID = chatWorkspace.selectedConversationID else { return nil }
        return chatWorkspace.conversations.first { $0.conversationID == conversationID }
    }

    /// A draft has no tools until its first send creates the conversation.
    var canToggleTools: Bool {
        workspace.projectOpenError == nil && chatWorkspace.selectedConversationID != nil
    }

    var isToolsPaneShown: Bool {
        canToggleTools && tools.isShown(for: chatWorkspace.selectedConversationID)
    }

    func toggleTools() {
        guard canToggleTools, let conversationID = chatWorkspace.selectedConversationID else { return }
        tools.toggleVisibility(for: conversationID)
    }

    /// Show Next/Previous Tab cycle the shown pane's tabs.
    var canSelectAdjacentTool: Bool {
        guard isToolsPaneShown, let conversationID = chatWorkspace.selectedConversationID else { return false }
        return tools.state(for: conversationID).tools.count > 1
    }

    func selectAdjacentTool(_ offset: Int, focus: Bool) {
        guard canSelectAdjacentTool, let conversationID = chatWorkspace.selectedConversationID else { return }
        tools.selectAdjacent(offset, in: conversationID, focus: focus)
    }

    /// A tab click moves focus into the tab, as in Terminal and Xcode. A tab
    /// it selects takes focus once it mounts; the selected terminal is
    /// already mounted, so it takes focus now.
    func selectTool(_ tool: ConversationTool, in conversationID: ConversationID) {
        let wasSelected = tools.state(for: conversationID).selected == tool
        tools.select(tool, in: conversationID, focus: true)
        guard wasSelected, case .terminal(let terminalID) = tool,
              let record = terminalWorkspace.record(for: terminalID)
        else { return }
        terminalWorkspace.surfaceController(for: record)?.activateFocus()
    }

    /// A conversation whose project left the workspace has no root to start
    /// a terminal in.
    var canOpenTerminal: Bool {
        guard canToggleTools, let conversation = selectedConversation else { return false }
        return terminalWorkspace.canCreateTerminal(at: conversation.workingRoot.selection)
    }

    /// Opens a terminal tab in the selected conversation, focused.
    func openTerminal() {
        guard canOpenTerminal,
              let conversation = selectedConversation,
              let record = terminalWorkspace.createTerminal(at: conversation.workingRoot.selection, backend: diffModel)
        else { return }
        tools.show(.terminal(record.id), in: conversation.conversationID, focus: true)
    }

    func openReview() {
        guard canToggleTools, let conversationID = chatWorkspace.selectedConversationID else { return }
        tools.show(.review, in: conversationID)
    }

    /// The plan Add Tool → Plan opens: the pending plan, else the one the
    /// Plan tab last showed, else the newest plan in the transcript.
    var reopenablePlan: ChatPlan? {
        guard canToggleTools, let conversationID = chatWorkspace.selectedConversationID else { return nil }
        return chatWorkspace.selectedPendingPlans.first?.plan
            ?? tools.state(for: conversationID).plan
            ?? chatWorkspace.selectedLatestPlanRecord
    }

    var canOpenPlan: Bool { reopenablePlan != nil }

    func openPlan() {
        guard let plan = reopenablePlan, let conversationID = chatWorkspace.selectedConversationID else { return }
        tools.showPlan(plan, in: conversationID)
    }

    /// Closes a tab, and a terminal tab's terminal with it.
    func closeTool(_ tool: ConversationTool, in conversationID: ConversationID) {
        guard tools.close(tool, in: conversationID) else { return }
        if case .terminal(let terminalID) = tool {
            terminalWorkspace.closeTerminal(terminalID, backend: diffModel)
        }
    }

    /// A deleted conversation's terminals close with it.
    private func closeTools(of conversationID: ConversationID) {
        for case .terminal(let terminalID) in tools.remove(conversationID) {
            terminalWorkspace.closeTerminal(terminalID, backend: diffModel)
        }
    }

    /// A replaced Core holds none of the window's terminals, so their tabs go.
    func resetTerminals() {
        terminalWorkspace.closeAll(backend: diffModel)
        tools.pruneTerminals(keeping: Set(terminalWorkspace.records.map(\.id)))
    }

    /// `project · branch` for the selected conversation's root, or the draft's.
    var contextSubtitle: String {
        let root: (name: String, sourceID: ProjectSourceID?)?
        if chatWorkspace.selectedConversationID != nil {
            root = selectedConversation.map { ($0.workingRoot.displayName, $0.workingRoot.sourceID) }
        } else {
            let sourceID: ProjectSourceID? = if case .project(_, let sourceID) = chatWorkspace.draftWorkingRoot {
                sourceID
            } else {
                nil
            }
            root = (chatWorkspace.draftWorkingRootName, sourceID)
        }

        var loadedBranches: [ProjectSourceID: String] = [:]
        if diffModel.hasSnapshot, let sourceID = diffModel.projectSummary?.projectSourceID {
            loadedBranches[sourceID] = diffModel.branchTitle
        }
        return WorkspaceContextSubtitle.resolve(
            rootName: root?.name,
            sourceID: root?.sourceID,
            projects: workspace.projects,
            loadedBranches: loadedBranches
        )
    }

    /// The project Remove Selected Project acts on: the selected
    /// conversation's root, or the draft's, while it is in the Workspace.
    var contextProjectID: ProjectID? {
        let projectID: ProjectID?
        if chatWorkspace.selectedConversationID != nil {
            projectID = selectedConversation?.workingRoot.projectID
        } else if case .project(let draftProjectID, _) = chatWorkspace.draftWorkingRoot {
            projectID = draftProjectID
        } else {
            projectID = nil
        }
        guard let projectID, workspace.projects.contains(where: { $0.project.projectID == projectID })
        else { return nil }
        return projectID
    }

    /// Wires the stable menu-command actions object once; see the type's doc comment
    /// for why this must not be rebuilt per render.
    private func configureWindowActions() {
        windowActions.workspace = workspace
        windowActions.session = self
        windowActions.toggleTools = { [weak self] in self?.toggleTools() }
        windowActions.openTerminal = { [weak self] in self?.openTerminal() }
        windowActions.newChat = { [weak self] in
            guard let self else { return }
            commandLogger.info("new chat action invoked")
            chatWorkspace.beginNewConversation(backend: diffModel)
        }
#if DEBUG
        windowActions.rebuildAndRelaunch = { [weak self] in
            guard let self else { return }
            DevRelauncher.rebuildAndRelaunch(
                workspacePath: ReviewEnvironment.defaultWorkingDirectory,
                reloadState: reloadState
            )
        }
#endif
        windowActions.addProject = { [weak self] in self?.chooseWorkspace() }
        windowActions.removeProject = { [weak self] in self?.removeContextProject() }
        windowActions.renameWorkspace = { [weak self] in self?.renameWorkspace() }
        windowActions.archiveWorkspace = { [weak self] in self?.archiveWorkspace() }
    }

    /// Runs from the content's `.task`, which is cancelled only once the
    /// content goes, a turn after the window closes.
    func prepareWorkspace() async {
        await runtime.start()
        guard !Task.isCancelled, !isClosed else { return }
        if case .failed(let message) = runtime.connectionState {
            coreFailure = CoreFailurePresentation(message: message)
        }
        if case .ready(let coreInstanceID) = runtime.connectionState {
            readyCoreInstanceID = coreInstanceID
        }
        await openWorkspaceIfNeeded()
        guard !Task.isCancelled, !isClosed else { return }
        await chatWorkspace.installBackendHandlers(backend: diffModel)
        guard !Task.isCancelled, !isClosed else {
            // Closing uninstalled before these handlers landed.
            await chatWorkspace.uninstallBackendHandlers(backend: diffModel)
            return
        }
        await chatWorkspace.loadIfNeeded(backend: diffModel)
        guard !Task.isCancelled, !isClosed else { return }
        restoreConversationSelectionIfNeeded()
    }

    private func openWorkspaceIfNeeded() async {
        guard !didResolveLaunchProject && !workspace.isOpeningProject else {
            return
        }
        workspace.beginOpeningProject(path: workspaceID.rawValue)
        navigationLogger.info("opening Workspace workspaceID=\(self.workspaceID.rawValue, privacy: .public)")

        do {
            let detail = try await runtime.openWorkspace(workspaceID)
            guard !Task.isCancelled, !isClosed else { return }
            let preferredProjectID = restored.projectID.isEmpty
                ? nil
                : ProjectID(restored.projectID)
            workspace.apply(detail: detail, preferredProjectID: preferredProjectID)
            configureSelectedProject()
            didResolveLaunchProject = true
            navigationLogger.info(
                "opened Workspace workspaceID=\(self.workspaceID.rawValue, privacy: .public)"
            )
        } catch {
            guard !Task.isCancelled, !isClosed else { return }
            let message = error.localizedDescription
            markWorkspaceUnavailable(message)
            didResolveLaunchProject = true
            navigationLogger.error("failed to open Workspace: \(message, privacy: .public)")
        }
    }

    /// One-shot: re-select the window's last conversation once the first
    /// history load has landed. A stale ID clears so the composer landing
    /// shows instead.
    func restoreConversationSelectionIfNeeded() {
        guard !didRestoreConversationSelection, chatWorkspace.didCompleteInitialLoad else { return }
        didRestoreConversationSelection = true
        guard !restored.conversationID.isEmpty else { return }
        let conversationID = ConversationID(restored.conversationID)
        guard chatWorkspace.conversations.contains(where: { $0.conversationID == conversationID })
        else {
            restored.conversationID = ""
            return
        }
        chatWorkspace.select(conversationID, backend: diffModel)
    }

    /// Opens a listed conversation, as selecting its sidebar row does.
    func openConversation(_ conversationID: ConversationID) {
        guard chatWorkspace.selectedConversationID != conversationID else { return }
        chatWorkspace.select(conversationID, backend: diffModel)
    }

    func chooseWorkspace() {
        guard let selectedPath = ProjectFolderPicker.chooseGitBackedWorkspace() else {
            return
        }

        runTask { [self] in
            await openProject(path: selectedPath)
        }
    }

    private func openProject(path: String) async {
        workspace.beginOpeningProject(path: path)
        navigationLogger.info("opening selected project path=\(path, privacy: .private)")

        do {
            let result = try await runtime.addProject(workspaceID: workspaceID, path: path)
            guard !Task.isCancelled, !isClosed else { return }
            workspace.apply(detail: result.workspace, preferredProjectID: result.projectID)
            configureSelectedProject()
            if let member = result.workspace.projects.first(where: {
                $0.project.projectID == result.projectID
            }) {
                // Adding a project focuses it, which is why this does not just
                // defer to `configureSelectedProject` above: in the 2+ shape that
                // would honor the previous root instead. Normalizing keeps the
                // 0→1 transition from landing anywhere the collapsed shape
                // cannot offer.
                let selection = normalizeRestoredWorkingRoot(
                    .project(
                        projectID: result.projectID,
                        sourceID: member.source.projectSourceID
                    ),
                    projects: result.workspace.projects,
                    selectedProject: workspace.projectSummary,
                    hasWorkspaceFiles: workspace.workspaceFiles != nil
                )
                chatWorkspace.selectDraftWorkingRoot(selection)
            }
            didResolveLaunchProject = true
            navigationLogger.info("added selected Project projectID=\(result.projectID.rawValue, privacy: .public)")
        } catch {
            guard !Task.isCancelled, !isClosed else { return }
            let message = error.localizedDescription
            markWorkspaceUnavailable(message)
            didResolveLaunchProject = true
            navigationLogger.error("failed to open selected project: \(message, privacy: .public)")
        }
    }

    func handleConnectionStateChange(_ state: OttamAppRuntime.ConnectionState) {
        if case .ready = state {} else { chatWorkspace.disconnectSessionActivity() }
        if case .failed(let message) = state {
            coreFailure = CoreFailurePresentation(message: message)
            return
        }
        coreFailure = nil
        guard case .ready(let coreInstanceID) = state else { return }
        let previousCoreInstanceID = readyCoreInstanceID
        readyCoreInstanceID = coreInstanceID
        guard let previousCoreInstanceID,
              previousCoreInstanceID != coreInstanceID,
              didResolveLaunchProject
        else { return }
        runTask { [self] in
            await rehydrateAfterCoreReplacement()
        }
    }

    func handleWindowDidBecomeKey(_ notification: Notification) {
        guard let window = notification.object as? NSWindow,
              runtime.windows.contains(window, for: workspaceID)
        else { return }
        runTask { [self] in
            await chatWorkspace.focusBackendHandlers(backend: diffModel)
        }
    }

    private func rehydrateAfterCoreReplacement() async {
        guard !isRehydratingAfterCoreReplacement else { return }
        isRehydratingAfterCoreReplacement = true
        defer { isRehydratingAfterCoreReplacement = false }

        do {
            let detail = try await runtime.getWorkspace(workspaceID)
            guard !Task.isCancelled, !isClosed else { return }
            workspace.apply(detail: detail, preferredProjectID: workspace.selectedProjectID)
            configureSelectedProject()
            await chatWorkspace.recoverAfterCoreReplacement(backend: diffModel)
            guard !Task.isCancelled, !isClosed else { return }
            resetTerminals()
            tools.refreshReviewModels()
        } catch {
            guard !Task.isCancelled, !isClosed else { return }
            let message = error.localizedDescription
            markWorkspaceUnavailable(message)
            navigationLogger.error(
                "failed to rehydrate Workspace after Core replacement: \(message, privacy: .public)"
            )
        }
    }

    func retryCoreFromSheet() {
        coreFailure = nil
        runtime.retryConnection()
    }

    func closeWorkspaceAfterCoreFailure() {
        coreFailure = nil
        closeWorkspaceWindow()
    }

    /// Called once, as the window closes: cancels the window's actions, closes
    /// its terminals, stops watching its files, and releases the backend
    /// handlers it installed. The
    /// cleanup runs outside `runTask`, which a closed window no longer starts.
    func tearDown() {
        guard !isClosed else { return }
        isClosed = true
        for task in tasks.values { task.cancel() }
        tasks.removeAll()
        terminalWorkspace.closeAll(backend: diffModel)
        tools.releaseDocuments()
        Task {
            await chatWorkspace.uninstallBackendHandlers(backend: diffModel)
        }
    }

    /// Starts one of the window's actions, cancelled when the window closes.
    /// Each checks `!Task.isCancelled, !isClosed` after every await.
    private func runTask(_ operation: @escaping @MainActor () async -> Void) {
        guard !isClosed else { return }
        let id = UUID()
        tasks[id] = Task { [weak self] in
            await operation()
            self?.tasks.removeValue(forKey: id)
        }
    }

    private func closeWorkspaceWindow() {
        runtime.windows.close(workspaceID)
        if runtime.windows.openWorkspaceCount == 0 {
            openWorkspaceChooser()
        }
    }

    /// Drops the workspace into the "project unavailable" state: records the failure and
    /// reconfigures the terminal and chat surfaces with no selected project.
    private func markWorkspaceUnavailable(_ message: String) {
        workspace.failProjectOpen(message)
        diffModel.markProjectUnavailable(message)
        terminalWorkspace.configure(
            workspaceID: workspaceID,
            workspaceFiles: workspace.workspaceFiles,
            projects: workspace.projects,
            backend: diffModel
        )
        chatWorkspace.configure(
            workspaceID: workspaceID,
            workspaceFiles: workspace.workspaceFiles,
            projects: workspace.projects,
            projectSummary: nil,
            backend: diffModel
        )
    }

    private func configureSelectedProject() {
        let project = workspace.projectSummary
        if let project {
            diffModel.configure(projectSummary: project)
        } else {
            diffModel.markProjectUnavailable("Add a Project to this Workspace to continue.")
        }
        terminalWorkspace.configure(
            workspaceID: workspaceID,
            workspaceFiles: workspace.workspaceFiles,
            projects: workspace.projects,
            backend: diffModel
        )
        chatWorkspace.configure(
            workspaceID: workspaceID,
            workspaceFiles: workspace.workspaceFiles,
            projects: workspace.projects,
            projectSummary: project,
            backend: diffModel
        )
        if !didRestoreWorkingRoots {
            // A restored root is not a fresh user pick, so it is normalized
            // against the current shape before it is applied. `.workspaceFiles`
            // is always valid, so the setters' validity check would otherwise
            // let a root stored under a different shape survive and silently
            // create work in a directory the user can no longer see or choose.
            if let selection = Self.decode(restored.chatRoot) {
                chatWorkspace.selectDraftWorkingRoot(
                    normalizeRestoredWorkingRoot(
                        selection,
                        projects: workspace.projects,
                        selectedProject: project,
                        hasWorkspaceFiles: workspace.workspaceFiles != nil
                    )
                )
            }
            didRestoreWorkingRoots = true
        }
    }

    static func encode(_ selection: WorkingRootSelection) -> String {
        switch selection {
        case .workspaceFiles:
            return "workspaceFiles"
        case .project(let projectID, let sourceID):
            return "project:\(projectID.rawValue):\(sourceID.rawValue)"
        }
    }

    static func decode(_ value: String) -> WorkingRootSelection? {
        if value == "workspaceFiles" {
            return .workspaceFiles
        }
        let parts = value.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0] == "project" else { return nil }
        return .project(
            projectID: ProjectID(String(parts[1])),
            sourceID: ProjectSourceID(String(parts[2]))
        )
    }

    private func removeContextProject() {
        guard let projectID = contextProjectID,
              let removedIndex = workspace.projects.firstIndex(where: {
                  $0.project.projectID == projectID
              })
        else { return }
        let project = workspace.projects[removedIndex]
        let alert = NSAlert()
        alert.messageText = "Remove \(project.project.displayName) from this Workspace?"
        alert.informativeText = "The Project and its history will not be deleted."
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        runTask { [self] in
            do {
                let detail = try await runtime.removeProject(
                    workspaceID: workspaceID,
                    projectID: projectID
                )
                guard !Task.isCancelled, !isClosed else { return }
                let ordered = detail.projects.sorted {
                    ($0.sortOrder, $0.project.projectID.rawValue)
                        < ($1.sortOrder, $1.project.projectID.rawValue)
                }
                let replacement = ordered.isEmpty
                    ? nil
                    : ordered[min(removedIndex, ordered.count - 1)].project.projectID
                workspace.apply(detail: detail, preferredProjectID: replacement)
                configureSelectedProject()
            } catch {
                guard !Task.isCancelled, !isClosed else { return }
                workspace.failProjectOpen(error.localizedDescription)
            }
        }
    }

    private func renameWorkspace() {
        let alert = NSAlert()
        alert.messageText = "Rename Workspace"
        let field = NSTextField(
            string: workspace.workspaceSummary?.displayName ?? ""
        )
        field.frame = NSRect(x: 0, y: 0, width: 320, height: 24)
        alert.accessoryView = field
        alert.addButton(withTitle: "Rename")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        runTask { [self] in
            do {
                let detail = try await runtime.renameWorkspace(
                    workspaceID,
                    displayName: field.stringValue
                )
                guard !Task.isCancelled, !isClosed else { return }
                workspace.apply(detail: detail, preferredProjectID: workspace.selectedProjectID)
            } catch {
                guard !Task.isCancelled, !isClosed else { return }
                workspace.failProjectOpen(error.localizedDescription)
            }
        }
    }

    /// Window-modal so the confirmation is visibly tied to the Workspace window it
    /// will archive, rather than floating free of it like an app-modal `NSAlert`.
    private func archiveWorkspace() {
        isArchiveConfirmationPresented = true
    }

    func performArchiveWorkspace() {
        runTask { [self] in
            do {
                try await runtime.archiveWorkspace(workspaceID)
                guard !Task.isCancelled, !isClosed else { return }
                closeWorkspaceWindow()
            } catch {
                guard !Task.isCancelled, !isClosed else { return }
                workspace.failProjectOpen(error.localizedDescription)
            }
        }
    }
}
