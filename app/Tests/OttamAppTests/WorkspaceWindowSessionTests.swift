import OttamBackendClient
import Foundation
import SwiftUI
import Testing
@testable import OttamApp

@MainActor
@Suite("Workspace window session")
struct WorkspaceWindowSessionTests {
    private func session(_ id: String = "session-tests") -> WorkspaceWindowSession {
        let client = OttamCoreProcessClient(executableURL: URL(fileURLWithPath: "/nonexistent/core"))
        // The runtime decides onboarding at init, which records it in the
        // store's home; keep that out of the developer's real one.
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("window-session-\(UUID().uuidString)")
        return WorkspaceWindowSession(
            workspaceID: WorkspaceID(id),
            runtime: OttamAppRuntime(client: client, onboardingStore: OnboardingStore(directory: home))
        )
    }

    /// Observation callbacks re-arm on a later main-actor turn.
    private func settle() async {
        for _ in 0..<2 {
            await withCheckedContinuation { continuation in
                DispatchQueue.main.async { continuation.resume() }
            }
        }
    }

    /// Settles until `condition` holds, within a bound.
    private func settle(until condition: () -> Bool) async {
        let deadline = ContinuousClock.now + .seconds(3)
        while !condition(), ContinuousClock.now < deadline {
            await settle()
        }
    }

    /// A window mounts the root view, whose startup fails against the missing
    /// Core on turns of its own: it fails the project open and clears the chat
    /// selection. Window tests wait it out, then open a project for tools.
    private func settleStartup(_ session: WorkspaceWindowSession) async {
        await settle { session.workspace.projectOpenError != nil && session.chatWorkspace.errorText != nil }
        #expect(session.workspace.projectOpenError != nil)
        let projectID = ProjectID("project")
        session.workspace.apply(projectSummary: ResolvedProject(
            project: ProjectSummary(projectID: projectID, displayName: "project", createdAt: 0, updatedAt: 0, lastOpenedAt: 0),
            source: ProjectSourceSummary(
                projectSourceID: ProjectSourceID("source"),
                projectID: projectID,
                kind: .localGit,
                displayName: "project",
                canonicalPath: "/tmp/project",
                createdAt: 0,
                updatedAt: 0
            )
        ))
        await settle()
    }

    /// A workspace whose window and split autosaves are removed afterwards.
    private func withScratchWorkspace(_ body: (String) async throws -> Void) async rethrows {
        let id = "tools-\(UUID().uuidString.lowercased())"
        defer {
            UserDefaults.standard.removeObject(forKey: "NSSplitView Subview Frames OttamWorkspace.Split.\(id)")
            UserDefaults.standard.removeObject(forKey: "NSWindow Frame OttamWorkspace.\(id)")
        }
        try await body(id)
    }

    @Test("working roots round-trip through their restoration encoding")
    func workingRootEncoding() {
        let project = WorkingRootSelection.project(
            projectID: ProjectID("project"),
            sourceID: ProjectSourceID("source")
        )
        for selection in [WorkingRootSelection.workspaceFiles, project] {
            #expect(WorkspaceWindowSession.decode(WorkspaceWindowSession.encode(selection)) == selection)
        }
        #expect(WorkspaceWindowSession.decode("") == nil)
        #expect(WorkspaceWindowSession.decode("project:only-two") == nil)
        #expect(WorkspaceWindowSession.decode("folder:a:b") == nil)
    }

    @Test("a project error hides the sidebar without saving it, then restores the user's state")
    func projectErrorSidebar() {
        let split = WorkspaceSplitViewController(session: session())
        _ = split.view
        let autosaveName = split.splitView.autosaveName
        #expect(autosaveName != nil)

        for userCollapsed in [false, true] {
            split.sidebarItem.isCollapsed = userCollapsed
            split.setProjectErrorShown(true)
            #expect(split.sidebarItem.isCollapsed)
            #expect(split.splitView.autosaveName == nil)
            split.setProjectErrorShown(false)
            #expect(split.sidebarItem.isCollapsed == userCollapsed)
            #expect(split.splitView.autosaveName == autosaveName)
        }
    }

    @Test("the sidebar menu command collapses the window's sidebar")
    func toggleSidebarAction() {
        let session = session()
        let split = WorkspaceSplitViewController(session: session)
        _ = split.view
        let wasCollapsed = split.sidebarItem.isCollapsed
        session.windowActions.toggleSidebar()
        #expect(split.sidebarItem.isCollapsed != wasCollapsed)
    }

    @Test("window actions drive this session's models")
    func windowActions() {
        let session = session()
        let actions = session.windowActions
        #expect(actions.workspace === session.workspace)

        actions.archiveWorkspace()
        #expect(session.isArchiveConfirmationPresented)
    }

