import AppKit
import OttamBackendClient
import OttamPresentationLogic
import Observation

@MainActor
@Observable
final class OttamAppRuntime {
    enum ConnectionState: Equatable {
        case starting
        case ready(CoreInstanceID)
        case recovering(attempt: Int)
        case failed(String)
    }

    let client: OttamCoreProcessClient
    let windows = WorkspaceWindowRegistry()
    /// One managed Node runtime per Mac, so one install controller per process.
    /// Every surface that offers the install shares this state.
    let runtimeInstall: RuntimeInstallController
    /// Adapter installs, keyed per agent. Settings is the only surface today,
    /// but the state is process-wide for the same reason the runtime's is.
    let agentInstall: AgentInstallController
    /// Remote MCP servers' sign-ins, which Settings and chat cards share.
    let mcpSignIn: McpSignInController
    private(set) var connectionState: ConnectionState = .starting
    private(set) var coreExitError: CoreExitError?
    private(set) var workspaces: [WorkspaceListItem] = []
    /// A one-shot deep-link into the Settings window, set just before
    /// `openWindow(id: "settings")` and cleared by the window on consumption.
    /// Deliberately transient — persisting it would make a plain ⌘, reopen
    /// whatever pane a deep-link once targeted.
    var pendingSettingsPane: SettingsPane?
    private(set) var workspaceListError: String?
    private var recoveryTask: Task<Void, Never>?
    private var lifecycleInstalled = false
    private var didStart = false

    init(
        client: OttamCoreProcessClient = OttamCoreProcessClient(
            executableURL: ReviewEnvironment.ottamCoreExecutableURL
        )
    ) {
        self.client = client
        self.runtimeInstall = RuntimeInstallController(client: client)
        self.agentInstall = AgentInstallController(client: client)
        self.mcpSignIn = McpSignInController(client: client)
        // Seed the chooser dimensions before Core is up; refreshed lists
        // replace the snapshot after startup.
        workspaces = WorkspaceListSnapshotCache.load()
    }

    var activeWorkspaces: [WorkspaceListItem] {
        workspaces.filter { $0.workspace.archivedAt == nil }
    }

    var archivedWorkspaces: [WorkspaceListItem] {
        workspaces.filter { $0.workspace.archivedAt != nil }
    }

    func start() async {
        if !lifecycleInstalled {
            lifecycleInstalled = true
            await client.setLifecycleHandler { [weak self] event in
                await self?.receiveLifecycleEvent(event)
            }
        }
        guard !didStart else { return }
        didStart = true
        // Off the startup path: a chat calls a server only once its workspace
        // has opened, long after this subscription lands.
        Task { await mcpSignIn.start() }
        await refreshWorkspaces()
    }

    func refreshWorkspaces() async {
        do {
            let result = try await client.listWorkspaces(includeArchived: true)
            workspaces = result.workspaces
            workspaceListError = nil
            coreExitError = nil
            WorkspaceListSnapshotCache.save(result.workspaces)
            if case .ready = connectionState {
                return
            }
            let initialized = try await client.initializeResult()
            connectionState = .ready(initialized.coreInstanceID)
        } catch {
            retainCoreExitError(error)
            workspaceListError = error.localizedDescription
            await recover(after: error)
        }
    }

    func createWorkspace(displayName: String) async throws -> WorkspaceDetail {
        let detail = try await client.createWorkspace(displayName: displayName)
        await refreshWorkspaces()
        return detail
    }

    func openWorkspace(_ workspaceID: WorkspaceID) async throws -> WorkspaceDetail {
        let detail = try await client.openWorkspace(workspaceID: workspaceID)
        await refreshWorkspaces()
        return detail
    }

    func getWorkspace(_ workspaceID: WorkspaceID) async throws -> WorkspaceDetail {
        try await client.getWorkspace(workspaceID: workspaceID)
    }

    func renameWorkspace(_ workspaceID: WorkspaceID, displayName: String) async throws -> WorkspaceDetail {
        let detail = try await client.updateWorkspace(
            workspaceID: workspaceID,
            displayName: displayName
        )
        await refreshWorkspaces()
        return detail
    }

    func archiveWorkspace(_ workspaceID: WorkspaceID) async throws {
        _ = try await client.archiveWorkspace(workspaceID: workspaceID)
        await refreshWorkspaces()
    }

    func restoreWorkspace(_ workspaceID: WorkspaceID) async throws {
        _ = try await client.restoreWorkspace(workspaceID: workspaceID)
        await refreshWorkspaces()
    }

    func addProject(workspaceID: WorkspaceID, path: String) async throws -> WorkspaceAddProjectResult {
        let result = try await client.addProject(workspaceID: workspaceID, path: path)
        await refreshWorkspaces()
        return result
    }

    func removeProject(workspaceID: WorkspaceID, projectID: ProjectID) async throws -> WorkspaceDetail {
        let detail = try await client.removeProject(
            workspaceID: workspaceID,
            projectID: projectID
        )
        await refreshWorkspaces()
        return detail
    }

    func openProject(path: String) async throws -> ProjectOpenPathResult {
        let result = try await client.openProject(path: path)
        await refreshWorkspaces()
        return result
    }

    func retryConnection() {
        recoveryTask?.cancel()
        recoveryTask = nil
        didStart = false
        coreExitError = nil
        connectionState = .starting
        Task { await start() }
    }

