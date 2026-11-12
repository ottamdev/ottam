import AppKit
import OttamAccessibility
import OttamBackendClient
import Observation
import SwiftUI

/// The workspace window's columns, each one long-lived hosting controller.
/// AppKit owns widths and collapsed state through the split view's autosave.
/// See `docs/architecture/app/macos-ui.md`.
@MainActor
final class WorkspaceSplitViewController: NSSplitViewController {
    static let sidebarDefaultWidth: CGFloat = 300
    static let toolsDefaultWidth: CGFloat = 320

    let sidebarItem: NSSplitViewItem
    let mainItem: NSSplitViewItem
    let toolsItem: NSSplitViewItem
    private let toolsPane: ToolsPaneViewController
    private let session: WorkspaceWindowSession
    private let autosaveName: String
    /// The user's sidebar state while a project error hides the sidebar.
    private(set) var sidebarCollapsedBeforeProjectError: Bool?
    /// The conversation whose pane state was last applied.
    private var toolsConversationID: ConversationID?
    /// A slide runs to its end, which applies the latest state: AppKit
    /// finishes an overlapping collapse after a later one starts, undoing it.
    private var isToolsSliding = false
    /// Built once, so the tabs keep their state while out of the toolbar.
    private var toolsTabsItem: NSToolbarItem?

    init(session: WorkspaceWindowSession) {
        self.session = session
        autosaveName = "OttamWorkspace.Split.\(session.workspaceID.rawValue)"

        let sidebarHost = NSHostingController(rootView: WorkspaceSidebarColumn(session: session))
        sidebarHost.sizingOptions = []
        sidebarHost.sceneBridgingOptions = []
        sidebarHost.view.frame.size.width = Self.sidebarDefaultWidth
        sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebarHost)
        sidebarItem.minimumThickness = 240
        sidebarItem.maximumThickness = NSSplitViewItem.unspecifiedDimension
        sidebarItem.holdingPriority = NSLayoutConstraint.Priority(255)
        sidebarItem.collapseBehavior = .preferResizingSiblingsWithFixedSplitView

        let mainHost = NSHostingController(rootView: WorkspaceRootView(session: session))
        mainHost.sizingOptions = []
        // The window controller owns the toolbar and title.
        mainHost.sceneBridgingOptions = []
        mainItem = NSSplitViewItem(viewController: mainHost)
        mainItem.minimumThickness = 320
        mainItem.holdingPriority = NSLayoutConstraint.Priority(250)
        // The main column runs under the sidebar with a matching safe area, so
        // the sidebar glass shows the canvas, as it does in `NavigationSplitView`.
        mainItem.automaticallyAdjustsSafeAreaInsets = true

        let toolsHost = NSHostingController(rootView: ConversationToolsColumn(session: session))
        toolsHost.sizingOptions = []
        toolsHost.sceneBridgingOptions = []
        toolsPane = ToolsPaneViewController(content: toolsHost)
        // The pane shows at its view's width; see `setToolsWidth(_:)`.
        toolsPane.view.frame.size.width = Self.toolsDefaultWidth
        toolsItem = NSSplitViewItem(inspectorWithViewController: toolsPane)
        toolsItem.minimumThickness = 280
        // An inspector's default maximum is 270, under this minimum, which
        // would pin the pane at 280.
        toolsItem.maximumThickness = NSSplitViewItem.unspecifiedDimension
        toolsItem.holdingPriority = NSLayoutConstraint.Priority(260)
        toolsItem.collapseBehavior = .preferResizingSiblingsWithFixedSplitView
        // The selected conversation owns visibility, so AppKit never collapses
        // the pane itself: not by divider drag, and not for window width (all
        // three columns fit the window's minimum).
        toolsItem.canCollapse = false
        toolsItem.canCollapseFromWindowResize = false
        toolsItem.isCollapsed = true