    @Test("window restoration round-trips through a secure coder")
    func restorationCoding() throws {
        var state = WorkspaceRestorationState()
        state.projectID = "project"
        state.chatRoot = "workspace"
        state.conversationID = "conversation"
        state.collapsedSections = "chats,project:a"

        let archiver = NSKeyedArchiver(requiringSecureCoding: true)
        WorkspaceWindowRestoration.encode(WorkspaceID("restored"), state, with: archiver)
        archiver.finishEncoding()
        let unarchiver = try NSKeyedUnarchiver(forReadingFrom: archiver.encodedData)
        unarchiver.requiresSecureCoding = true
        let decoded = try #require(WorkspaceWindowRestoration.decode(from: unarchiver))

        #expect(decoded.workspaceID == WorkspaceID("restored"))
        #expect(decoded.state == state)
    }

    @Test("the main workspace window publishes its actions to the menu commands")
    func activeActions() {
        let first = session()
        let registry = first.runtime.windows
        let second = WorkspaceWindowSession(workspaceID: WorkspaceID("second"), runtime: first.runtime)
        let firstWindow = WorkspaceWindowController(session: first, registry: registry)
        let secondWindow = WorkspaceWindowController(session: second, registry: registry)
        let notification = Notification(name: NSWindow.didBecomeMainNotification)

        firstWindow.windowDidBecomeMain(notification)
        #expect(registry.activeActions === first.windowActions)
        secondWindow.windowDidBecomeMain(notification)
        firstWindow.windowDidResignMain(notification)
        #expect(registry.activeActions === second.windowActions)
        secondWindow.windowDidResignMain(notification)
        #expect(registry.activeActions == nil)
    }

    @Test("closing a window releases it, then drops its content after the event")
    func closeReleasesWindow() async {
        let runtime = session().runtime
        let registry = runtime.windows
        let controller = registry.open(WorkspaceID("closing"), runtime: runtime, show: false)
        controller.windowDidBecomeMain(Notification(name: NSWindow.didBecomeMainNotification))
        #expect(registry.openWorkspaceCount == 1)

        controller.close()
        #expect(controller.session.isClosed)
        #expect(registry.openWorkspaceCount == 0)
        #expect(registry.activeActions == nil)
        #expect(controller.window?.contentViewController != nil)
        await withCheckedContinuation { continuation in
            DispatchQueue.main.async { continuation.resume() }
        }
        #expect(controller.window?.contentViewController == nil)
    }

    @Test("startup still in flight when the window closes leaves its models alone")
    func startupStopsAtClose() async {
        let session = session()
        let preparing = Task { await session.prepareWorkspace() }
        session.tearDown()
        await preparing.value
        // Against the missing Core, a live startup reports the failure and
        // marks the project unavailable.
        #expect(session.coreFailure == nil)
        #expect(session.workspace.projectOpenError == nil)
        #expect(!session.didResolveLaunchProject)
    }

    // MARK: - Terminal tabs

    private func project(_ name: String) -> WorkspaceProjectSummary {
        let projectID = ProjectID("project-\(name)")
        return WorkspaceProjectSummary(
            project: ProjectSummary(projectID: projectID, displayName: name, createdAt: 0, updatedAt: 0, lastOpenedAt: 0),
            source: ProjectSourceSummary(
                projectSourceID: ProjectSourceID("source-\(name)"),
                projectID: projectID,
                kind: .localGit,
                displayName: name,
                canonicalPath: "/tmp/\(name)",
                createdAt: 0,
                updatedAt: 0
            ),
            sortOrder: 0,
            addedAt: 0
        )
    }

    private func conversation(_ title: String, in member: WorkspaceProjectSummary) -> ConversationSummary {
        ConversationSummary(
            conversationID: ConversationID(title),
            workspaceID: WorkspaceID("session-tests"),
            workingRoot: WorkingRootSummary(
                kind: .project,
                displayName: member.project.displayName,
                canonicalPath: member.source.canonicalPath,
                projectID: member.project.projectID,
                sourceID: member.source.projectSourceID,
                availability: .available
            ),
            title: title,
            createdAt: "2026-09-30T00:00:00Z",
            updatedAt: "2026-09-30T00:00:00Z",
            alwaysAllowToolActions: false
        )
    }

    private func detail(
        _ session: WorkspaceWindowSession,
        named name: String = "Tests",
        projects: [WorkspaceProjectSummary] = []
    ) -> WorkspaceDetail {
        WorkspaceDetail(
            workspace: WorkspaceSummary(
                workspaceID: session.workspaceID, displayName: name, createdAt: 0, updatedAt: 0, lastOpenedAt: 0
            ),
            workspaceFiles: WorkingRootSummary(
                kind: .workspaceFiles, displayName: "Workspace files", canonicalPath: "/tmp/files", availability: .available
            ),
            projects: projects
        )
    }

    /// A session whose workspace holds `projects`, with `conversations` loaded.
    private func terminalSession(
        projects: [WorkspaceProjectSummary],
        conversations: [ConversationSummary]
    ) -> WorkspaceWindowSession {
        let session = session()
        session.chatWorkspace.apply(conversations: conversations)
        configureTerminals(session, projects: projects)
        return session
    }