    private func recover(after error: Error) async {
        retainCoreExitError(error)
        guard recoveryTask == nil else { return }
        recoveryTask = Task { @MainActor [weak self] in
            guard let self else { return }
            let delays: [UInt64] = [250_000_000, 1_000_000_000, 3_000_000_000]
            var latestError = error
            for (index, delay) in delays.enumerated() {
                guard !Task.isCancelled else { return }
                connectionState = .recovering(attempt: index + 1)
                try? await Task.sleep(nanoseconds: delay)
                do {
                    let result = try await client.listWorkspaces(includeArchived: true)
                    let initialized = try await client.initializeResult()
                    workspaces = result.workspaces
                    workspaceListError = nil
                    coreExitError = nil
                    WorkspaceListSnapshotCache.save(result.workspaces)
                    connectionState = .ready(initialized.coreInstanceID)
                    recoveryTask = nil
                    return
                } catch {
                    latestError = error
                    retainCoreExitError(error)
                    workspaceListError = error.localizedDescription
                }
            }
            connectionState = .failed(latestError.localizedDescription)
            recoveryTask = nil
        }
    }

    private func receiveLifecycleEvent(_ event: CoreProcessLifecycleEvent) async {
        switch event {
        case .starting:
            if recoveryTask == nil {
                connectionState = .starting
            }
        case .ready(let result):
            connectionState = .ready(result.coreInstanceID)
        case .terminated(_, let error):
            connectionState = .recovering(attempt: 1)
            await recover(after: error.map(AgentConnectionError.coreExited) ?? .processTerminated)
        }
    }

    private func retainCoreExitError(_ error: Error) {
        if case AgentConnectionError.coreExited(let exitError) = error {
            coreExitError = exitError
        }
    }
}

struct WorkspaceWindowSnapshot: Equatable {
    let workspaceID: WorkspaceID
    let windowNumber: Int
    let title: String
    let isKeyWindow: Bool
}

/// Owns the open workspace windows, one per workspace.
@MainActor
@Observable
final class WorkspaceWindowRegistry {
    /// The main workspace window's actions, read by the menu-bar commands. Nil
    /// while another window (chooser, Settings) is main.
    var activeActions: OttamWindowActions?
    @ObservationIgnored private var controllers: [WorkspaceID: WorkspaceWindowController] = [:]
    @ObservationIgnored private weak var chooserWindow: NSWindow?

    init() {
        NotificationCenter.default.addObserver(
            forName: NSApplication.didFinishRestoringWindowsNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.didFinishRestoringWindows()
            }
        }
    }

    func registerChooser(_ window: NSWindow) {
        chooserWindow = window
    }

    /// SwiftUI skips the chooser whenever the app has saved windows, so a
    /// launch whose saved windows all fail to restore would start with none.
    private func didFinishRestoringWindows() {
        guard controllers.isEmpty else { return }
        restorationLogger.notice("no workspace window restored; opening the default window")
        _ = NSApplication.shared.delegate?.applicationOpenUntitledFile?(NSApplication.shared)
    }

    func closeChooser() {
        chooserWindow?.performClose(nil)
        chooserWindow = nil
    }

    /// Shows the workspace's window, creating it on first open. Restoration
    /// passes the saved state and lets AppKit show the window.
    @discardableResult
    func open(
        _ workspaceID: WorkspaceID,
        runtime: OttamAppRuntime,
        restored: WorkspaceRestorationState? = nil,
        show: Bool = true
    ) -> WorkspaceWindowController {
        let controller = controllers[workspaceID] ?? {
            let session = WorkspaceWindowSession(workspaceID: workspaceID, runtime: runtime)
            if let restored {
                session.restored = restored
            }
            let controller = WorkspaceWindowController(session: session, registry: self)
            if restored == nil, !controller.hasSavedFrame, let anchor = frontmostWindow {
                controller.cascade(from: anchor)
            }
            controllers[workspaceID] = controller
            return controller
        }()
        if show {
            NSApplication.shared.activate()
            controller.showWindow(nil)
        }
        return controller
    }

    func focus(_ workspaceID: WorkspaceID) -> Bool {
        guard let window = controllers[workspaceID]?.window else { return false }
        NSApplication.shared.activate()
        window.makeKeyAndOrderFront(nil)
        return true
    }

    func close(_ workspaceID: WorkspaceID) {
        controllers[workspaceID]?.close()
    }

    /// Called by a closing window.
    func release(_ controller: WorkspaceWindowController) {
        let workspaceID = controller.session.workspaceID
        guard controllers[workspaceID] === controller else { return }
        controllers.removeValue(forKey: workspaceID)
    }

    func contains(_ window: NSWindow, for workspaceID: WorkspaceID) -> Bool {
        controllers[workspaceID]?.window === window
    }

    var openWorkspaceCount: Int {
        controllers.count
    }

    private var frontmostWindow: NSWindow? {
        let windows = Set(controllers.values.compactMap { $0.window.map(ObjectIdentifier.init) })
        return NSApplication.shared.orderedWindows.first { windows.contains(ObjectIdentifier($0)) }
    }

    var snapshots: [WorkspaceWindowSnapshot] {
        controllers.compactMap { workspaceID, controller in
            guard let window = controller.window else { return nil }
            return WorkspaceWindowSnapshot(
                workspaceID: workspaceID,
                windowNumber: window.windowNumber,
                title: window.title,
                isKeyWindow: window.isKeyWindow
            )
        }
        .sorted { $0.workspaceID.rawValue < $1.workspaceID.rawValue }
    }
}