        super.init(nibName: nil, bundle: nil)
        addSplitViewItem(sidebarItem)
        addSplitViewItem(mainItem)
        addSplitViewItem(toolsItem)
        session.windowActions.toggleSidebar = { [weak self] in
            self?.toggleSidebar(nil)
        }
        session.windowActions.selectAdjacentTool = { [weak self] offset in
            self?.selectAdjacentTool(offset)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        splitView.autosaveName = autosaveName
        splitView.setAccessibilityIdentifier(AccessibilityID.workspaceRoot)
        observeCoreState()
        observeColumns()
        // Autosave records the pane's width, but each launch starts from the
        // default.
        setToolsWidth(Self.toolsDefaultWidth)
    }

    /// The pane shows at its view's width, while the split sizes the window
    /// from its inspector wrapper's frame. A wrapper wider than the room beside
    /// the other columns grows the window by the pane's whole width (S6).
    private func setToolsWidth(_ width: CGFloat) {
        guard let index = splitViewItems.firstIndex(of: toolsItem) else { return }
        toolsItem.viewController.view.frame.size.width = width
        splitView.arrangedSubviews[index].frame.size.width = width
    }

    /// A pane hidden at some width may not fit a window narrowed since. It
    /// shows at the room beside the sidebar's width and main's minimum,
    /// never below its own minimum; there the sidebar narrows instead.
    private func fitToolsWidth() {
        guard let index = splitViewItems.firstIndex(of: toolsItem) else { return }
        let divider = splitView.dividerThickness
        if !sidebarItem.isCollapsed {
            let maximumSidebarWidth = splitView.bounds.width - mainItem.minimumThickness
                - toolsItem.minimumThickness - 2 * divider
            if maximumSidebarWidth >= sidebarItem.minimumThickness,
               splitView.arrangedSubviews[0].frame.width > maximumSidebarWidth {
                // On macOS 26, revealing the inspector can otherwise borrow
                // from the visible main column instead of narrowing the sidebar.
                splitView.setPosition(maximumSidebarWidth, ofDividerAt: 0)
            }
        }
        let sidebar = sidebarItem.isCollapsed ? 0 : splitView.arrangedSubviews[0].frame.width + divider
        let room = splitView.bounds.width - sidebar - mainItem.minimumThickness - divider
        let width = splitView.arrangedSubviews[index].frame.width
        setToolsWidth(max(toolsItem.minimumThickness, min(width, room)))
    }

    /// Show Next/Previous Tab. Keyboard focus follows the selection unless a
    /// view outside the pane holds it, such as the composer. Review has
    /// nothing to focus, so leaving a terminal for it leaves focus nowhere;
    /// the next terminal takes it back.
    private func selectAdjacentTool(_ offset: Int) {
        let responder = view.window?.firstResponder as? NSView
        session.selectAdjacentTool(offset, focus: responder?.isDescendant(of: toolsPane.view) ?? true)
    }

    /// A project error hides the sidebar. Autosave is off until the error
    /// clears, so the user's saved state never records the hide. Autosave
    /// comes back before the user's state does: setting the name reloads the
    /// saved frames, which may predate the user's last toggle.
    func setProjectErrorShown(_ shown: Bool) {
        if shown, sidebarCollapsedBeforeProjectError == nil {
            sidebarCollapsedBeforeProjectError = sidebarItem.isCollapsed
            splitView.autosaveName = nil
            sidebarItem.isCollapsed = true
        } else if !shown, let wasCollapsed = sidebarCollapsedBeforeProjectError {
            sidebarCollapsedBeforeProjectError = nil
            splitView.autosaveName = autosaveName
            sidebarItem.isCollapsed = wasCollapsed
        }
    }