    private func configureTerminals(_ session: WorkspaceWindowSession, projects: [WorkspaceProjectSummary]) {
        session.terminalWorkspace.configure(
            workspaceID: session.workspaceID,
            workspaceFiles: nil,
            projects: projects,
            backend: session.diffModel
        )
    }

    @Test("Open Terminal adds a focused tab in the selected conversation, and closing the tab closes its terminal")
    func openTerminal() {
        let alpha = project("alpha")
        let chat = conversation("chat", in: alpha)
        let session = terminalSession(projects: [alpha], conversations: [chat])
        session.chatWorkspace.selectedConversationID = chat.conversationID

        session.openTerminal()
        // The menu command.
        session.windowActions.openTerminal()
        let records = session.terminalWorkspace.records
        let tabs = records.map { ConversationTool.terminal($0.id) }
        let state = session.tools.state(for: chat.conversationID)
        #expect(records.map(\.cwd) == ["/tmp/alpha", "/tmp/alpha"])
        #expect(state.tools == tabs)
        #expect(state.selected == tabs.last)
        #expect(tabs.map(state.ordinal(of:)) == [1, 2])
        #expect(session.isToolsPaneShown)
        #expect(session.tools.consumeFocus(for: tabs[1], in: chat.conversationID))

        session.closeTool(tabs[1], in: chat.conversationID)
        #expect(session.terminalWorkspace.records == [records[0]])
        #expect(session.tools.state(for: chat.conversationID).selected == tabs[0])

        session.tearDown()
        #expect(session.terminalWorkspace.records.isEmpty)
    }

    @Test("a draft, or a conversation whose project left the workspace, cannot open a terminal")
    func openTerminalNeedsRoot() {
        let alpha = project("alpha")
        let chat = conversation("chat", in: alpha)
        let session = terminalSession(projects: [alpha], conversations: [chat])
        #expect(!session.windowActions.canOpenTerminal)
        session.chatWorkspace.selectedConversationID = chat.conversationID
        #expect(session.windowActions.canOpenTerminal)

        configureTerminals(session, projects: [project("beta")])
        #expect(!session.windowActions.canOpenTerminal)
        session.openTerminal()
        #expect(session.terminalWorkspace.records.isEmpty)
        #expect(session.tools.state(for: chat.conversationID).tools.isEmpty)
    }

    @Test("Remove Project acts on the draft's or conversation's project, never a stale selection")
    func removeProjectTarget() {
        let alpha = project("alpha")
        let beta = project("beta")
        let chat = conversation("chat", in: alpha)
        let departed = conversation("departed", in: project("gamma"))
        let session = terminalSession(projects: [alpha, beta], conversations: [chat, departed])
        session.workspace.apply(
            detail: detail(session, projects: [alpha, beta]),
            preferredProjectID: alpha.project.projectID
        )

        session.chatWorkspace.draftWorkingRoot = .project(
            projectID: beta.project.projectID,
            sourceID: beta.source.projectSourceID
        )
        #expect(session.contextProjectID == beta.project.projectID)
        #expect(session.windowActions.canRemoveProject)

        session.chatWorkspace.draftWorkingRoot = .workspaceFiles
        #expect(!session.windowActions.canRemoveProject)

        session.chatWorkspace.selectedConversationID = chat.conversationID
        #expect(session.contextProjectID == alpha.project.projectID)

        session.chatWorkspace.selectedConversationID = departed.conversationID
        #expect(!session.windowActions.canRemoveProject)
    }

    @Test("Show Next/Previous Tab cycle a shown pane's tabs; focus follows unless a view outside the pane holds it")
    func selectAdjacentTool() throws {
        let session = session()
        let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
        let window = try #require(controller.window)
        let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
        let actions = session.windowActions
        let conversation = ConversationID("conversation")
        let term = ConversationTool.terminal(TerminalID("term"))
        session.chatWorkspace.selectedConversationID = conversation
        session.tools.show(.review, in: conversation)
        #expect(!actions.canSelectAdjacentTool)
        session.tools.show(term, in: conversation)
        #expect(actions.canSelectAdjacentTool)
        @MainActor func select(_ offset: Int, focusedIn column: NSSplitViewItem?) -> ConversationTool? {
            let editor = NSTextView()
            column?.viewController.view.addSubview(editor)
            defer { editor.removeFromSuperview() }
            #expect(window.makeFirstResponder(column == nil ? nil : editor))
            actions.selectAdjacentTool(offset)
            return session.tools.state(for: conversation).selected
        }

        // Review has nothing to focus, so leaving a terminal for it leaves
        // focus nowhere; the terminal takes it back.
        #expect(select(1, focusedIn: nil) == .review)
        #expect(session.tools.consumeFocus(for: .review, in: conversation))
        #expect(select(1, focusedIn: split.toolsItem) == term)
        #expect(session.tools.consumeFocus(for: term, in: conversation))
        // The composer keeps focus.
        #expect(select(-1, focusedIn: split.mainItem) == .review)
        #expect(!session.tools.consumeFocus(for: .review, in: conversation))

        session.windowActions.toggleTools()
        #expect(!actions.canSelectAdjacentTool)
        actions.selectAdjacentTool(1)
        #expect(session.tools.state(for: conversation).selected == .review)
    }

