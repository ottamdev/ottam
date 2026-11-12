import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamWorkspaceLogic
import OSLog
import SwiftUI

private let commandLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Commands"
)

@MainActor
/// A workspace window's menu-bar actions, published by the registry while the
/// window is main. One identity-stable object per window, so the commands
/// re-evaluate only when the observable fields they read change; rebuilding
/// the menu bar's NSMenus mid-update briefly flashes their popup windows.
@Observable
final class OttamWindowActions {
    weak var workspace: WorkspaceStore?
    weak var session: WorkspaceWindowSession?
    var toggleSidebar: () -> Void = {}
    var toggleTools: () -> Void = {}
    var openTerminal: () -> Void = {}
    var selectAdjacentTool: (Int) -> Void = { _ in }
    var newChat: () -> Void = {}
#if DEBUG
    var rebuildAndRelaunch: () -> Void = {}
#endif
    var addProject: () -> Void = {}
    var removeProject: () -> Void = {}
    var renameWorkspace: () -> Void = {}
    var archiveWorkspace: () -> Void = {}

    /// Read by both the menu commands and the toolbar items.
    var canToggleTools: Bool { session?.canToggleTools ?? false }
    var isToolsPaneShown: Bool { session?.isToolsPaneShown ?? false }
    var canOpenTerminal: Bool { session?.canOpenTerminal ?? false }
    var canSelectAdjacentTool: Bool { session?.canSelectAdjacentTool ?? false }
    var canRemoveProject: Bool { session?.contextProjectID != nil }
}

struct OttamCommands: Commands {
    let runtime: OttamAppRuntime
    @Environment(\.openWindow) private var openWindow

    private var windowActions: OttamWindowActions? { runtime.windows.activeActions }

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Ottam Window") {
                commandLogger.info("open workspace chooser command invoked")
                openWindow(id: "workspaceChooser")
            }
        }

        CommandGroup(replacing: .appSettings) {
            Button("Settings...") {
                commandLogger.info("open settings command invoked")
                openWindow(id: "settings")
            }
            .keyboardShortcut(",", modifiers: .command)
        }

        CommandMenu("Workspace") {
            Button("Open Workspace…") {
                openWindow(id: "workspaceChooser")
            }
            .keyboardShortcut("o", modifiers: [.command, .shift])

            Divider()

            Button("Add Project…") {
                windowActions?.addProject()
            }
            .disabled(windowActions == nil)

            Button("Remove Selected Project…") {
                windowActions?.removeProject()
            }
            .disabled(!(windowActions?.canRemoveProject ?? false))

            Divider()

            Button("Rename Workspace…") {
                windowActions?.renameWorkspace()
            }
            .disabled(windowActions == nil)

            Button("Archive Workspace…") {
                windowActions?.archiveWorkspace()
            }
            .disabled(windowActions == nil)

            Divider()

            Button("Toggle Sidebar") {
                commandLogger.info("toggle sidebar command invoked")
                windowActions?.toggleSidebar()
            }
                .keyboardShortcut("s", modifiers: [.command, .control])
                .disabled(windowActions == nil)

            Button(windowActions?.isToolsPaneShown == true ? "Hide Tools" : "Show Tools") {
                commandLogger.info("toggle tools command invoked")
                windowActions?.toggleTools()
            }
                .keyboardShortcut("i", modifiers: [.command, .option])
                .disabled(!(windowActions?.canToggleTools ?? false))

            Button("New Terminal") {
                commandLogger.info("new terminal command invoked")
                windowActions?.openTerminal()
            }
                .keyboardShortcut("`", modifiers: .control)
                .disabled(!(windowActions?.canOpenTerminal ?? false))

            Divider()

            Button("New Chat") {
                commandLogger.info("new chat command invoked")
                windowActions?.newChat()
            }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(windowActions == nil)

        }

        // Workspace windows don't tab, so these name the tools pane's tabs.
        CommandGroup(after: .windowSize) {
            Divider()

            Button("Show Previous Tab") {
                windowActions?.selectAdjacentTool(-1)
            }
                .keyboardShortcut("[", modifiers: [.command, .shift])
                .disabled(!(windowActions?.canSelectAdjacentTool ?? false))

            Button("Show Next Tab") {
                windowActions?.selectAdjacentTool(1)
            }
                .keyboardShortcut("]", modifiers: [.command, .shift])
                .disabled(!(windowActions?.canSelectAdjacentTool ?? false))
        }

#if DEBUG
        // Development builds only: release builds ship no Debug menu.
        CommandMenu("Debug") {
            Button("Rebuild and Relaunch") {
                commandLogger.info("rebuild and relaunch command invoked")
                windowActions?.rebuildAndRelaunch()
            }
            .keyboardShortcut("r")
            .disabled(windowActions == nil)
            Button("Markdown Engine Lab") {
                openWindow(id: "markdownEngineLab")
            }
        }
#endif
    }
}