    /// Autosave also records the tools pane, but the selected conversation
    /// owns it: it is applied after anything that reloads the saved frames.
    private func observeColumns() {
        withObservationTracking {
            setProjectErrorShown(session.workspace.projectOpenError != nil)
            applyToolsPane()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observeColumns()
            }
        }
    }

    /// The pane's tabs: a toolbar item after the inspector tracking separator,
    /// which the toolbar lays over the pane and sizes with it. Unbordered, so
    /// the toolbar draws no glass behind the strip's own track.
    func makeToolsTabsItem(_ identifier: NSToolbarItem.Identifier) -> NSToolbarItem {
        if let toolsTabsItem { return toolsTabsItem }
        let host = NSHostingView(rootView: ConversationToolsTabStrip(session: session))
        // The toolbar sizes the strip; its tabs never do.
        host.sizingOptions = []
        host.sceneBridgingOptions = []
        host.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            host.heightAnchor.constraint(equalToConstant: ConversationToolsTabStrip.trackHeight),
            // A minimum makes the item flexible, so the toolbar widens it to
            // the section. A larger one holds it wider than the pane while the
            // pane slides open.
            host.widthAnchor.constraint(greaterThanOrEqualToConstant: 1),
        ])
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = "Tool Tabs"
        item.view = host
        item.isBordered = false
        toolsTabsItem = item
        return item
    }

    /// A collapsed pane's toolbar section keeps room for its items, hidden
    /// ones too, which holds New Chat and Tools off the window's corner. So
    /// the tabs and Add Tool leave the toolbar before the pane slides shut,
    /// and join it once the pane has slid open: joining earlier shoves New
    /// Chat and Tools aside by that room.
    private func setToolsTabsShown(_ shown: Bool) {
        guard let toolbar = view.window?.toolbar else { return }
        for identifier in [NSToolbarItem.Identifier.toolsTabs, .addTool] {
            let index = toolbar.items.firstIndex { $0.itemIdentifier == identifier }
            if shown, index == nil {
                toolbar.insertItem(withItemIdentifier: identifier, at: toolbar.items.count)
            } else if !shown, let index {
                toolbar.removeItem(at: index)
            }
        }
    }

    /// The selected conversation's tabs; a draft has none.
    private var toolsState: ConversationToolsState {
        session.chatWorkspace.selectedConversationID.map(session.tools.state(for:)) ?? ConversationToolsState()
    }

    /// Toggling animates, as the sidebar toggle does. Switching conversations
    /// snaps, so the pane never slides while its content changes. Changes
    /// during a slide wait for its end.
    private func applyToolsPane() {
        // Read before returning, so the next change still wakes the observer.
        let conversationID = session.chatWorkspace.selectedConversationID
        let collapsed = !session.isToolsPaneShown
        let selected = toolsState.selected
        // The conversation stays unrecorded, so a switch snaps at the end.
        guard !isToolsSliding else { return }
        defer { toolsConversationID = conversationID }
        guard toolsItem.isCollapsed != collapsed else { return settleToolsPane() }
        if collapsed { setToolsTabsShown(false) } else { fitToolsWidth() }
        let animates = conversationID == toolsConversationID
            && view.window?.isVisible == true
            && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        guard animates else {
            toolsItem.isCollapsed = collapsed
            return settleToolsPane()
        }
        if !collapsed {
            // Content joining the window rebuilds its views there, and Review's
            // rebuild stalls the slide for as long as it takes. So Review joins
            // once the pane has slid open, a loader in its place until then.
            if selected == .review { toolsPane.deferContent() } else { toolsPane.attachContent() }
        }
        isToolsSliding = true
        NSAnimationContext.runAnimationGroup { _ in
            toolsItem.animator().isCollapsed = collapsed
        } completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.isToolsSliding = false
                self.applyToolsPane()
            }
        }
    }

    /// Past any slide, a shown pane holds its content, and its tabs once the
    /// conversation has one; before that the pane shows the tool picker.
    private func settleToolsPane() {
        let shown = !toolsItem.isCollapsed
        setToolsTabsShown(shown && !toolsState.tools.isEmpty)
        if shown { toolsPane.attachContent() }
    }

    /// Scenarios read Core's state from the workspace root's label.
    private func observeCoreState() {
        withObservationTracking {
            splitView.setAccessibilityLabel("Workspace, \(session.coreAccessibilityState)")
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observeCoreState()
            }
        }
    }
}