    @Test("a replaced Core closes terminal tabs and keeps Review")
    func resetTerminals() {
        let alpha = project("alpha")
        let chat = conversation("chat", in: alpha)
        let session = terminalSession(projects: [alpha], conversations: [chat])
        session.chatWorkspace.selectedConversationID = chat.conversationID
        session.openReview()
        session.openTerminal()

        session.resetTerminals()
        #expect(session.terminalWorkspace.records.isEmpty)
        #expect(session.tools.state(for: chat.conversationID).tools == [.review])
        #expect(session.tools.state(for: chat.conversationID).selected == .review)
    }

    @Test("deleting a conversation closes its terminals and forgets its tabs")
    func deleteClosesTerminals() {
        let alpha = project("alpha")
        let chat = conversation("chat", in: alpha)
        let other = conversation("other", in: alpha)
        let session = terminalSession(projects: [alpha], conversations: [chat, other])
        session.chatWorkspace.selectedConversationID = chat.conversationID
        session.openTerminal()
        session.chatWorkspace.selectedConversationID = other.conversationID
        session.openTerminal()
        let kept = session.terminalWorkspace.records[1]

        session.chatWorkspace.didDeleteConversation(chat.conversationID)
        #expect(session.terminalWorkspace.records == [kept])
        #expect(session.tools.state(for: chat.conversationID).tools.isEmpty)
        #expect(session.tools.state(for: other.conversationID).tools == [.terminal(kept.id)])
    }