struct WorkspaceRootView: View {
    @Bindable var session: WorkspaceWindowSession
    /// Read in `body`, so a font change in Settings invalidates the content
    /// surface without changing view identity — terminal surfaces and scroll
    /// positions survive it.
    private let typography = TypographySettings.shared
    @Environment(\.openWindow) private var openWindow

    private var runtime: OttamAppRuntime { session.runtime }
    private var workspaceID: WorkspaceID { session.workspaceID }
    private var workspace: WorkspaceStore { session.workspace }
    private var diffModel: DiffReviewModel { session.diffModel }
    private var chatWorkspace: ChatWorkspaceModel { session.chatWorkspace }

    var body: some View {
        stateTrackingView
#if DEBUG
        .background {
            if session.didResolveLaunchProject {
                UITestWindowProbe { window in
                    UITestPreparation.registerWorkspaceIfRequested(
                        workspaceID: workspaceID,
                        window: window,
                        workspace: workspace,
                        diffModel: diffModel,
                        chatWorkspace: chatWorkspace,
                        openSettings: openSettingsWindow,
                        openMarkdownLab: { openWindow(id: "markdownEngineLab") }
                    )
                }
                .frame(width: 0, height: 0)
            }
        }
#endif
        .sheet(item: $session.coreFailure) { failure in
            CoreRecoverySheet(
                message: failure.message,
                exitError: runtime.coreExitError,
                retry: session.retryCoreFromSheet,
                closeWorkspace: session.closeWorkspaceAfterCoreFailure
            )
        }
        .alert(
            "Archive “\(session.archiveConfirmationWorkspaceName)”?",
            isPresented: $session.isArchiveConfirmationPresented
        ) {
            Button("Archive", role: .destructive) { session.performArchiveWorkspace() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Projects and history remain available. You can restore the Workspace later.")
        }
    }

    private var configuredWorkspaceView: some View {
        detailContent
        // A replacement Core invalidates subscriptions and AppKit-backed view
        // state below the stable window models. Rebuild each column's content
        // once per Core generation while preserving the window-owned models.
        .id(session.readyCoreInstanceID)
        // Leaf views (engine picker's Manage Agents…, the composer's Set Up
        // Agents) deep-link to Settings through the runtime in the environment.
        .environment(runtime)
    }

    private var stateTrackingView: some View {
        configuredWorkspaceView
        .task {
            // The action alone: capturing the view would capture the session
            // that stores this closure.
            session.openWorkspaceChooser = { [openWindow] in openWindow(id: "workspaceChooser") }
            await session.prepareWorkspace()
        }
        .onChange(of: workspace.selectedProjectID) { _, projectID in
            session.restored.projectID = projectID?.rawValue ?? ""
        }
        .onChange(of: workspace.projects.map(\.source.projectSourceID)) { _, sourceIDs in
            session.tools.retainReviewModels(for: Set(sourceIDs))
        }
        .onChange(of: chatWorkspace.draftWorkingRoot) { _, selection in
            session.restored.chatRoot = WorkspaceWindowSession.encode(selection)
        }
        .onChange(of: chatWorkspace.selectedConversationID) { _, conversationID in
            session.restored.conversationID = conversationID?.rawValue ?? ""
            session.tools.cancelFocus()
        }
        .onChange(of: chatWorkspace.didCompleteInitialLoad) { _, completed in
            if completed {
                session.restoreConversationSelectionIfNeeded()
            }
        }
        .onChange(of: runtime.connectionState) { _, state in
            session.handleConnectionStateChange(state)
        }
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { notification in
            session.handleWindowDidBecomeKey(notification)
        }
    }

    @ViewBuilder
    private var detailContent: some View {
        if let projectOpenError = workspace.projectOpenError {
            ProjectUnavailableWorkspaceView(
                projectOpenError: projectOpenError,
                projectOpenPath: workspace.projectOpenPath,
                isOpeningProject: workspace.isOpeningProject,
                chooseWorkspace: session.chooseWorkspace
            )
        } else {
            WorkspaceDetailView(
                workspace: workspace,
                diffModel: diffModel,
                chatWorkspace: chatWorkspace,
                addProject: session.chooseWorkspace
            )
            // The detail column is the content surface: agent output, code,
            // diffs, the composer. The sidebar and toolbar around it stay in
            // the system font. See `appTypographyRoot`.
            .appTypographyRoot(typography.resolved)
#if DEBUG
            .devReloadOverlay(session.reloadState)
#endif
        }
    }

    private func openSettingsWindow() {
        openWindow(id: "settings")
    }
}

@MainActor
enum ProjectFolderPicker {
    static func chooseGitBackedWorkspace() -> String? {
        let panel = NSOpenPanel()
        panel.title = "Choose a Git Repository"
        panel.message = "Choose a folder inside a Git repository to continue."
        panel.prompt = "Choose Workspace"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        panel.resolvesAliases = true
        return panel.runModal() == .OK ? panel.url?.standardizedFileURL.path : nil
    }
}
