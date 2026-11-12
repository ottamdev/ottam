import AppKit
import OttamBackendClient
import OttamWorkspaceLogic
import Observation
import OSLog
import SwiftUI

extension NSToolbarItem.Identifier {
    static let newChat = NSToolbarItem.Identifier("newChat")
    static let toggleTools = NSToolbarItem.Identifier("toggleTools")
    static let toolsTabs = NSToolbarItem.Identifier("toolsTabs")
    static let addTool = NSToolbarItem.Identifier("addTool")
    static let agents = NSToolbarItem.Identifier("agents")
}

/// One workspace window. AppKit owns the window and toolbar; SwiftUI renders
/// the content. See `docs/architecture/app/macos-ui.md`.
@MainActor
final class WorkspaceWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate,
    NSToolbarItemValidation, NSMenuItemValidation
{
    static let defaultContentSize = NSSize(width: 1504, height: 1047)

    let session: WorkspaceWindowSession
    private let registry: WorkspaceWindowRegistry
    /// Whether this workspace's window has been open before.
    private(set) var hasSavedFrame = false

    init(session: WorkspaceWindowSession, registry: WorkspaceWindowRegistry) {
        self.session = session
        self.registry = registry
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.defaultContentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: true
        )
        super.init(window: window)
        window.isReleasedWhenClosed = false
        window.identifier = WorkspaceWindowRestoration.identifier
        window.restorationClass = WorkspaceWindowRestoration.self
        // Test and capture launches leave nothing for the next launch.
        window.isRestorable = WindowPlacement.isEnabledForCurrentLaunch
        window.tabbingMode = .disallowed
        window.toolbarStyle = .unified
        window.delegate = self
        window.contentViewController = WorkspaceShellViewController(session: session)
        // Toolbars that share an identifier share inserted and removed items,
        // so each window's toolbar is its own: the tools pane's tabs join and
        // leave it with that window's pane.
        let toolbar = NSToolbar(identifier: "OttamWorkspace.\(UUID().uuidString)")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window.toolbar = toolbar

        let visible = NSScreen.main?.visibleFrame.size ?? Self.defaultContentSize
        window.setContentSize(NSSize(
            width: min(Self.defaultContentSize.width, visible.width),
            height: min(Self.defaultContentSize.height, visible.height)
        ))
        window.center()
        hasSavedFrame = WindowPlacement.shared.attach(
            to: window, role: .workspace(session.workspaceID.rawValue)
        )
        observeTitle()
        observeAgentGroup()
        observeRestorableState()
        observeToolbarValidation()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    /// Offsets the window down and right of `anchor`, as a new document
    /// window would be.
    func cascade(from anchor: NSWindow) {
        window?.cascadeTopLeft(from: anchor.cascadeTopLeft(from: .zero))
    }

    private func observeTitle() {
        withObservationTracking {
            // A closed window would rejoin the Window menu: changing its item
            // adds one when it has none.
            guard !session.isClosed, let window else { return }
            window.title = session.windowTitle
            window.subtitle = session.contextSubtitle
            // After the title and subtitle, which each reset the window's
            // Window menu item.
            NSApplication.shared.changeWindowsItem(window, title: session.windowsMenuTitle, filename: false)
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observeTitle()
            }
        }
    }

    /// The agents pill joins the toolbar, beside the title, while the
    /// selected conversation delegated or was delegated, and leaves it
    /// otherwise.
    private func observeAgentGroup() {
        withObservationTracking {
            guard !session.isClosed else { return }
            applyAgentGroup(session.chatWorkspace.selectedAgentGroup)
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observeAgentGroup()
            }
        }
    }

    private var agentGroup: ConversationAgentGroup?
    /// The conversation the pill's menu checks.
    private var agentGroupSelection: ConversationID?

    private func applyAgentGroup(_ group: ConversationAgentGroup?) {
        guard let toolbar = window?.toolbar else { return }
        let selected = session.chatWorkspace.selectedConversationID
        let index = toolbar.items.firstIndex { $0.itemIdentifier == .agents }
        // Any change to the conversation list lands here. Rebuild the pill
        // only when what it shows changed.
        if index != nil, group == agentGroup, selected == agentGroupSelection { return }
        agentGroup = group
        agentGroupSelection = selected
        if group == nil {
            if let index { toolbar.removeItem(at: index) }
            return
        }
        if let index, let item = toolbar.items[index] as? NSMenuToolbarItem {
            configureAgentsItem(item)
        } else if index == nil {
            let separator = toolbar.items.firstIndex { $0.itemIdentifier == .sidebarTrackingSeparator }
            toolbar.insertItem(withItemIdentifier: .agents, at: separator.map { $0 + 1 } ?? 0)
        }
    }

    /// The pill: the agents' faces, and a menu that opens each one's
    /// conversation with its state.
    private func agentsItem(_ identifier: NSToolbarItem.Identifier) -> NSToolbarItem {
        let item = NSMenuToolbarItem(itemIdentifier: identifier)
        item.isBordered = true
        item.showsIndicator = true
        configureAgentsItem(item)
        return item
    }

    private func configureAgentsItem(_ item: NSMenuToolbarItem) {
        guard let group = agentGroup else { return }
        // The pill shows the agents' faces. Their names are its label, which
        // the icon-only toolbar hides but VoiceOver reads, its tooltip, and
        // the menu.
        item.image = AgentStackView.image(group.members, description: group.title)
        item.title = ""
        item.label = group.title
        item.paletteLabel = "Agents"
        item.toolTip = group.title
        let menu = NSMenu()
        let selected = session.chatWorkspace.selectedConversationID
        for member in group.members {
            if !member.isParent, member == group.members.dropFirst().first { menu.addItem(.separator()) }
            // The task leads, since the tile already shows the agent; a long
            // title is cut so it doesn't set the menu's width, and is whole
            // in the tooltip.
            let title = member.title.cappedTitle()
            let entry = NSMenuItem(
                title: title,
                action: #selector(openAgentConversation(_:)),
                keyEquivalent: ""
            )
            entry.subtitle = "\(member.agentName) · \(member.state)"
            if title != member.title {
                entry.toolTip = member.title
            }
            entry.image = agentImage(member)
            entry.target = self
            entry.representedObject = member.conversationID.rawValue
            entry.state = member.conversationID == selected ? .on : .off
            // VoiceOver reads the whole title, then the agent and its state.
            entry.setAccessibilityLabel(member.title)
            entry.setAccessibilityValue("\(member.agentName), \(member.state)")
            menu.addItem(entry)
        }
        item.menu = menu
    }

    /// A member's tile at menu size: the parent's brand, a child's own.
    /// The item's title names the agent, so the image carries no label.
    private func agentImage(_ member: ConversationAgentGroup.Member) -> NSImage {
        switch member.icon {
        case .brand(let brand):
            AgentIconView.image(brand: brand, name: member.agentName, size: 16)
        case .avatar(let avatar):
            AgentAvatarView.image(avatar, size: 16)
        }
    }

    @objc private func openAgentConversation(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String else { return }
        session.openConversation(ConversationID(raw))
    }

    /// AppKit validates toolbar items after each event. State that changes
    /// without one, such as a first send creating its conversation or an
    /// Accessibility action, revalidates here.
    private func observeToolbarValidation() {
        withObservationTracking {
            _ = session.windowActions.canToggleTools
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.window?.toolbar?.validateVisibleItems()
                self?.observeToolbarValidation()
            }
        }
    }

    private func observeRestorableState() {
        withObservationTracking {
            _ = session.restored
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.window?.invalidateRestorableState()
                self?.observeRestorableState()
            }
        }
    }

    // MARK: - Toolbar

    /// The sidebar toggle sits at the sidebar's trailing edge, as in Mail and
    /// Finder, and beside the window controls while the sidebar is collapsed.
    /// `toggleSidebar:` finds the sidebar split through the responder chain,
    /// or through the window when nothing inside it is first responder. Tools
    /// sits with the conversation it belongs to, not over the pane. It is not
    /// the standard `toggleInspector:`, which collapses the split item
    /// directly: the pane's visibility belongs to the selected conversation.
    /// While the pane shows, its tabs fill the section over it, then Add Tool.
    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [
            .flexibleSpace, .toggleSidebar, .sidebarTrackingSeparator,
            .flexibleSpace, .newChat, .toggleTools, .inspectorTrackingSeparator,
        ]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar) + [.toolsTabs, .addTool, .agents]
    }

    func toolbar(
        _ toolbar: NSToolbar,
        itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        switch itemIdentifier {
        case .newChat:
            item(itemIdentifier, title: "New Chat", symbol: "plus", action: #selector(newChat(_:)))
        case .toggleTools:
            item(itemIdentifier, title: "Tools", symbol: "sidebar.trailing", action: #selector(toggleTools(_:)))
        case .toolsTabs:
            (window?.contentViewController as? WorkspaceShellViewController)?.split.makeToolsTabsItem(itemIdentifier)
        case .addTool:
            addToolItem(itemIdentifier)
        case .agents:
            agentsItem(itemIdentifier)
        default:
            nil
        }
    }

    /// Add Tool: a pull-down of the kinds of tool, bordered so the toolbar
    /// draws it as New Chat and Tools are drawn. Its items have no images, as
    /// menus draw by default from macOS 27.
    private func addToolItem(_ identifier: NSToolbarItem.Identifier) -> NSToolbarItem {
        let item = NSMenuToolbarItem(itemIdentifier: identifier)
        item.label = "Add Tool"
        item.toolTip = "Add Tool"
        item.image = NSImage(systemSymbolName: "plus", accessibilityDescription: "Add Tool")
        item.showsIndicator = false
        item.isBordered = true
        item.menu = NSMenu()
        // The item is a pull-down, which takes its menu's first item as its
        // title and never lists it.
        item.menu.addItem(withTitle: "", action: nil, keyEquivalent: "")
        for (kind, action) in [
            (ConversationToolKind.review, #selector(openReview(_:))),
            (.terminal, #selector(openTerminal(_:))),
            (.plan, #selector(openPlan(_:))),
        ] {
            let entry = NSMenuItem(title: kind.title, action: action, keyEquivalent: "")
            entry.target = self
            item.menu.addItem(entry)
        }
        return item
    }

    private func item(
        _ identifier: NSToolbarItem.Identifier,
        title: String,
        symbol: String,
        action: Selector
    ) -> NSToolbarItem {
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = title
        item.toolTip = title
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        item.isBordered = true
        item.target = self
        item.action = action
        return item
    }

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        switch item.itemIdentifier {
        case .toggleTools: session.windowActions.canToggleTools
        default: true
        }
    }

    @objc private func newChat(_ sender: Any?) {
        session.windowActions.newChat()
    }

    @objc private func toggleTools(_ sender: Any?) {
        session.windowActions.toggleTools()
    }

    /// A conversation whose project left the workspace cannot open a terminal;
    /// one with no plan cannot open Plan.
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(openTerminal(_:)): session.canOpenTerminal
        case #selector(openPlan(_:)): session.canOpenPlan
        default: true
        }
    }

    @objc private func openReview(_ sender: Any?) {
        session.openReview()
    }

    @objc private func openTerminal(_ sender: Any?) {
        session.openTerminal()
    }

    @objc private func openPlan(_ sender: Any?) {
        session.openPlan()
    }

    // MARK: - Window

    /// The width minimum for live resize; see `WorkspaceShellViewController`.
    func windowWillResize(_ sender: NSWindow, to frameSize: NSSize) -> NSSize {
        let minimum = sender.frameRect(forContentRect: NSRect(origin: .zero, size: WorkspaceShellViewController.minimumSize))
        return NSSize(width: max(frameSize.width, minimum.width), height: frameSize.height)
    }

    func window(_ window: NSWindow, willEncodeRestorableState state: NSCoder) {
        WorkspaceWindowRestoration.encode(session.workspaceID, session.restored, with: state)
    }

    func windowDidBecomeMain(_ notification: Notification) {
        registry.activeActions = session.windowActions
    }

    func windowDidResignMain(_ notification: Notification) {
        if registry.activeActions === session.windowActions {
            registry.activeActions = nil
        }
    }

    func windowWillClose(_ notification: Notification) {
        windowDidResignMain(notification)
        registry.release(self)
        session.tearDown()
        // After the current event: closing can start inside one of the
        // content's own actions (Archive, the core-failure sheet).
        DispatchQueue.main.async { [self] in
            window?.contentViewController = nil
        }
    }
}

/// The window's content: the column split, and the startup cover over the
/// whole window until the first paint settles.
@MainActor
final class WorkspaceShellViewController: NSViewController {
    static let minimumSize = NSSize(width: 920, height: 620)

    let split: WorkspaceSplitViewController
    private let session: WorkspaceWindowSession
    private let cover = NSHostingView(rootView: WorkspaceStartupCover())

    init(session: WorkspaceWindowSession) {
        self.session = session
        split = WorkspaceSplitViewController(session: session)
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override func loadView() {
        view = NSView()
        addChild(split)
        cover.sizingOptions = []
        for subview in [split.view, cover] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(subview)
            NSLayoutConstraint.activate([
                subview.leadingAnchor.constraint(equalTo: view.leadingAnchor),
                subview.trailingAnchor.constraint(equalTo: view.trailingAnchor),
                subview.topAnchor.constraint(equalTo: view.topAnchor),
                subview.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            ])
        }
        // The height minimum; `contentMinSize` is ignored under Auto Layout.
        // The width minimum is the window delegate's: a width constraint here
        // pins the split view's fitting width, and the split then grows the
        // window to show the tools pane instead of resizing its siblings (S6).
        split.view.heightAnchor.constraint(greaterThanOrEqualToConstant: Self.minimumSize.height).isActive = true
        cover.isHidden = !session.isPreparingFirstPaint
        observeFirstPaint()
    }

    private func observeFirstPaint() {
        withObservationTracking {
            setCoverShown(session.isPreparingFirstPaint)
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observeFirstPaint()
            }
        }
    }

    private func setCoverShown(_ shown: Bool) {
        if shown {
            cover.alphaValue = 1
            cover.isHidden = false
        } else if !cover.isHidden {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.2
                context.timingFunction = CAMediaTimingFunction(name: .easeOut)
                cover.animator().alphaValue = 0
            } completionHandler: { [weak self] in
                MainActor.assumeIsolated {
                    guard let self, !self.session.isPreparingFirstPaint else { return }
                    self.cover.isHidden = true
                }
            }
        }
    }
}