    @Test("the toolbar holds the standard tracking items, New Chat, and Tools, sent to the window's actions")
    func toolbar() throws {
        let session = session()
        let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
        let toolbar = try #require(controller.window?.toolbar)
        #expect(toolbar.items.map(\.itemIdentifier) == [
            .flexibleSpace, .toggleSidebar, .sidebarTrackingSeparator,
            .flexibleSpace, .newChat, .toggleTools, .inspectorTrackingSeparator,
        ])
        #expect(!toolbar.allowsUserCustomization)
        #expect(toolbar.displayMode == .iconOnly)

        let newChat = try #require(toolbar.items.first { $0.itemIdentifier == .newChat })
        #expect(newChat.label == "New Chat")
        #expect(controller.validateToolbarItem(newChat))
        var performed = false
        session.windowActions.newChat = { performed = true }
        #expect(NSApp.sendAction(try #require(newChat.action), to: newChat.target, from: newChat))
        #expect(performed)

        // A draft has no tools; a conversation does.
        let tools = try #require(toolbar.items.first { $0.itemIdentifier == .toggleTools })
        #expect(tools.label == "Tools")
        #expect(!controller.validateToolbarItem(tools))
        session.chatWorkspace.selectedConversationID = ConversationID("conversation")
        #expect(controller.validateToolbarItem(tools))
        var toggled = false
        session.windowActions.toggleTools = { toggled = true }
        #expect(NSApp.sendAction(try #require(tools.action), to: tools.target, from: tools))
        #expect(toggled)
    }

    @Test("toolbar items revalidate when their state changes without an event")
    func toolbarRevalidates() async throws {
        let session = session()
        let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
        let toolbar = try #require(controller.window?.toolbar)
        let tools = try #require(toolbar.items.first { $0.itemIdentifier == .toggleTools })
        await settleStartup(session)
        toolbar.validateVisibleItems()
        #expect(!tools.isEnabled)

        session.chatWorkspace.selectedConversationID = ConversationID("conversation")
        await settle { tools.isEnabled }
        #expect(tools.isEnabled)
        session.chatWorkspace.selectedConversationID = nil
        await settle { !tools.isEnabled }
        #expect(!tools.isEnabled)
    }

    @Test("the tools pane follows the selected conversation, and a project error hides it")
    func toolsPaneFollowsConversation() async {
        let session = session()
        let split = WorkspaceSplitViewController(session: session)
        _ = split.view
        let first = ConversationID("first")
        #expect(split.toolsItem.isCollapsed)

        // The pane shows its picker until the conversation has a tab.
        session.chatWorkspace.selectedConversationID = first
        session.windowActions.toggleTools()
        await settle()
        #expect(!split.toolsItem.isCollapsed)
        #expect(session.tools.state(for: first).tools.isEmpty)

        session.chatWorkspace.selectedConversationID = ConversationID("second")
        await settle()
        #expect(split.toolsItem.isCollapsed)
        session.chatWorkspace.selectedConversationID = first
        await settle()
        #expect(!split.toolsItem.isCollapsed)

        session.windowActions.toggleTools()
        await settle()
        #expect(split.toolsItem.isCollapsed)
        session.windowActions.toggleTools()
        await settle()
        #expect(!split.toolsItem.isCollapsed)

        session.workspace.failProjectOpen("gone")
        await settle()
        #expect(split.toolsItem.isCollapsed)
        #expect(!session.windowActions.canToggleTools)
    }

    @Test("a saved expanded pane does not come back without its conversation")
    func toolsPaneIgnoresAutosave() async {
        await withScratchWorkspace { id in
            UserDefaults.standard.set([
                "0.000000, 0.000000, 300.000000, 700.000000, NO, NO",
                "300.000000, 0.000000, 300.000000, 700.000000, NO, NO",
                "600.000000, 0.000000, 320.000000, 700.000000, NO, NO",
            ], forKey: "NSSplitView Subview Frames OttamWorkspace.Split.\(id)")
            let split = WorkspaceSplitViewController(session: session(id))
            _ = split.view
            #expect(split.toolsItem.isCollapsed)
        }
    }

    @Test("showing the tools pane at the minimum width never grows the window (S6)")
    func toolsPaneNeverGrowsWindow() async throws {
        try await withScratchWorkspace { id in
            // A pane widened last launch starts from the default width.
            UserDefaults.standard.set([
                "0.000000, 0.000000, 300.000000, 700.000000, NO, NO",
                "300.000000, 0.000000, 704.000000, 700.000000, NO, NO",
                "1004.000000, 0.000000, 500.000000, 700.000000, NO, NO",
            ], forKey: "NSSplitView Subview Frames OttamWorkspace.Split.\(id)")
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            await settleStartup(session)
            window.setContentSize(WorkspaceShellViewController.minimumSize)
            window.layoutIfNeeded()
            split.splitView.setPosition(400, ofDividerAt: 0)
            window.layoutIfNeeded()
            #expect(split.splitView.arrangedSubviews[0].frame.width == 400)
            let frame = window.frame

            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            session.tools.show(.review, in: conversation)
            await settle { !split.toolsItem.isCollapsed }
            window.layoutIfNeeded()
            #expect(!split.toolsItem.isCollapsed)
            #expect(window.frame == frame)
            let tools = split.splitView.arrangedSubviews[2].frame.width
            #expect(tools >= split.toolsItem.minimumThickness)
            #expect(tools <= WorkspaceSplitViewController.toolsDefaultWidth)
        }
    }

    @Test("the tabs and Add Tool join the toolbar over the pane only while it shows a tab, and never widen it (S7)")
    func toolsTabsNeverWidenPane() async throws {
        try await withScratchWorkspace { id in
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let toolbar = try #require(window.toolbar)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            // Another window's toolbar never gains this window's tabs.
            let otherSession = self.session()
            let other = WorkspaceWindowController(session: otherSession, registry: otherSession.runtime.windows)
            let otherToolbar = try #require(other.window?.toolbar)
            #expect(otherToolbar.identifier != toolbar.identifier)
            @MainActor func tabsItem(_ toolbar: NSToolbar) -> NSToolbarItem? {
                toolbar.items.first { $0.itemIdentifier == .toolsTabs }
            }
            @MainActor func addItem(_ toolbar: NSToolbar) -> NSToolbarItem? {
                toolbar.items.first { $0.itemIdentifier == .addTool }
            }
            await settleStartup(session)
            window.setContentSize(WorkspaceShellViewController.minimumSize)
            window.layoutIfNeeded()
            #expect(tabsItem(toolbar) == nil)
            // The picker shows without tabs or Add Tool.
            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            session.tools.toggleVisibility(for: conversation)
            await settle { !split.toolsItem.isCollapsed }
            #expect(tabsItem(toolbar) == nil)
            #expect(addItem(toolbar) == nil)
            session.tools.show(.review, in: conversation)
            await settle { tabsItem(toolbar) != nil }
            window.layoutIfNeeded()
            let tabs = try #require(tabsItem(toolbar))
            #expect(!tabs.isBordered)
            // Add Tool follows the tabs, bordered as New Chat and Tools are.
            let add = try #require(toolbar.items.last as? NSMenuToolbarItem)
            #expect(add.itemIdentifier == .addTool && add.isBordered && !add.showsIndicator)
            #expect(toolbar.items.dropLast().last === tabs)
            // A pull-down never lists its first item, its title.
            #expect(add.menu.items.map(\.title) == ["", "Review", "Terminal", "Plan"])
            #expect(controller.validateMenuItem(add.menu.items[2]) == session.canOpenTerminal)
            // Plan stays listed, disabled until the conversation has a plan,
            // and reopens it after its tab closes.
            let planItem = add.menu.items[3]
            #expect(!controller.validateMenuItem(planItem))
            let plan = ChatPlan(title: "Implement this plan?", markdown: "# Plan")
            session.tools.showPlan(plan, in: conversation)
            session.closeTool(.plan, in: conversation)
            #expect(!session.tools.state(for: conversation).tools.contains(.plan))
            #expect(controller.validateMenuItem(planItem))
            NSApp.sendAction(try #require(planItem.action), to: planItem.target, from: planItem)
            #expect(session.tools.state(for: conversation).selected == .plan)
            #expect(session.tools.state(for: conversation).plan == plan)
            session.closeTool(.plan, in: conversation)
            #expect(tabsItem(otherToolbar) == nil)
            #expect(addItem(otherToolbar) == nil)
            let frame = window.frame
            let pane = split.splitView.arrangedSubviews[2]
            let width = pane.frame.width

            for index in 0..<7 {
                session.tools.show(.terminal(TerminalID("terminal-\(index)")), in: conversation)
            }
            await settle()
            window.layoutIfNeeded()
            #expect(window.frame == frame)
            #expect(pane.frame.width == width)

            let host = try #require(tabs.view)
            let strip = host.convert(host.bounds, to: nil)
            let paneFrame = pane.convert(pane.bounds, to: nil)
            let content = try #require(split.toolsItem.viewController.view.subviews.first)
            #expect(strip.width > paneFrame.width / 2)
            #expect(strip.minX >= paneFrame.minX && strip.maxX <= paneFrame.maxX)
            #expect(strip.minY >= window.contentLayoutRect.maxY)
            #expect(content.convert(content.bounds, to: nil).maxY == window.contentLayoutRect.maxY)

            // The same item comes back, with its state.
            session.tools.toggleVisibility(for: conversation)
            await settle { split.toolsItem.isCollapsed }
            #expect(tabsItem(toolbar) == nil)
            #expect(addItem(toolbar) == nil)
            session.tools.toggleVisibility(for: conversation)
            await settle { !split.toolsItem.isCollapsed }
            #expect(toolbar.items.dropLast().last === tabs)
            #expect(toolbar.items.last?.itemIdentifier == .addTool)

            // Closing the last tab keeps the pane, back at its picker.
            for tool in session.tools.state(for: conversation).tools {
                session.tools.close(tool, in: conversation)
            }
            await settle { tabsItem(toolbar) == nil }
            #expect(tabsItem(toolbar) == nil)
            #expect(addItem(toolbar) == nil)
            #expect(!split.toolsItem.isCollapsed)
        }
    }

    /// The cases run in parallel, which delays their main-actor turns enough
    /// that overlapping slides once stuck the pane collapsed while shown.
    @Test(
        "rapid animated toggles settle on the conversation's latest state",
        .enabled(if: !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion, "Reduce Motion snaps toggles"),
        arguments: [2, 3, 4, 5, 6, 7]
    )
    func rapidToolsToggles(count: Int) async throws {
        try await withScratchWorkspace { id in
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let toolbar = try #require(window.toolbar)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            let pane = try #require(split.toolsItem.viewController as? ToolsPaneViewController)
            await settleStartup(session)
            // Only a visible window animates its toggles.
            window.orderFront(nil)
            defer { window.close() }
            #expect(window.isVisible)
            let frame = window.frame
            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            // Review waits out each show's slide behind a loader.
            session.tools.show(.review, in: conversation)
            await settle { !split.toolsItem.isCollapsed && toolbar.items.last?.itemIdentifier == .addTool }
            let width = split.splitView.arrangedSubviews[2].frame.width

            // Each toggle lands while the first is still sliding.
            for _ in 0..<count {
                session.windowActions.toggleTools()
                try await Task.sleep(for: .milliseconds(35))
            }
            // Up to two slides follow the last toggle.
            let shown = count.isMultiple(of: 2)
            @MainActor func items(_ identifier: NSToolbarItem.Identifier) -> Int {
                toolbar.items.filter { $0.itemIdentifier == identifier }.count
            }
            try await Task.sleep(for: .milliseconds(800))
            await settle { split.toolsItem.isCollapsed == !shown && items(.addTool) == (shown ? 1 : 0) }
            window.layoutIfNeeded()
            #expect(session.isToolsPaneShown == shown)
            #expect(split.toolsItem.isCollapsed == !shown)
            #expect(items(.toolsTabs) == (shown ? 1 : 0))
            #expect(items(.addTool) == (shown ? 1 : 0))
            if shown {
                #expect(!pane.isContentDeferred)
                #expect(split.splitView.arrangedSubviews[2].frame.width == width)
            }
            #expect(window.frame == frame)
        }
    }

    @Test("a collapsed pane leaves the split under the toolbar, so the sidebar glass reaches the top")
    func collapsedPaneKeepsSplitUnderToolbar() async throws {
        try await withScratchWorkspace { id in
            // Saved with the pane shown, which puts the pane's view in the
            // window before the pane collapses.
            UserDefaults.standard.set([
                "0.000000, 0.000000, 300.000000, 700.000000, NO, NO",
                "300.000000, 0.000000, 700.000000, 700.000000, NO, NO",
                "1000.000000, 0.000000, 320.000000, 700.000000, NO, NO",
            ], forKey: "NSSplitView Subview Frames OttamWorkspace.Split.\(id)")
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            await settleStartup(session)
            window.layoutIfNeeded()
            #expect(split.toolsItem.isCollapsed)
            #expect(window.contentLayoutRect.maxY < window.frame.height)
            let splitView = split.splitView
            #expect(splitView.convert(splitView.bounds, to: nil).maxY == window.frame.height)
        }
    }

    @Test("the pane's content can wait out of it behind a loader, and comes back under the band")
    func toolsContentDefers() async throws {
        try await withScratchWorkspace { id in
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            let pane = try #require(split.toolsItem.viewController as? ToolsPaneViewController)
            await settleStartup(session)
            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            session.tools.show(.review, in: conversation)
            await settle { !split.toolsItem.isCollapsed }
            window.layoutIfNeeded()
            let container = try #require(pane.view.subviews.first)
            let content = try #require(container.subviews.first)
            #expect(!pane.isContentDeferred)

            pane.deferContent()
            #expect(pane.isContentDeferred)
            #expect(content.superview == nil)
            #expect(container.subviews.count == 1)
            pane.attachContent()
            window.layoutIfNeeded()
            #expect(!pane.isContentDeferred)
            #expect(container.subviews == [content])
            #expect(content.frame == container.bounds)
            #expect(container.convert(container.bounds, to: nil).maxY == window.contentLayoutRect.maxY)
        }
    }

    @Test("the tools pane opens at its default width, and both side columns widen without a cap")
    func sideColumnsResize() async throws {
        try await withScratchWorkspace { id in
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            await settleStartup(session)
            window.setContentSize(NSSize(width: 1800, height: 700))
            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            session.tools.show(.review, in: conversation)
            await settle { !split.toolsItem.isCollapsed }
            window.layoutIfNeeded()
            let sidebar = split.splitView.arrangedSubviews[0]
            let pane = split.splitView.arrangedSubviews[2]
            #expect(pane.frame.width == WorkspaceSplitViewController.toolsDefaultWidth)

            split.splitView.setPosition(500, ofDividerAt: 0)
            split.splitView.setPosition(pane.frame.minX - 500, ofDividerAt: 1)
            window.layoutIfNeeded()
            #expect(sidebar.frame.width == 500)
            #expect(pane.frame.width >= WorkspaceSplitViewController.toolsDefaultWidth + 500)
            // Main runs under both side columns; its visible width is between them.
            #expect(pane.frame.minX - sidebar.frame.maxX >= split.mainItem.minimumThickness)
        }
    }

    @Test("a sidebar saved wider than the window's room never grows it")
    func savedWideSidebarFitsWindow() async throws {
        try await withScratchWorkspace { id in
            UserDefaults.standard.set([
                "0.000000, 0.000000, 800.000000, 700.000000, NO, NO",
                "801.000000, 0.000000, 799.000000, 700.000000, NO, NO",
                "1600.000000, 0.000000, 320.000000, 700.000000, YES, NO",
            ], forKey: "NSSplitView Subview Frames OttamWorkspace.Split.\(id)")
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            window.setContentSize(WorkspaceShellViewController.minimumSize)
            window.layoutIfNeeded()
            let frame = window.frame
            // Opening the project reloads the saved frames into the narrow window.
            await settleStartup(session)
            window.layoutIfNeeded()
            let sidebar = split.splitView.arrangedSubviews[0]
            #expect(window.frame == frame)
            #expect(!split.sidebarItem.isCollapsed)
            #expect(split.splitView.bounds.width - sidebar.frame.maxX >= split.mainItem.minimumThickness)
        }
    }

    /// A sidebar width of 0 collapses it.
    @Test("a widened pane shown again in a narrowed window fits it (S6)", arguments: [300, 240, 400, 0] as [CGFloat])
    func widenedToolsPaneFitsWindow(sidebarWidth: CGFloat) async throws {
        try await withScratchWorkspace { id in
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            await settleStartup(session)
            window.setContentSize(NSSize(width: 1600, height: 700))
            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            session.tools.show(.review, in: conversation)
            await settle { !split.toolsItem.isCollapsed }
            window.layoutIfNeeded()
            let sidebar = split.splitView.arrangedSubviews[0]
            let pane = split.splitView.arrangedSubviews[2]
            split.splitView.setPosition(pane.frame.minX - 480, ofDividerAt: 1)
            if sidebarWidth > 0 {
                split.splitView.setPosition(sidebarWidth, ofDividerAt: 0)
            } else {
                split.sidebarItem.isCollapsed = true
            }
            window.layoutIfNeeded()
            #expect(pane.frame.width == 800)

            session.tools.toggleVisibility(for: conversation)
            await settle { split.toolsItem.isCollapsed }
            window.setContentSize(WorkspaceShellViewController.minimumSize)
            window.layoutIfNeeded()
            let frame = window.frame
            let sidebarEnd = split.sidebarItem.isCollapsed ? 0 : sidebar.frame.maxX
            session.tools.toggleVisibility(for: conversation)
            await settle { !split.toolsItem.isCollapsed }
            window.layoutIfNeeded()
            #expect(window.frame == frame)
            #expect(pane.frame.width >= split.toolsItem.minimumThickness)
            #expect(pane.frame.width < 800)
            if pane.frame.width > split.toolsItem.minimumThickness {
                #expect((split.sidebarItem.isCollapsed ? 0 : sidebar.frame.maxX) == sidebarEnd)
            }
            let mainStart = split.sidebarItem.isCollapsed ? 0 : sidebar.frame.maxX
            #expect(pane.frame.minX - mainStart >= split.mainItem.minimumThickness)
        }
    }

    @Test(
        "at the minimum, a mid, and a wide width, all three columns and every toolbar item fit",
        arguments: [920, 1100, 1504] as [CGFloat]
    )
    func columnsFitWindowWidths(width: CGFloat) async throws {
        try await withScratchWorkspace { id in
            let session = session(id)
            let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
            let window = try #require(controller.window)
            let toolbar = try #require(window.toolbar)
            let split = try #require((window.contentViewController as? WorkspaceShellViewController)?.split)
            await settleStartup(session)
            window.setContentSize(NSSize(width: width, height: 700))
            window.layoutIfNeeded()
            let frame = window.frame
            let sidebarWidth = split.splitView.arrangedSubviews[0].frame.width

            let conversation = ConversationID("conversation")
            session.chatWorkspace.selectedConversationID = conversation
            session.tools.show(.review, in: conversation)
            await settle { !split.toolsItem.isCollapsed && toolbar.items.last?.itemIdentifier == .addTool }
            window.layoutIfNeeded()
            let sidebar = split.splitView.arrangedSubviews[0]
            let pane = split.splitView.arrangedSubviews[2]
            #expect(window.frame == frame)
            // The pane yields to the sidebar, down to its minimum.
            #expect(sidebar.frame.width == sidebarWidth)
            #expect(pane.frame.width >= split.toolsItem.minimumThickness)
            #expect(pane.frame.width <= WorkspaceSplitViewController.toolsDefaultWidth)
            #expect(pane.frame.maxX <= split.splitView.bounds.width)
            #expect(pane.frame.minX - sidebar.frame.maxX >= split.mainItem.minimumThickness)
            // None overflow; the tracking separators are never visible items.
            let separators: [NSToolbarItem.Identifier] = [.sidebarTrackingSeparator, .inspectorTrackingSeparator]
            #expect(
                toolbar.visibleItems?.map(\.itemIdentifier)
                    == toolbar.items.map(\.itemIdentifier).filter { !separators.contains($0) }
            )
        }
    }

    @Test("live resize stops at the minimum width")
    func windowMinimumWidth() throws {
        let session = session()
        let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
        let window = try #require(controller.window)
        let minimum = window.frameRect(forContentRect: NSRect(origin: .zero, size: WorkspaceShellViewController.minimumSize))
        let clamped = controller.windowWillResize(window, to: NSSize(width: 700, height: 500))
        #expect(clamped == NSSize(width: minimum.width, height: 500))
        let wide = NSSize(width: 1200, height: 800)
        #expect(controller.windowWillResize(window, to: wide) == wide)
    }

    @Test("the window shows the session's title and subtitle")
    func windowTitleBar() throws {
        let session = session()
        let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
        let window = try #require(controller.window)
        #expect(window.title == session.windowTitle)
        #expect(window.subtitle == session.contextSubtitle)
    }

    @Test("the title is Chat until a conversation loads")
    func windowTitle() {
        let session = session()
        #expect(session.windowTitle == "Chat")
        #expect(session.windowsMenuTitle == "Chat")
    }

    @Test("the Window menu item keeps the Workspace name, and a closed window stays out of the menu")
    func windowsMenuItem() async throws {
        let app = NSApplication.shared
        let menu = NSMenu(title: "Window")
        let previous = app.windowsMenu
        app.windowsMenu = menu
        defer { app.windowsMenu = previous }
        let session = session()
        session.workspace.apply(detail: detail(session, named: "Menu Probe"))
        let controller = WorkspaceWindowController(session: session, registry: session.runtime.windows)
        let window = try #require(controller.window)
        #expect(window.title == "Chat")
        #expect(menu.items.map(\.title).contains("Menu Probe — Chat"))

        controller.close()
        session.workspace.apply(detail: detail(session, named: "Menu Probe Renamed"))
        await settle()
        #expect(!menu.items.contains { $0.title.hasPrefix("Menu Probe") })
    }

    @Test("a window saved with a route, even a retired one, restores the rest", arguments: ["chat", "terminal"])
    func retiredRouteKey(saved: String) throws {
        let json = """
            {"route":"\(saved)","projectID":"project","chatRoot":"workspace",\
            "conversationID":"conversation","collapsedSections":"chats"}
            """
        let state = try JSONDecoder().decode(WorkspaceRestorationState.self, from: Data(json.utf8))
        #expect(state == WorkspaceRestorationState(
            projectID: "project", chatRoot: "workspace", conversationID: "conversation", collapsedSections: "chats"
        ))
    }
}
