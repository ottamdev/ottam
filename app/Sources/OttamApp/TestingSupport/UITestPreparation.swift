#if DEBUG
import AppKit
import OttamBackendClient
import Darwin
import OSLog
import SwiftUI

@MainActor
enum UITestPreparation {
    nonisolated private static let logger = Logger(
        subsystem: "dev.ottam.Ottam",
        category: "UITestPreparation"
    )

    static let request = PreparationRequest.fromEnvironment()
    static var createsEmptyWorkspace: Bool { request?.createEmptyWorkspace == true }
    /// Whether this process was launched by the scenario runner at all. Gates
    /// the measurement-only affordances, which must not exist for a real user.
    static var isActive: Bool { request != nil }
    static var requestedWorkspaceCount: Int { request?.workspaceCount ?? 1 }
    static var additionalProjectPaths: [String] { request?.additionalProjectPaths ?? [] }

    private static var workspacePreparationStarted = false
    private static var workspaceSucceeded: Bool?
    private static var primaryWorkspaceID: WorkspaceID?
    private static var workspaceRegistrations: [WorkspaceID: WorkspaceRegistration] = [:]
    private static var chooserPreparationStarted = false
    private static var chooserSucceeded: Bool?
    private static var settingsPreparationStarted = false
    private static var settingsSucceeded: Bool?
    private static var markdownLabPreparationStarted = false
    private static var markdownLabSucceeded: Bool?

    static var needsWorkspacePreparation: Bool {
        guard let request else { return false }
        // The lab window is opened from the workspace registration chain, so a
        // lab capture needs a workspace window even without a Chat capture.
        return request.preparesWorkspace || request.preparesMarkdownLab
    }