/// The tools pane's column: the tool, below the toolbar band that holds its
/// tabs. The toolbar draws no scroll edge over the pane, so nothing may
/// scroll under the tabs.
///
/// After an animated show the pane loses the band from its safe area (macOS
/// 27 beta), so the band's height comes from the window's content layout
/// guide, which holds however the pane was shown.
///
/// The content can wait out of the pane, a loader in its place, so it
/// joins the window after a slide rather than during one.
final class ToolsPaneViewController: NSViewController {
    private let content: NSViewController
    /// Holds the content, or the loader, below the band.
    private let container = NSView()
    private let loader = ToolsLoaderView()
    private var band: NSLayoutConstraint?

    init(content: NSViewController) {
        self.content = content
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    var isContentDeferred: Bool { loader.superview != nil }

    override func loadView() {
        let view = WindowAttachedView()
        view.didMoveToWindow = { [weak self] window in self?.pinContainer(to: window) }
        self.view = view
        addChild(content)
        container.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(container)
        NSLayoutConstraint.activate([
            container.topAnchor.constraint(greaterThanOrEqualTo: view.topAnchor),
            container.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            container.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            container.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        fill(with: content.view)
    }

    /// Takes the content out of the pane, the loader in its place.
    func deferContent() {
        guard !isContentDeferred else { return }
        loadViewIfNeeded()
        content.view.removeFromSuperview()
        fill(with: loader)
    }

    func attachContent() {
        guard isContentDeferred else { return }
        loader.removeFromSuperview()
        fill(with: content.view)
    }

    private func fill(with subview: NSView) {
        subview.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(subview)
        NSLayoutConstraint.activate([
            subview.topAnchor.constraint(equalTo: container.topAnchor),
            subview.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            subview.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            subview.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])
    }

    private func pinContainer(to window: NSWindow) {
        band?.isActive = false
        guard let guide = window.contentLayoutGuide as? NSLayoutGuide else { return }
        band = container.topAnchor.constraint(equalTo: guide.topAnchor)
        // Under the split controller's pin of its split view (749): a
        // collapsed pane has no height, so a band that outranked the pin
        // would pull the split view, sidebar glass and all, below the toolbar.
        band?.priority = NSLayoutConstraint.Priority(748)
        band?.isActive = true
    }
}

/// Stands in for the pane's content while it waits: the editor's
/// background under a spinner.
private final class ToolsLoaderView: NSView {
    private let spinner = NSProgressIndicator()

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.translatesAutoresizingMaskIntoConstraints = false
        addSubview(spinner)
        NSLayoutConstraint.activate([
            spinner.centerXAnchor.constraint(equalTo: centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override var wantsUpdateLayer: Bool { true }

    override func updateLayer() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = ReviewColors.editorBackgroundNS.cgColor
        }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { spinner.stopAnimation(nil) } else { spinner.startAnimation(nil) }
    }
}

/// A view that reports joining a window, where the window's layout guides
/// can be constrained against.
private final class WindowAttachedView: NSView {
    var didMoveToWindow: ((NSWindow) -> Void)?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let window { didMoveToWindow?(window) }
    }
}

/// The sidebar column's content.
struct WorkspaceSidebarColumn: View {
    @Bindable var session: WorkspaceWindowSession

    var body: some View {
        // A sidebar `List` outside a navigation container never restyles its
        // rows after a selection change in an inactive window: the previously
        // selected row keeps its weight (macOS 27). `NavigationSplitView`'s
        // sidebar column provided this context.
        NavigationStack {
            SidebarView(
                collapsedSectionsRaw: $session.restored.collapsedSections,
                workspace: session.workspace,
                chatWorkspace: session.chatWorkspace,
                backend: session.diffModel,
                addProject: session.chooseWorkspace
            )
        }
        // Rebuilt once per Core generation, like the main column.
        .id(session.readyCoreInstanceID)
        .environment(session.runtime)
    }
}
