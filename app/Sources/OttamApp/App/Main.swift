import AppKit
import Darwin
import OttamBackendClient
import OSLog
import SwiftUI

private let appLifecycleLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "AppLifecycle"
)

@main
@MainActor
struct OttamMain: App {
    @NSApplicationDelegateAdaptor(OttamApplicationDelegate.self) private var appDelegate
    @State private var runtime: OttamAppRuntime

    init() {
        // Before anything else can crash or exit, and before this launch's
        // marker replaces the previous session's.
        ExitDiagnostics.install()
        let runtime = OttamAppRuntime()
        _runtime = State(initialValue: runtime)
        WorkspaceWindowRestoration.runtime = runtime
        TerminationSignalHandler.install()
        // Before any scene body runs. The chooser opens ahead of every
        // workspace window, so applying the stored appearance from a window's
        // own setup left it — and only it — rendering in system appearance.
        AppearanceSettings.applyStored()
        // Same reasoning as the appearance call above: the bundled faces have to
        // be resolvable before the first scene body, or a content surface paints
        // once in the system font before the family arrives.
        AppTypography.registerBundledFonts()
        // After registration, so the first terminal config already names a
        // resolvable family.
        TerminalFontBridge.start()
        // Tooltips carry text the UI truncates (command descriptions), so they
        // come up sooner than AppKit's ~1 s. AppKit has no per-view delay;
        // this registered default is app-wide and yields to a user's own value.
        UserDefaults.standard.register(defaults: ["NSInitialToolTipDelay": 400])
        appLifecycleLogger.notice("Ottam app started")
    }

    var body: some Scene {
        Window("Welcome to Ottam", id: "workspaceChooser") {
            WorkspaceChooserView(runtime: runtime)
                .windowMinimizeBehavior(.disabled)
                .windowFullScreenBehavior(.disabled)
        }
        .defaultSize(
            width: WorkspaceChooserView.windowSize.width,
            height: WorkspaceChooserView.windowSize.height
        )
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        // Onboarding opens in its place until it finishes, then opens it.
        .defaultLaunchBehavior(runtime.onboardingPending ? .suppressed : .automatic)
        // App-wide; they act on the main workspace window through the
        // registry. Workspace windows are AppKit's (WorkspaceWindowController).
        .commands {
            OttamCommands(runtime: runtime)
        }

        // First launch only. No close button: quitting is the way out, and
        // the next launch resumes.
        Window("Set Up Ottam", id: OnboardingView.windowID) {
            OnboardingView(runtime: runtime)
                .windowMinimizeBehavior(.disabled)
                .windowFullScreenBehavior(.disabled)
                .windowDismissBehavior(.disabled)
        }
        .defaultSize(width: OnboardingView.windowSize.width, height: OnboardingView.windowSize.height)
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(runtime.onboardingPending ? .presented : .suppressed)

        Window("Settings", id: "settings") {
            SettingsView(runtime: runtime)
        }
        .defaultSize(width: 980, height: 680)
        .windowResizability(.contentMinSize)

#if DEBUG
        // Native Markdown workbench: fixture gallery and hosted-card
        // interaction through the production AppKit transcript controller.
        // Debug menu → Markdown Engine Lab.
        Window("Markdown Engine Lab", id: "markdownEngineLab") {
            MarkdownEngineLabView()
        }
        .defaultSize(width: 1100, height: 720)
        .windowResizability(.contentMinSize)
#endif
    }
}

@MainActor
final class OttamApplicationDelegate: NSObject, NSApplicationDelegate {
    private var quitTask: Task<Void, Never>?
    private var waitingPanel: NSPanel?
    private var waitingPanelRefreshing = false
    private var quitMonitor: Task<Void, Never>?
    private var quitPanelDelay: Task<Void, Never>?
    private var quitOperationKey: String?

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let runtime = WorkspaceWindowRestoration.runtime else { return .terminateNow }
        guard quitTask == nil else { return .terminateLater }
        let operationKey = UUID().uuidString
        quitOperationKey = operationKey
        quitPanelDelay = Task { @MainActor in
            do { try await Task.sleep(for: .milliseconds(300)) } catch { return }
            guard !Task.isCancelled else { return }
            showWaitingPanel(refreshing: false)
        }
        quitMonitor = Task { @MainActor in
            while !Task.isCancelled {
                if let readiness = try? await runtime.client.shutdownReadinessIfRunning(), !Task.isCancelled, !readiness.ready { showWaitingPanel() }
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
        quitTask = Task { @MainActor in
            do {
                while true {
                    try Task.checkCancellation()
                    let readiness = try await runtime.client.shutdownReadinessIfRunning()
                    try Task.checkCancellation()
                    if readiness?.ready != false { break }
                    showWaitingPanel()
                    try await Task.sleep(for: .milliseconds(100))
                }
                try Task.checkCancellation()
                try await runtime.client.prepareShutdownIfRunning(operationKey: operationKey)
                try Task.checkCancellation()
                finishQuitPreparation()
                sender.reply(toApplicationShouldTerminate: true)
            } catch {
                Task { try? await runtime.client.cancelAgentShutdown(operationKey: operationKey) }
                finishQuitPreparation()
                sender.reply(toApplicationShouldTerminate: false)
                if !(error is CancellationError) && !Task.isCancelled {
                    let alert = NSAlert()
                    alert.messageText = "Couldn’t safely quit Ottam"
                    alert.informativeText = error.localizedDescription
                    alert.runModal()
                }
            }
        }
        return .terminateLater
    }

    private func finishQuitPreparation() {
        quitMonitor?.cancel()
        quitMonitor = nil
        quitPanelDelay?.cancel()
        quitPanelDelay = nil
        quitOperationKey = nil
        waitingPanel?.close()
        waitingPanel = nil
        waitingPanelRefreshing = false
        quitTask = nil
    }

    private func waitingView(refreshing: Bool) -> AgentShutdownWaitingView {
        AgentShutdownWaitingView(refreshing: refreshing) { [weak self] in
            guard let self else { return }
            self.quitTask?.cancel()
            if let key = self.quitOperationKey, let runtime = WorkspaceWindowRestoration.runtime {
                Task { try? await runtime.client.cancelAgentShutdown(operationKey: key) }
            }
        }
    }

    private func showWaitingPanel(refreshing: Bool = true) {
        if let waitingPanel {
            if refreshing && !waitingPanelRefreshing {
                waitingPanel.contentView = NSHostingView(rootView: waitingView(refreshing: true))
                waitingPanelRefreshing = true
            }
            return
        }
        let panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 440, height: 170), styleMask: [.titled], backing: .buffered, defer: false)
        panel.title = "Quit Ottam"
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(rootView: waitingView(refreshing: refreshing))
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        waitingPanel = panel
        waitingPanelRefreshing = refreshing
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        ExitDiagnostics.subscribeToCrashDiagnostics()
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Only reached once `applicationShouldTerminate` approved the quit.
        ExitDiagnostics.recordGracefulQuit()
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        true
    }
}