    static func publishFixture(
        name: String,
        status: String,
        fields: [String: Any]
    ) {
        guard let stateDirectory = request?.externalStateDirectory else {
            return
        }
        do {
            let directory = stateDirectory
                .appendingPathComponent("fixtures", isDirectory: true)
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true
            )
            var object = fields
            object["schemaVersion"] = 1
            object["status"] = status
            let data = try JSONSerialization.data(
                withJSONObject: object,
                options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
            )
            try data.write(
                to: directory.appendingPathComponent("\(name).json"),
                options: .atomic
            )
        } catch {
            log("failed to publish fixture \(name): \(error.localizedDescription)")
        }
    }

    static func prepareChooserIfRequested() async {
        guard let request,
              request.preparesWorkspaceChooser,
              !chooserPreparationStarted
        else { return }
        chooserPreparationStarted = true

        guard let stateDirectory = request.externalStateDirectory,
              let window = await waitForChooserWindow()
        else {
            log("UI-test chooser preparation requires an external driver and window")
            chooserSucceeded = false
            finishIfReady()
            return
        }

        await settle(window: window)
        let name = "workspace-chooser"
        chooserSucceeded = await publishWindowReadiness(
            name: name,
            window: window,
            stateDirectory: stateDirectory
        )
        finishIfReady()
    }

    static func registerWorkspaceIfRequested(
        workspaceID: WorkspaceID,
        window: NSWindow,
        workspace: WorkspaceStore,
        diffModel: DiffReviewModel,
        chatWorkspace: ChatWorkspaceModel,
        openSettings: @escaping () -> Void,
        openMarkdownLab: @escaping () -> Void
    ) {
        guard request != nil else { return }
        workspaceRegistrations[workspaceID] = WorkspaceRegistration(
            window: window,
            workspace: workspace,
            diffModel: diffModel,
            chatWorkspace: chatWorkspace,
            openSettings: openSettings,
            openMarkdownLab: openMarkdownLab
        )
        startWorkspacePreparationIfReady()
    }

    static func workspaceLaunchSucceeded(
        primaryWorkspaceID: WorkspaceID,
        workspaceIDs: [WorkspaceID]
    ) {
        self.primaryWorkspaceID = primaryWorkspaceID
        publishFixture(
            name: "workspace-preparation",
            status: "passed",
            fields: [
                "message": "workspace launch completed",
                "primaryWorkspaceID": primaryWorkspaceID.rawValue,
                "workspaceIDs": workspaceIDs.map(\.rawValue),
            ]
        )
        startWorkspacePreparationIfReady()
    }

    static func workspaceLaunchFailed(message: String) {
        workspaceSucceeded = false
        publishFixture(
            name: "workspace-preparation",
            status: "fixture_failed",
            fields: ["message": message]
        )
        finishIfReady()
    }

    static func prepareSettingsIfRequested(window: NSWindow) {
        guard let request,
              request.preparesSettings,
              !settingsPreparationStarted
        else { return }
        settingsPreparationStarted = true

        Task { @MainActor in
            guard let externalStateDirectory = request.externalStateDirectory else {
                log("Settings capture requires the external UI-test driver")
                settingsSucceeded = false
                finishIfReady()
                return
            }
            await settle(window: window)
            settingsSucceeded = await publishWindowReadiness(
                name: "settings-window",
                window: window,
                stateDirectory: externalStateDirectory
            )
            finishIfReady()
        }
    }

    /// The fixture the markdown lab should open on for this capture, if the
    /// harness named one. Read at the lab view's init, before any capture.
    static var requestedMarkdownLabFixture: String? {
        request?.markdownLabFixture
    }

    /// The lab tab a capture wants open, if the harness named one.
    static var requestedMarkdownLabTab: String? {
        request?.markdownLabTab
    }

    static func prepareMarkdownLabIfRequested(window: NSWindow) {
        guard let request,
              request.preparesMarkdownLab,
              !markdownLabPreparationStarted
        else { return }
        markdownLabPreparationStarted = true

        Task { @MainActor in
            guard let externalStateDirectory = request.externalStateDirectory else {
                log("Markdown lab capture requires the external UI-test driver")
                markdownLabSucceeded = false
                finishIfReady()
                return
            }
            // The lab is its own capture surface; give it the deterministic
            // frame the workspace window gets.
            applyFrameSize(request.windowFrameSize, to: window)
            await settle(window: window)
            markdownLabSucceeded = await publishWindowReadiness(
                name: "markdown-lab-window",
                window: window,
                stateDirectory: externalStateDirectory
            )
            finishIfReady()
        }
    }

    private static func waitForChooserWindow() async -> NSWindow? {
        for _ in 0..<40 {
            if let window = NSApp.windows.first(where: { window in
                window.isVisible && window.title == "Welcome to Ottam"
            }) {
                return window
            }
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
        return nil
    }

    private static func startWorkspacePreparationIfReady() {
        guard let request,
              !workspacePreparationStarted,
              let primaryWorkspaceID,
              let registration = workspaceRegistrations[primaryWorkspaceID]
        else {
            return
        }
        workspacePreparationStarted = true
        Task { @MainActor in
            workspaceSucceeded = await prepareWorkspace(
                request: request,
                window: registration.window,
                workspace: registration.workspace,
                diffModel: registration.diffModel,
                chatWorkspace: registration.chatWorkspace,
                openSettings: registration.openSettings,
                openMarkdownLab: registration.openMarkdownLab
            )
            finishIfReady()
        }
    }

    private struct WorkspaceRegistration {
        let window: NSWindow
        let workspace: WorkspaceStore
        let diffModel: DiffReviewModel
        let chatWorkspace: ChatWorkspaceModel
        let openSettings: () -> Void
        let openMarkdownLab: () -> Void
    }

    private static func prepareWorkspace(
        request: PreparationRequest,
        window: NSWindow,
        workspace: WorkspaceStore,
        diffModel: DiffReviewModel,
        chatWorkspace: ChatWorkspaceModel,
        openSettings: @escaping () -> Void,
        openMarkdownLab: @escaping () -> Void
    ) async -> Bool {
        guard let externalStateDirectory = request.externalStateDirectory else {
            log("route preparation requires the external UI-test driver")
            return false
        }

        applyFrameSize(request.windowFrameSize, to: window)
        applyAppearance(request.appearance, to: window)
        await settle(window: window)

        var succeeded = true
        if request.preparesWorkspace {
            if await prepareChat(
                request: request,
                workspace: workspace,
                diffModel: diffModel,
                chatWorkspace: chatWorkspace
            ) {
                await settle(window: window)
                // The harness waits on the Chat page's old route name.
                succeeded = await publishWindowReadiness(
                    name: "route-chat",
                    window: window,
                    stateDirectory: externalStateDirectory
                )
            } else {
                succeeded = false
            }
        }
        if request.preparesSettings {
            openSettings()
        }
        if request.preparesMarkdownLab {
            openMarkdownLab()
        }

        return succeeded
    }

    /// False when the scenario's preferred engine never became ready; the
    /// page then goes uncaptured.
    private static func prepareChat(
        request: PreparationRequest,
        workspace: WorkspaceStore,
        diffModel: DiffReviewModel,
        chatWorkspace: ChatWorkspaceModel
    ) async -> Bool {
        chatWorkspace.configure(
            workspaceID: workspace.workspaceID,
            workspaceFiles: workspace.workspaceFiles,
            projects: workspace.projects,
            projectSummary: workspace.projectSummary,
            backend: diffModel
        )
        // Join the view-owned initial load before applying scenario
        // selection. Otherwise that older task can finish after
        // preparation and restore the persisted/default engine over a
        // deterministic fixture engine such as `test.native`.
        await chatWorkspace.loadIfNeeded(backend: diffModel)
        if let preferredEngine = ProcessInfo.processInfo.environment[
            "OTTAM_UI_TEST_PREFERRED_ENGINE"
        ] {
            let deadline = ContinuousClock.now + .seconds(10)
            var preferredEngineReady = false
            while ContinuousClock.now < deadline {
                await chatWorkspace.reload(backend: diffModel)
                if chatWorkspace.engines.contains(where: {
                    $0.engineID.rawValue == preferredEngine
                }) {
                    chatWorkspace.selectedEngineID = EngineID(preferredEngine)
                    preferredEngineReady = true
                    break
                }
                try? await Task.sleep(for: .milliseconds(100))
            }
            guard preferredEngineReady else {
                publishFixture(
                    name: "chat-engine-selection",
                    status: "failed",
                    fields: [
                        "preferredEngine": preferredEngine,
                        "availableEngines":
                            chatWorkspace.engines.map(\.engineID.rawValue),
                    ]
                )
                log("preferred UI-test engine did not become ready: \(preferredEngine)")
                return false
            }
            publishFixture(
                name: "chat-engine-selection",
                status: "passed",
                fields: [
                    "preferredEngine": preferredEngine,
                    "selectedEngine":
                        chatWorkspace.selectedEngineID?.rawValue ?? "",
                    "availableEngines":
                        chatWorkspace.engines.map(\.engineID.rawValue),
                ]
            )
        } else {
            await chatWorkspace.reload(backend: diffModel)
        }
        if let conversationSelection = request.conversationSelection {
            // The capture workspace starts empty; seed a small history so the
            // sidebar sections (project group + workspace-files bucket) render.
            if chatWorkspace.conversations.isEmpty {
                await seedConversationsForCapture(
                    workspace: workspace,
                    diffModel: diffModel
                )
                await chatWorkspace.reload(backend: diffModel)
            }
            await chatWorkspace.selectForCapture(
                matching: conversationSelection == "1" ? nil : conversationSelection,
                backend: diffModel
            )
        }
        return true
    }

    private static func publishWindowReadiness(
        name: String,
        window: NSWindow,
        stateDirectory: URL
    ) async -> Bool {
        if let rawDelay = ProcessInfo.processInfo.environment[
            "OTTAM_UI_TEST_READINESS_DELAY_MS"
        ],
        let requestedDelay = UInt64(rawDelay) {
            let boundedDelay = min(requestedDelay, 5_000)
            try? await Task.sleep(for: .milliseconds(Int64(boundedDelay)))
        }
        do {
            try FileManager.default.createDirectory(
                at: stateDirectory,
                withIntermediateDirectories: true
            )
            let readyURL = stateDirectory.appendingPathComponent("\(name).window-id")
            try String(window.windowNumber).write(to: readyURL, atomically: true, encoding: .utf8)
        } catch {
            log("failed to publish external capture state for \(name): \(error.localizedDescription)")
            return false
        }

        let doneURL = stateDirectory.appendingPathComponent("\(name).done")
        let failedURL = stateDirectory.appendingPathComponent("\(name).failed")
        for _ in 0..<600 {
            if FileManager.default.fileExists(atPath: failedURL.path) {
                log("external capture failed for \(name)")
                return false
            }
            if FileManager.default.fileExists(atPath: doneURL.path) {
                return true
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }

        log("timed out waiting for external capture of \(name)")
        return false
    }

    /// `nil` restores the system default rather than pinning one, so an
    /// appearance-less scenario is unaffected. Set both authorities in the
    /// isolated test process: SwiftUI can resolve its color scheme before the
    /// window override arrives, while AppKit chrome resolves from the window.
    private static func applyAppearance(_ name: NSAppearance.Name?, to window: NSWindow) {
        let appearance = name.flatMap(NSAppearance.init(named:))
        NSApp.appearance = appearance
        window.appearance = appearance
    }

    private static func applyFrameSize(_ frameSize: NSSize?, to window: NSWindow) {
        guard let frameSize else {
            return
        }

        let frame = NSRect(
            x: window.frame.minX,
            y: window.frame.maxY - frameSize.height,
            width: frameSize.width,
            height: frameSize.height
        )
        window.setFrame(frame, display: true)
    }

    /// Creates a deterministic small history in the capture workspace: enough
    /// project conversations to exercise the sidebar's "Show more" fold plus a
    /// couple of workspace-files chats for the loose bucket.
    private static func seedConversationsForCapture(
        workspace: WorkspaceStore,
        diffModel: DiffReviewModel
    ) async {
        let projectTitles = [
            "Persistence redesign spec",
            "Sidebar navigation redesign",
            "Crash investigation",
            "Workspace chooser flicker",
            "Model selection support",
            "Session loading flicker",
        ]
        let looseTitles = [
            "Brainstorm a product name",
            "Evaluate idea viability",
        ]
        do {
            if !workspace.projects.isEmpty {
                for (index, title) in projectTitles.enumerated() {
                    let project = workspace.projects[index % workspace.projects.count]
                    let selection = WorkingRootSelection.project(
                        projectID: project.project.projectID,
                        sourceID: project.source.projectSourceID
                    )
                    _ = try await diffModel.createConversation(
                        workspaceID: workspace.workspaceID,
                        workingRoot: selection,
                        title: title
                    )
                }
            }
            for title in looseTitles {
                _ = try await diffModel.createConversation(
                    workspaceID: workspace.workspaceID,
                    workingRoot: .workspaceFiles,
                    title: title
                )
            }
        } catch {
            log("failed to seed conversations for capture: \(error.localizedDescription)")
        }
    }

    private static func settle(window: NSWindow) async {
        if request?.quiet == true {
            // A non-key test window still needs to be ordered onto the Window
            // Server so ScreenCaptureKit and AX can address it authoritatively.
            // `orderFront(_:)` may leave an inactive app's window omitted from
            // the AXWindows collection on recent macOS releases.
            window.orderFrontRegardless()
        } else {
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
        }
        for _ in 0..<4 {
            window.displayIfNeeded()
            window.contentViewController?.view.layoutSubtreeIfNeeded()
            window.contentView?.layoutSubtreeIfNeeded()
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
    }

    private static func finishIfReady() {
        guard let request else {
            return
        }

        guard !request.preparesWorkspace || workspaceSucceeded != nil else {
            return
        }
        guard !request.preparesWorkspaceChooser || chooserSucceeded != nil else {
            return
        }
        guard !request.preparesSettings || settingsSucceeded != nil else {
            return
        }
        guard !request.preparesMarkdownLab || markdownLabSucceeded != nil else {
            return
        }

        let succeeded = workspaceSucceeded != false
            && chooserSucceeded != false
            && settingsSucceeded != false
            && markdownLabSucceeded != false

        if request.exitWhenFinished {
            ExitDiagnostics.exitDeliberately(succeeded ? EXIT_SUCCESS : EXIT_FAILURE)
        }
    }

    nonisolated static func log(_ message: String) {
        logger.info("\(message, privacy: .public)")
        fputs("Ottam UI test preparation: \(message)\n", stderr)
    }

}
#endif