/// Recreates the workspace windows AppKit saved at quit (⌥⌘Q, or any quit
/// with "Close windows when quitting an app" off).
@MainActor
final class WorkspaceWindowRestoration: NSObject, NSWindowRestoration {
    static let identifier = NSUserInterfaceItemIdentifier("workspace")
    /// Set at launch, before AppKit restores windows.
    static weak var runtime: OttamAppRuntime?

    private static let workspaceIDKey = "workspaceID"
    private static let stateKey = "workspaceState"

    static func restoreWindow(
        withIdentifier identifier: NSUserInterfaceItemIdentifier,
        state: NSCoder,
        completionHandler: @escaping (NSWindow?, (any Error)?) -> Void
    ) {
#if DEBUG
        guard !UITestPreparation.isActive else {
            completionHandler(nil, nil)
            return
        }
#endif
        guard let runtime,
              let decoded = decode(from: state)
        else {
            completionHandler(nil, nil)
            return
        }
        restorationLogger.info("restoring workspace window \(decoded.workspaceID.rawValue, privacy: .public)")
        let controller = runtime.windows.open(
            decoded.workspaceID,
            runtime: runtime,
            restored: decoded.state,
            show: false
        )
        completionHandler(controller.window, nil)
    }

    static func encode(_ workspaceID: WorkspaceID, _ state: WorkspaceRestorationState, with coder: NSCoder) {
        coder.encode(workspaceID.rawValue as NSString, forKey: workspaceIDKey)
        if let data = try? JSONEncoder().encode(state) {
            coder.encode(data as NSData, forKey: stateKey)
        }
    }

    static func decode(from coder: NSCoder) -> (workspaceID: WorkspaceID, state: WorkspaceRestorationState)? {
        guard let rawID = coder.decodeObject(of: NSString.self, forKey: workspaceIDKey) as String? else {
            return nil
        }
        // A state from an older build restores the window with defaults.
        let state = (coder.decodeObject(of: NSData.self, forKey: stateKey) as Data?)
            .flatMap { try? JSONDecoder().decode(WorkspaceRestorationState.self, from: $0) }
            ?? WorkspaceRestorationState()
        return (WorkspaceID(rawID), state)
    }
}

let restorationLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "WindowRestoration"
)
