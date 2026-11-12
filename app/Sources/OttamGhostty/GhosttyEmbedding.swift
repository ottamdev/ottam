import AppKit
import Carbon.HIToolbox
import GhosttyKit
import Metal
import OSLog
import QuartzCore

private let ghosttyLogger = Logger(
    subsystem: "dev.ottam.Ottam",
    category: "TerminalGhostty"
)

public enum GhosttyTerminalConfigurationReload {
    @MainActor
    @discardableResult
    public static func reload(source: String = "api") -> Bool {
        GhosttyRuntime.shared.reloadConfiguration(source: source)
    }

    /// Why the last reload failed, when the runtime knows. Nil after a
    /// successful reload.
    @MainActor
    public static var lastReloadError: String? {
        GhosttyRuntime.shared.lastReloadError
    }

    @MainActor
    public static func overlayConfigURL() -> URL? {
        GhosttyRuntime.shared.overlayConfigURL()
    }

    /// The app's code font. Applied beneath the overlay file, and reloaded on a
    /// short debounce so dragging a size stepper does not rebuild every surface
    /// on every step. A nil family means the system monospaced font.
    @MainActor
    public static func setFont(family: String?, points: Double) {
        GhosttyRuntime.shared.setFont(family: family, points: points)
    }

    /// True if the overlay file sets `font-size`, in which case it overrides
    /// the app's Code Font size and Settings should say so.
    @MainActor
    public static func overlayPinsFontSize() -> Bool {
        GhosttyRuntime.shared.overlayPinsFontSize()
    }
}

public protocol TerminalHostViewProviding {
    func makeTerminalHostView(configuration: GhosttyTerminalConfiguration) -> NSView
}

public struct GhosttyTerminalConfiguration: Sendable {
    public var title: String
    public var workingDirectory: String
    public var startupCommand: String?
    public var manualIO: Bool
    public var onManualInput: (@Sendable (Data) -> Void)?
    public var onResize: (@Sendable (UInt16, UInt16) -> Void)?

    public init(title: String, workingDirectory: String, startupCommand: String) {
        self.title = title
        self.workingDirectory = workingDirectory
        self.startupCommand = startupCommand
        self.manualIO = false
        self.onManualInput = nil
        self.onResize = nil
    }

    public init(
        title: String,
        workingDirectory: String,
        onManualInput: (@escaping @Sendable (Data) -> Void),
        onResize: (@escaping @Sendable (UInt16, UInt16) -> Void)
    ) {
        self.title = title
        self.workingDirectory = workingDirectory
        self.startupCommand = nil
        self.manualIO = true
        self.onManualInput = onManualInput
        self.onResize = onResize
    }
}

public final class GhosttyTerminalHostProvider: TerminalHostViewProviding {
    public init() {}

    public func makeTerminalHostView(configuration: GhosttyTerminalConfiguration) -> NSView {
        GhosttyTerminalHostView(configuration: configuration)
    }
}

public final class GhosttyTerminalHostView: NSView {
    private let configuration: GhosttyTerminalConfiguration
    private let scrollView = GhosttyScrollView()
    private let documentView = NSView(frame: .zero)
    private var terminalView: EmbeddedGhosttyView?
    private var scrollbar: GhosttyScrollbar?
    private var boundsObserver: NSObjectProtocol?
    private var liveScrollObservers: [NSObjectProtocol] = []
    private var isSynchronizingScrollView = false
    private var isLiveScrolling = false
    private var userScrolledAwayFromBottom = false
    private var explicitCellHeight: CGFloat?
    private var lastSentRow: Int?
    private let maxVirtualDocumentHeight: CGFloat = 1_000_000

    public init(configuration: GhosttyTerminalConfiguration) {
        self.configuration = configuration
        super.init(frame: .zero)
        buildLayout()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    deinit {
        if let boundsObserver {
            NotificationCenter.default.removeObserver(boundsObserver)
        }
        for observer in liveScrollObservers {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    private func buildLayout() {
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor

        let terminalView = EmbeddedGhosttyView(configuration: configuration) { _ in }
        self.terminalView = terminalView
        terminalView.onScrollbarChange = { [weak self] scrollbar in
            self?.updateScrollbar(scrollbar)
        }
        terminalView.onCellSizeChange = { [weak self] cellSize in
            self?.updateCellSize(cellSize)
        }
        scrollView.terminalView = terminalView
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.hasVerticalScroller = false
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = false
        scrollView.usesPredominantAxisScrolling = true
        scrollView.scrollerStyle = .overlay
        scrollView.drawsBackground = false
        scrollView.backgroundColor = .clear
        scrollView.contentView.drawsBackground = false
        scrollView.contentView.backgroundColor = .clear
        scrollView.contentView.clipsToBounds = true
        scrollView.contentView.postsBoundsChangedNotifications = true

        scrollView.documentView = documentView
        documentView.addSubview(terminalView)
        addSubview(scrollView)

        boundsObserver = NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification,
            object: scrollView.contentView,
            queue: .main
        ) { [weak self] _ in
            self?.handleScrollChange()
        }
        liveScrollObservers = [
            NotificationCenter.default.addObserver(
                forName: NSScrollView.willStartLiveScrollNotification,
                object: scrollView,
                queue: .main
            ) { [weak self] _ in
                self?.beginLiveScroll()
            },
            NotificationCenter.default.addObserver(
                forName: NSScrollView.didLiveScrollNotification,
                object: scrollView,
                queue: .main
            ) { [weak self] _ in
                self?.handleLiveScrollUpdate()
            },
            NotificationCenter.default.addObserver(
                forName: NSScrollView.didEndLiveScrollNotification,
                object: scrollView,
                queue: .main
            ) { [weak self] _ in
                self?.endLiveScroll()
            }
        ]

        NSLayoutConstraint.activate([
            scrollView.leadingAnchor.constraint(equalTo: leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: trailingAnchor),
            scrollView.topAnchor.constraint(equalTo: topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: bottomAnchor)
        ])
    }

    public override func layout() {
        super.layout()
        synchronizeGeometry()
    }

    public func processOutput(_ data: Data) {
        terminalView?.processOutput(data)
    }

    public func teardownTerminalSurface() {
        terminalView?.teardownTerminalSurface()
    }

    public func activateTerminalFocus() {
        terminalView?.setTerminalFocus(true)
    }

    public func deactivateTerminalFocus() {
        terminalView?.setTerminalFocus(false)
    }

    public func setTerminalResizeReportingEnabled(_ isEnabled: Bool) {
        terminalView?.setResizeReportingEnabled(isEnabled)
    }

    private func updateScrollbar(_ scrollbar: GhosttyScrollbar) {
        let previousScrollbar = self.scrollbar
        guard previousScrollbar != scrollbar else {
            return
        }
        self.scrollbar = scrollbar
        let shouldShow = scrollbar.total > scrollbar.len && scrollbar.len > 0
        if scrollView.hasVerticalScroller != shouldShow {
            scrollView.hasVerticalScroller = shouldShow
            scrollView.tile()
        }
        let wasSynchronizingScrollView = isSynchronizingScrollView
        isSynchronizingScrollView = true
        if previousScrollbar?.total != scrollbar.total || previousScrollbar?.len != scrollbar.len {
            synchronizeGeometry()
        }
        if previousScrollbar?.offset != scrollbar.offset,
           shouldSynchronizeScrollPosition(for: scrollbar) {
            synchronizeScrollPosition()
        }
        synchronizeTerminalOrigin()
        isSynchronizingScrollView = wasSynchronizingScrollView
    }

    private func updateCellSize(_ cellSize: CGSize) {
        guard cellSize.height > 0 else { return }
        explicitCellHeight = cellSize.height
        synchronizeGeometry()
    }

    private func handleScrollChange() {
        synchronizeTerminalOrigin()
        guard !isSynchronizingScrollView else { return }
        updateUserScrollStateFromVisibleRect()
        if isLiveScrolling {
            sendScrollPositionToGhostty()
        }
    }

    private func beginLiveScroll() {
        isLiveScrolling = true
        updateUserScrollStateFromVisibleRect()
    }

    private func handleLiveScrollUpdate() {
        synchronizeTerminalOrigin()
        updateUserScrollStateFromVisibleRect()
        sendScrollPositionToGhostty()
    }

    private func endLiveScroll() {
        updateUserScrollStateFromVisibleRect()
        isLiveScrolling = false
    }

    private func synchronizeGeometry() {
        guard let terminalView else { return }
        scrollView.layoutSubtreeIfNeeded()
        let contentSize = scrollView.contentSize
        setFrameSizeIfNeeded(terminalView, size: contentSize)
        setFrameSizeIfNeeded(documentView, size: CGSize(
            width: contentSize.width,
            height: documentHeight(for: contentSize.height)
        ))
        synchronizeTerminalOrigin()
    }

    private func synchronizeScrollPosition() {
        guard let scrollbar,
              let cellHeight = cellHeight(for: scrollbar),
              cellHeight > 0 else {
            return
        }

        let targetY = mappedDocumentOffset(
            CGFloat(scrollbar.rowsBelowVisibleRegion) * cellHeight,
            contentHeight: scrollView.contentView.bounds.height
        )
        let maxY = max(0, documentView.frame.height - scrollView.contentView.bounds.height)
        let targetOrigin = CGPoint(x: 0, y: max(0, min(maxY, targetY)))
        guard !pointsApproximatelyEqual(scrollView.contentView.bounds.origin, targetOrigin) else {
            lastSentRow = Int(scrollbar.offset)
            updateUserScrollStateFromVisibleRect()
            return
        }

        let wasSynchronizingScrollView = isSynchronizingScrollView
        isSynchronizingScrollView = true
        scrollView.contentView.scroll(to: targetOrigin)
        scrollView.reflectScrolledClipView(scrollView.contentView)
        isSynchronizingScrollView = wasSynchronizingScrollView
        lastSentRow = Int(scrollbar.offset)
        updateUserScrollStateFromVisibleRect()
    }

    private func synchronizeTerminalOrigin() {
        guard let terminalView else { return }
        let visibleOrigin = scrollView.contentView.documentVisibleRect.origin
        guard !pointsApproximatelyEqual(terminalView.frame.origin, visibleOrigin) else { return }
        setFrameOriginIfNeeded(terminalView, origin: visibleOrigin)
    }

    private func sendScrollPositionToGhostty() {
        guard let terminalView,
              let scrollbar,
              let cellHeight = cellHeight(for: scrollbar),
              cellHeight > 0 else {
            return
        }

        let visibleRect = scrollView.contentView.documentVisibleRect
        let scrollOffset = unmappedDocumentOffset(
            documentView.frame.height - visibleRect.origin.y - visibleRect.height,
            contentHeight: visibleRect.height
        )
        let row = max(0, Int((scrollOffset / cellHeight).rounded(.towardZero)))
        guard row != lastSentRow else { return }
        lastSentRow = row
        userScrolledAwayFromBottom = row > 0
        _ = terminalView.performBindingAction("scroll_to_row:\(row)")
    }

    private func shouldSynchronizeScrollPosition(for scrollbar: GhosttyScrollbar) -> Bool {
        if isLiveScrolling {
            return false
        }
        if userScrolledAwayFromBottom, scrollbar.rowsBelowVisibleRegion > 0 {
            return false
        }
        if scrollbar.rowsBelowVisibleRegion == 0 {
            userScrolledAwayFromBottom = false
        }
        return true
    }

    private func updateUserScrollStateFromVisibleRect() {
        let visibleRect = scrollView.contentView.documentVisibleRect
        userScrolledAwayFromBottom = visibleRect.origin.y > 0.5
    }

    private func documentHeight(for contentHeight: CGFloat) -> CGFloat {
        guard let scrollbar,
              let cellHeight = cellHeight(for: scrollbar) else {
            return contentHeight
        }
        let documentGridHeight = CGFloat(scrollbar.total) * cellHeight
        let padding = contentHeight - CGFloat(scrollbar.len) * cellHeight
        let naturalHeight = max(contentHeight, documentGridHeight + padding)
        return min(naturalHeight, maxVirtualDocumentHeight)
    }

    private func cellHeight(for scrollbar: GhosttyScrollbar) -> CGFloat? {
        if let explicitCellHeight, explicitCellHeight > 0 {
            return explicitCellHeight
        }
        guard scrollbar.len > 0 else { return nil }
        let contentHeight = max(scrollView.contentSize.height, terminalView?.bounds.height ?? 0)
        guard contentHeight > 0 else { return nil }
        return contentHeight / CGFloat(scrollbar.len)
    }

    private func pointsApproximatelyEqual(_ lhs: CGPoint, _ rhs: CGPoint) -> Bool {
        abs(lhs.x - rhs.x) <= 0.5 && abs(lhs.y - rhs.y) <= 0.5
    }

    private func mappedDocumentOffset(_ offset: CGFloat, contentHeight: CGFloat) -> CGFloat {
        offset * virtualDocumentScale(contentHeight: contentHeight)
    }

    private func unmappedDocumentOffset(_ offset: CGFloat, contentHeight: CGFloat) -> CGFloat {
        let scale = virtualDocumentScale(contentHeight: contentHeight)
        guard scale > 0 else { return offset }
        return offset / scale
    }

    private func virtualDocumentScale(contentHeight: CGFloat) -> CGFloat {
        guard let scrollbar,
              let cellHeight = cellHeight(for: scrollbar) else {
            return 1
        }
        let naturalHeight = max(contentHeight, CGFloat(scrollbar.total) * cellHeight)
        guard naturalHeight > maxVirtualDocumentHeight,
              naturalHeight > contentHeight,
              maxVirtualDocumentHeight > contentHeight else {
            return 1
        }
        return (maxVirtualDocumentHeight - contentHeight) / (naturalHeight - contentHeight)
    }

    private func setFrameSizeIfNeeded(_ view: NSView, size: CGSize) {
        guard !sizesApproximatelyEqual(view.frame.size, size) else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        view.frame.size = size
        CATransaction.commit()
    }

    private func setFrameOriginIfNeeded(_ view: NSView, origin: CGPoint) {
        guard !pointsApproximatelyEqual(view.frame.origin, origin) else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        view.frame.origin = origin
        CATransaction.commit()
    }

    private func sizesApproximatelyEqual(_ lhs: CGSize, _ rhs: CGSize) -> Bool {
        abs(lhs.width - rhs.width) <= 0.5 && abs(lhs.height - rhs.height) <= 0.5
    }
}

private struct GhosttyScrollbar: Equatable {
    var total: UInt64
    var offset: UInt64
    var len: UInt64

    var rowsBelowVisibleRegion: UInt64 {
        let bottomOffset = total > len ? total - len : 0
        return bottomOffset > offset ? bottomOffset - offset : 0
    }

    init(_ cScrollbar: ghostty_action_scrollbar_s) {
        self.total = cScrollbar.total
        self.offset = cScrollbar.offset
        self.len = cScrollbar.len
    }
}

private final class GhosttyRuntime {
    static let shared = GhosttyRuntime()

    private let lock = NSLock()
    private var tickScheduled = false
    private(set) var app: ghostty_app_t?
    private var config: ghostty_config_t?
    private var surfacesByPointer: [UInt: WeakGhosttySurfaceOwner] = [:]
    private(set) var lastReloadError: String?
    private var mouseHidden = false
    private var secureInputEnabled = false
    private var fontFamily: String?
    private var fontPoints: Double = 13
    private var fontReload: DispatchWorkItem?
    private var resolvedFontPoints: Float = 13

    private init() {
        initialize()
    }

    deinit {
        if let app {
            ghostty_app_free(app)
        }
        if let config {
            ghostty_config_free(config)
        }
    }

    private func initialize() {
        configureGhosttyEnvironment()

        let result = ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv)
        guard result == GHOSTTY_SUCCESS else {
            ghosttyLogger.error("Ghostty init failed result=\(result, privacy: .public)")
            return
        }

        guard let config = makeConfiguration(source: "initial") else {
            return
        }

        var runtimeConfig = ghostty_runtime_config_s()
        runtimeConfig.userdata = Unmanaged.passUnretained(self).toOpaque()
        runtimeConfig.supports_selection_clipboard = false
        runtimeConfig.wakeup_cb = { userdata in
            guard let userdata else { return }
            Unmanaged<GhosttyRuntime>
                .fromOpaque(userdata)
                .takeUnretainedValue()
                .scheduleTick()
        }
        // The clipboard callbacks get the surface's userdata, its
        // `SurfaceContext`, not this runtime's. A read that starts must
        // complete, or Ghostty leaks its request.
        runtimeConfig.read_clipboard_cb = { userdata, clipboard, state, mimes, mimeCount, list in
            guard let userdata, clipboard == GHOSTTY_CLIPBOARD_STANDARD,
                  let view = Unmanaged<SurfaceContext>.fromOpaque(userdata).takeUnretainedValue().view else {
                return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED
            }
            // With Kitty paste events (mode 5522) a paste only lists the
            // types; the program then reads the one it wants.
            if list, mimeCount == 0 {
                guard NSPasteboard.general.availableType(from: [.string]) != nil else {
                    return GHOSTTY_CLIPBOARD_READ_UNAVAILABLE
                }
                view.completeClipboardListing(state: state)
                return GHOSTTY_CLIPBOARD_READ_STARTED
            }
            // Ottam serves plain text only: a Kitty read of another MIME type
            // is unsupported, not an empty clipboard.
            guard !list, GhosttyRuntime.requestsPlainText(mimes, count: mimeCount) else {
                return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED
            }
            guard let text = NSPasteboard.general.string(forType: .string) else {
                return GHOSTTY_CLIPBOARD_READ_UNAVAILABLE
            }
            view.completeClipboardRequest(text, state: state, confirmed: false)
            return GHOSTTY_CLIPBOARD_READ_STARTED
        }
        // Ghostty asks before a paste that could run a command, and before a
        // program reads the clipboard over OSC 52 (`clipboard-read = ask`).
        // Paste goes through, as it does in Terminal; the program reads an
        // empty clipboard. Anything else that asks, such as Kitty clipboard
        // access, is refused. Completing here, inside the read's completion,
        // is safe: Ghostty holds no lock and keeps the request for it.
        runtimeConfig.confirm_read_clipboard_cb = { userdata, confirm, state, request in
            guard let userdata,
                  let view = Unmanaged<SurfaceContext>.fromOpaque(userdata).takeUnretainedValue().view else { return }
            switch request {
            case GHOSTTY_CLIPBOARD_REQUEST_PASTE:
                let text = confirm.flatMap {
                    GhosttyRuntime.plainText(from: $0.pointee.contents, count: $0.pointee.contents_len)
                }
                view.completeClipboardRequest(text ?? "", state: state, confirmed: true)
            case GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ:
                view.completeClipboardRequest("", state: state, confirmed: true)
            default:
                view.denyClipboardRequest(state: state)
            }
        }
        // Ottam has no prompt, so a write that asks (`clipboard-write = ask`)
        // is refused.
        runtimeConfig.write_clipboard_cb = { _, clipboard, contents, count, confirm in
            guard !confirm, let contents else { return }
            GhosttyRuntime.writeClipboard(clipboard: clipboard, contents: contents, count: count)
        }
        runtimeConfig.action_cb = { _, target, action in
            switch action.tag {
            case GHOSTTY_ACTION_CELL_SIZE:
                guard target.tag == GHOSTTY_TARGET_SURFACE,
                      let owner = GhosttyRuntime.shared.surfaceOwner(for: target.target.surface) else {
                    return false
                }
                owner.updateCellSize(action.action.cell_size)
                return true
            case GHOSTTY_ACTION_SCROLLBAR:
                guard target.tag == GHOSTTY_TARGET_SURFACE,
                      let owner = GhosttyRuntime.shared.surfaceOwner(for: target.target.surface) else {
                    return false
                }
                owner.enqueueScrollbarUpdate(GhosttyScrollbar(action.action.scrollbar))
                return true
            case GHOSTTY_ACTION_SET_TITLE:
                if let title = action.action.set_title.title {
                    ghosttyLogger.debug("Ghostty title changed title=\(String(cString: title), privacy: .private)")
                }
                return true
            case GHOSTTY_ACTION_SET_TAB_TITLE:
                if let title = action.action.set_tab_title.title {
                    ghosttyLogger.debug("Ghostty tab title changed title=\(String(cString: title), privacy: .private)")
                }
                return true
            case GHOSTTY_ACTION_MOUSE_SHAPE:
                GhosttyRuntime.shared.updateMouseShape(action.action.mouse_shape)
                return true
            case GHOSTTY_ACTION_MOUSE_VISIBILITY:
                GhosttyRuntime.shared.updateMouseVisibility(action.action.mouse_visibility)
                return true
            case GHOSTTY_ACTION_MOUSE_OVER_LINK:
                if let url = action.action.mouse_over_link.url {
                    ghosttyLogger.debug("Ghostty mouse over link url=\(String(cString: url), privacy: .private)")
                }
                return true
            case GHOSTTY_ACTION_OPEN_URL:
                GhosttyRuntime.shared.openURL(action.action.open_url)
                return true
            case GHOSTTY_ACTION_RING_BELL:
                NSSound.beep()
                return true
            case GHOSTTY_ACTION_SECURE_INPUT:
                GhosttyRuntime.shared.updateSecureInput(action.action.secure_input)
                return true
            case GHOSTTY_ACTION_RELOAD_CONFIG:
                Task { @MainActor in
                    _ = GhosttyRuntime.shared.reloadConfiguration(source: "ghostty-action")
                }
                return true
            case GHOSTTY_ACTION_START_SEARCH:
                if let needle = action.action.start_search.needle {
                    ghosttyLogger.debug("Ghostty search started needle=\(String(cString: needle), privacy: .private)")
                }
                return true
            case GHOSTTY_ACTION_END_SEARCH, GHOSTTY_ACTION_SEARCH_TOTAL, GHOSTTY_ACTION_SEARCH_SELECTED:
                return true
            case GHOSTTY_ACTION_COPY_TITLE_TO_CLIPBOARD:
                return true
            case GHOSTTY_ACTION_COMMAND_FINISHED:
                ghosttyLogger.info("Ghostty command finished")
                return true
            default:
                return false
            }
        }
        runtimeConfig.close_surface_cb = { _, keepOpen in
            ghosttyLogger.info("Ghostty requested surface close keepOpen=\(keepOpen, privacy: .public)")
        }

        guard let app = ghostty_app_new(&runtimeConfig, config) else {
            ghosttyLogger.error("Ghostty app creation failed")
            ghostty_config_free(config)
            return
        }

        self.app = app
        self.config = config
        ghostty_app_set_focus(app, NSApp.isActive)

        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let app = self?.app else { return }
            ghostty_app_set_focus(app, true)
        }

        NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let app = self?.app else { return }
            ghostty_app_set_focus(app, false)
        }
    }

    private func configureGhosttyEnvironment() {
        let fileManager = FileManager.default
        if let ghosttyResourcesURL = Bundle.main.resourceURL?.appendingPathComponent("ghostty", isDirectory: true),
           fileManager.fileExists(atPath: ghosttyResourcesURL.path) {
            setenv("GHOSTTY_RESOURCES_DIR", ghosttyResourcesURL.path, 1)
        } else if getenv("GHOSTTY_RESOURCES_DIR") == nil,
                  fileManager.fileExists(atPath: "/Applications/Ghostty.app/Contents/Resources/ghostty") {
            setenv("GHOSTTY_RESOURCES_DIR", "/Applications/Ghostty.app/Contents/Resources/ghostty", 1)
        }

        if let terminfoURL = Bundle.main.resourceURL?.appendingPathComponent("terminfo", isDirectory: true),
           fileManager.fileExists(atPath: terminfoURL.path) {
            setenv("TERMINFO", terminfoURL.path, 1)
        }

        if getenv("TERM") == nil {
            setenv("TERM", "xterm-ghostty", 1)
        }
        if getenv("COLORTERM") == nil {
            setenv("COLORTERM", "truecolor", 1)
        }
        if getenv("TERM_PROGRAM") == nil {
            setenv("TERM_PROGRAM", "Ottam", 1)
        }
    }

    private static func writeClipboard(
        clipboard: ghostty_clipboard_e,
        contents: UnsafePointer<ghostty_clipboard_content_s>,
        count: Int
    ) {
        guard clipboard == GHOSTTY_CLIPBOARD_STANDARD,
              let text = plainText(from: contents, count: count) else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    private static func requestsPlainText(_ mimes: UnsafePointer<UnsafePointer<CChar>?>?, count: Int) -> Bool {
        guard let mimes else { return false }
        return (0..<count).contains { mimes[$0].map(isTextMime) ?? false }
    }

    /// Ghostty's text aliases, from `terminal.clipboard.isTextMime`.
    private static func isTextMime(_ mime: UnsafePointer<CChar>) -> Bool {
        ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING", "TEXT", "STRING"]
            .contains(String(cString: mime))
    }

    /// Clipboard data is sized by `len` and carries no terminating NUL.
    private static func plainText(
        from contents: UnsafePointer<ghostty_clipboard_content_s>?,
        count: Int
    ) -> String? {
        guard let contents else { return nil }
        let buffer = UnsafeBufferPointer(start: contents, count: count)
        guard let content = buffer.first(where: { $0.mime.map(isTextMime) ?? false }),
              let data = content.data else {
            return nil
        }
        return String(decoding: UnsafeRawBufferPointer(start: data, count: content.len), as: UTF8.self)
    }

    private func updateMouseShape(_ shape: ghostty_action_mouse_shape_e) {
        switch shape {
        case GHOSTTY_MOUSE_SHAPE_POINTER:
            NSCursor.pointingHand.set()
        case GHOSTTY_MOUSE_SHAPE_TEXT, GHOSTTY_MOUSE_SHAPE_CELL:
            NSCursor.iBeam.set()
        case GHOSTTY_MOUSE_SHAPE_CROSSHAIR:
            NSCursor.crosshair.set()
        case GHOSTTY_MOUSE_SHAPE_NOT_ALLOWED, GHOSTTY_MOUSE_SHAPE_NO_DROP:
            NSCursor.operationNotAllowed.set()
        case GHOSTTY_MOUSE_SHAPE_COL_RESIZE, GHOSTTY_MOUSE_SHAPE_EW_RESIZE:
            NSCursor.resizeLeftRight.set()
        case GHOSTTY_MOUSE_SHAPE_ROW_RESIZE, GHOSTTY_MOUSE_SHAPE_NS_RESIZE:
            NSCursor.resizeUpDown.set()
        default:
            NSCursor.arrow.set()
        }
    }

    private func updateMouseVisibility(_ visibility: ghostty_action_mouse_visibility_e) {
        switch visibility {
        case GHOSTTY_MOUSE_HIDDEN where !mouseHidden:
            NSCursor.hide()
            mouseHidden = true
        case GHOSTTY_MOUSE_VISIBLE where mouseHidden:
            NSCursor.unhide()
            mouseHidden = false
        default:
            break
        }
    }

    private func updateSecureInput(_ secureInput: ghostty_action_secure_input_e) {
        switch secureInput {
        case GHOSTTY_SECURE_INPUT_ON where !secureInputEnabled:
            EnableSecureEventInput()
            secureInputEnabled = true
        case GHOSTTY_SECURE_INPUT_OFF where secureInputEnabled:
            DisableSecureEventInput()
            secureInputEnabled = false
        case GHOSTTY_SECURE_INPUT_TOGGLE:
            if secureInputEnabled {
                DisableSecureEventInput()
            } else {
                EnableSecureEventInput()
            }
            secureInputEnabled.toggle()
        default:
            break
        }
    }

    private func openURL(_ openURL: ghostty_action_open_url_s) {
        guard let urlPointer = openURL.url,
              let url = URL(string: String(cString: urlPointer)) else {
            return
        }
        NSWorkspace.shared.open(url)
    }

    private func makeConfiguration(source: String) -> ghostty_config_t? {
        guard let config = ghostty_config_new() else {
            lastReloadError = "Ghostty config creation failed."
            ghosttyLogger.error("Ghostty config creation failed source=\(source, privacy: .public)")
            return nil
        }

        ghostty_config_load_default_files(config)
        // Beneath the overlay deliberately: Ghostty's config is last-wins, so a
        // hand-edited overlay file still beats the app's own font setting.
        guard loadConfig(fontDefaultsConfig, into: config, prefix: "ottam-defaults") else {
            ghostty_config_free(config)
            return nil
        }
        if let overlayURL = ensureOverlayConfigFile() {
            overlayURL.path.withCString { pathPointer in
                ghostty_config_load_file(config, pathPointer)
            }
            ghostty_config_load_recursive_files(config)
        } else {
            ghosttyLogger.error("Ghostty overlay config path unavailable source=\(source, privacy: .public)")
        }

        guard loadConfig(
            """
            macos-titlebar-proxy-icon = hidden
            shell-integration = none
            window-decoration = false
            """,
            into: config,
            prefix: "ottam-runtime"
        ) else {
            ghostty_config_free(config)
            return nil
        }
        ghostty_config_finalize(config)

        let diagnosticsCount = ghostty_config_diagnostics_count(config)
        if diagnosticsCount > 0 {
            for index in 0..<diagnosticsCount {
                let diagnostic = ghostty_config_get_diagnostic(config, index)
                if let message = diagnostic.message {
                    ghosttyLogger.error(
                        "Ghostty config diagnostic source=\(source, privacy: .public) message=\(String(cString: message), privacy: .public)"
                    )
                }
            }
        }

        var size: Float = 0
        if "font-size".withCString({ ghostty_config_get(config, &size, $0, UInt("font-size".utf8.count)) }),
           size > 0 {
            resolvedFontPoints = size
        }

        lastReloadError = nil
        return config
    }

    private func ensureOverlayConfigFile() -> URL? {
        let fileManager = FileManager.default
        guard let supportDirectory = try? fileManager.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else {
            return nil
        }
        let bundleID = Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam"
        let appDirectory = supportDirectory.appendingPathComponent(bundleID, isDirectory: true)
        let overlayURL = appDirectory.appendingPathComponent("config.ghostty", isDirectory: false)
        do {
            try fileManager.createDirectory(at: appDirectory, withIntermediateDirectories: true)
            if !fileManager.fileExists(atPath: overlayURL.path) {
                try defaultOverlayConfig.write(to: overlayURL, atomically: true, encoding: .utf8)
            }
            return overlayURL.resolvingSymlinksInPath()
        } catch {
            lastReloadError = error.localizedDescription
            ghosttyLogger.error("failed to prepare Ghostty overlay config: \(error.localizedDescription, privacy: .private)")
            return nil
        }
    }

    private var defaultOverlayConfig: String {
        """
        # Ottam Ghostty overlay. Edit this file and reload Ottam terminals.
        scrollback-limit = 10000000
        background = #101214
        foreground = #E7ECEF
        cursor-color = #A7C957

        """
    }

    /// The app's code font, as a config layer.
    ///
    /// No `font-family` line when the family is nil — that is the system font,
    /// and Ghostty's own default is already the system monospaced face.
    private var fontDefaultsConfig: String {
        // "%g" rather than interpolation: Double prints 13.0, and Ghostty wants a
        // bare number.
        var lines = [String(format: "font-size = %g", fontPoints)]
        if let fontFamily {
            // Emitted, but inert today: the embedded surface ignores
            // `font-family` from every config layer — verified by putting
            // `font-family = Courier New` in the overlay file, which changed
            // nothing while `background` in the same file did. Kept because it
            // is the correct key and costs nothing if Ghostty starts honoring it.
            lines.append("font-family = \(fontFamily)")
        }
        return lines.joined(separator: "\n") + "\n"
    }

    /// Applied on the next reload. Coalesced, because a font picker fires on
    /// every arrow-key step and each reload rebuilds every surface's config.
    /// Read when a surface is created. `ghostty_surface_config_s.font_size`
    /// overrides whatever the config says, so it has to be fed *from* the
    /// config — otherwise every layer beneath it, the overlay file included, is
    /// silently dead. This is the finalized value, so an overlay still wins.
    @MainActor
    var currentFontPoints: Float { resolvedFontPoints }

    @MainActor
    func setFont(family: String?, points: Double) {
        guard family != fontFamily || points != fontPoints else { return }
        fontFamily = family
        fontPoints = points

        fontReload?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                _ = self?.reloadConfiguration(source: "typography")
            }
        }
        fontReload = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2, execute: work)
    }

    /// Whether a hand-edited overlay sets `font-size`, so Settings can say why
    /// the Code Font size is not reaching the terminal. Only the size: the
    /// embedded surface ignores `font-family` from every layer (see
    /// `fontDefaultsConfig`), so an overlay family overrides nothing.
    @MainActor
    func overlayPinsFontSize() -> Bool {
        guard let overlayURL = overlayConfigURL(),
              let contents = try? String(contentsOf: overlayURL, encoding: .utf8)
        else { return false }

        return contents.split(separator: "\n").contains { line in
            line.split(separator: "=", maxSplits: 1).first?
                .trimmingCharacters(in: .whitespaces) == "font-size"
        }
    }

    /// Ghostty loads config from paths only, so each in-memory layer goes
    /// through a short-lived file named after the layer for its diagnostics.
    private func loadConfig(_ contents: String, into config: ghostty_config_t, prefix: String) -> Bool {
        let fileManager = FileManager.default
        let fileURL = fileManager.temporaryDirectory
            .appendingPathComponent("\(prefix)-\(UUID().uuidString).ghostty")
        do {
            try contents.write(to: fileURL, atomically: true, encoding: .utf8)
        } catch {
            lastReloadError = error.localizedDescription
            ghosttyLogger.error("Ghostty config layer write failed layer=\(prefix, privacy: .public)")
            return false
        }
        defer { try? fileManager.removeItem(at: fileURL) }
        fileURL.path.withCString { ghostty_config_load_file(config, $0) }
        return true
    }

    private func scheduleTick() {
        lock.lock()
        let shouldSchedule = !tickScheduled
        if shouldSchedule {
            tickScheduled = true
        }
        lock.unlock()

        guard shouldSchedule else { return }
        DispatchQueue.main.async { [weak self] in
            self?.tick()
        }
    }

    private func tick() {
        lock.lock()
        tickScheduled = false
        lock.unlock()

        guard let app else { return }
        ghostty_app_tick(app)
    }

    @MainActor
    @discardableResult
    func reloadConfiguration(source: String) -> Bool {
        guard let app else {
            lastReloadError = "Ghostty app is unavailable."
            return false
        }
        guard let newConfig = makeConfiguration(source: source) else {
            return false
        }

        ghostty_app_update_config(app, newConfig)
        for owner in surfaceOwners() {
            owner.updateConfiguration(newConfig)
        }
        let oldConfig = config
        config = newConfig
        if let oldConfig {
            ghostty_config_free(oldConfig)
        }
        NotificationCenter.default.post(
            name: .ottamGhosttyConfigDidReload,
            object: self,
            userInfo: ["source": source]
        )
        ghosttyLogger.info("Ghostty config reloaded source=\(source, privacy: .public)")
        return true
    }

    @MainActor
    func overlayConfigURL() -> URL? {
        ensureOverlayConfigFile()
    }

    func register(_ owner: GhosttySurfaceOwner, for surface: ghostty_surface_t) {
        lock.lock()
        surfacesByPointer[pointerKey(for: surface)] = WeakGhosttySurfaceOwner(owner)
        lock.unlock()
    }

    func unregister(surface: ghostty_surface_t) {
        lock.lock()
        surfacesByPointer.removeValue(forKey: pointerKey(for: surface))
        lock.unlock()
    }

    private func surfaceOwner(for surface: ghostty_surface_t) -> GhosttySurfaceOwner? {
        lock.lock()
        let owner = surfacesByPointer[pointerKey(for: surface)]?.owner
        lock.unlock()
        return owner
    }

    private func surfaceOwners() -> [GhosttySurfaceOwner] {
        lock.lock()
        let owners = surfacesByPointer.values.compactMap(\.owner)
        lock.unlock()
        return owners
    }

    private func pointerKey(for surface: ghostty_surface_t) -> UInt {
        UInt(bitPattern: surface)
    }
}

private final class WeakGhosttySurfaceOwner {
    weak var owner: GhosttySurfaceOwner?

    init(_ owner: GhosttySurfaceOwner) {
        self.owner = owner
    }
}

public extension Notification.Name {
    static let ottamGhosttyConfigDidReload = Notification.Name("ottamGhosttyConfigDidReload")
}

private final class GhosttyScrollView: NSScrollView {
    weak var terminalView: EmbeddedGhosttyView?

    override var acceptsFirstResponder: Bool { false }

    /// Scrolling reads back without taking keyboard focus, as in Terminal:
    /// typing in the composer stays there.
    override func scrollWheel(with event: NSEvent) {
        guard let terminalView else {
            super.scrollWheel(with: event)
            return
        }
        terminalView.scrollWheel(with: event)
    }
}

private struct ByteCappedDataBuffer {
    private var chunks: [Data] = []
    private var startIndex = 0
    private(set) var byteCount = 0

    var isEmpty: Bool {
        startIndex >= chunks.count
    }

    mutating func append(_ data: Data, maxBytes: Int?) {
        guard !data.isEmpty else { return }
        chunks.append(data)
        byteCount += data.count
        if let maxBytes {
            trim(to: maxBytes)
        }
    }

    mutating func removeAll(keepingCapacity: Bool = true) {
        chunks.removeAll(keepingCapacity: keepingCapacity)
        startIndex = 0
        byteCount = 0
    }

    mutating func drain(maxBytes: Int) -> [Data] {
        guard !isEmpty else {
            removeAll()
            return []
        }
        var output: [Data] = []
        var drainedBytes = 0
        while startIndex < chunks.count,
              output.isEmpty || drainedBytes < maxBytes {
            let chunk = chunks[startIndex]
            startIndex += 1
            byteCount -= chunk.count
            output.append(chunk)
            drainedBytes += chunk.count
        }
        compactIfNeeded()
        return output
    }

    private mutating func trim(to maxBytes: Int) {
        while byteCount > maxBytes, startIndex < chunks.count {
            byteCount -= chunks[startIndex].count
            startIndex += 1
        }
        compactIfNeeded()
    }

    private mutating func compactIfNeeded() {
        guard startIndex > 64, startIndex * 2 > chunks.count else { return }
        chunks.removeSubrange(0..<startIndex)
        startIndex = 0
    }
}

private final class GhosttySurfaceOwner {
    weak var view: EmbeddedGhosttyView?
    var onScrollbarChange: ((GhosttyScrollbar) -> Void)?
    var onOutputFlush: (([Data]) -> Void)?
    var onCellSizeChange: ((CGSize) -> Void)?
    private(set) var surface: ghostty_surface_t?
    private var retainedSurfaceContext: Unmanaged<SurfaceContext>?
    private var pendingOutput = ByteCappedDataBuffer()
    private var outputFlushScheduled = false
    private var pendingScrollbar: GhosttyScrollbar?
    private var scrollbarFlushScheduled = false
    private let scrollbarLock = NSLock()
    private(set) var isTearingDown = false
    var desiredFocus = false
    var displayID: UInt32?

    func install(surface newSurface: ghostty_surface_t?, context: Unmanaged<SurfaceContext>) {
        guard let newSurface else {
            return
        }
        assert(Thread.isMainThread)
        surface = newSurface
        retainedSurfaceContext = context
        isTearingDown = false
    }

    func replaceRetainedContext(_ context: Unmanaged<SurfaceContext>) {
        assert(Thread.isMainThread)
        retainedSurfaceContext?.release()
        retainedSurfaceContext = context
    }

    func teardown() {
        assert(Thread.isMainThread)
        guard let surface else {
            retainedSurfaceContext?.release()
            retainedSurfaceContext = nil
            pendingOutput.removeAll()
            return
        }

        isTearingDown = true
        GhosttyRuntime.shared.unregister(surface: surface)
        self.surface = nil
        pendingOutput.removeAll()
        ghostty_surface_free(surface)
        retainedSurfaceContext?.release()
        retainedSurfaceContext = nil
    }

    func bufferOutput(_ data: Data) {
        guard !data.isEmpty, !isTearingDown else { return }
        let maxPendingOutputBytes = 128 * 1024
        pendingOutput.append(data, maxBytes: surface == nil ? maxPendingOutputBytes : nil)
        scheduleOutputFlush()
    }

    func scheduleOutputFlush() {
        guard surface != nil, !isTearingDown, !outputFlushScheduled else { return }
        outputFlushScheduled = true
        DispatchQueue.main.async { [weak self] in
            self?.flushQueuedOutput()
        }
    }

    private func flushQueuedOutput() {
        outputFlushScheduled = false
        guard surface != nil, !isTearingDown, !pendingOutput.isEmpty else { return }

        let maxFlushBytes = 256 * 1024
        let chunks = pendingOutput.drain(maxBytes: maxFlushBytes)

        onOutputFlush?(chunks)
        if !pendingOutput.isEmpty {
            scheduleOutputFlush()
        }
    }

    func enqueueScrollbarUpdate(_ scrollbar: GhosttyScrollbar) {
        guard surface != nil, !isTearingDown else { return }
        scrollbarLock.lock()
        pendingScrollbar = scrollbar
        let shouldSchedule = !scrollbarFlushScheduled
        if shouldSchedule {
            scrollbarFlushScheduled = true
        }
        scrollbarLock.unlock()

        guard shouldSchedule else { return }
        DispatchQueue.main.async { [weak self] in
            self?.flushPendingScrollbar()
        }
    }

    private func flushPendingScrollbar() {
        scrollbarLock.lock()
        scrollbarFlushScheduled = false
        let scrollbar = pendingScrollbar
        pendingScrollbar = nil
        scrollbarLock.unlock()

        guard let scrollbar, surface != nil, !isTearingDown else { return }
        onScrollbarChange?(scrollbar)
    }

    func updateCellSize(_ cellSize: ghostty_action_cell_size_s) {
        guard surface != nil, !isTearingDown else { return }
        let size = CGSize(width: CGFloat(cellSize.width), height: CGFloat(cellSize.height))
        DispatchQueue.main.async { [weak self] in
            guard let self, self.surface != nil, !self.isTearingDown else { return }
            self.onCellSizeChange?(size)
        }
    }

    func updateConfiguration(_ config: ghostty_config_t) {
        guard let surface, !isTearingDown else { return }
        assert(Thread.isMainThread)
        ghostty_surface_update_config(surface, config)
        view?.handleConfigurationReload()
    }
}

private final class EmbeddedGhosttyView: NSView {
    private let configuration: GhosttyTerminalConfiguration
    private let onStateChange: (String) -> Void
    private let surfaceOwner = GhosttySurfaceOwner()
    private var lastPixelSize: CGSize = .zero
    private var lastAppliedScale: CGFloat = 0
    private var lastDrawableSize: CGSize = .zero
    private var lastReportedGrid: (rows: UInt16, cols: UInt16)?
    private var resizeReportingEnabled = true
    var onScrollbarChange: ((GhosttyScrollbar) -> Void)?
    var onCellSizeChange: ((CGSize) -> Void)?

    init(configuration: GhosttyTerminalConfiguration, onStateChange: @escaping (String) -> Void) {
        self.configuration = configuration
        self.onStateChange = onStateChange
        super.init(frame: .zero)
        surfaceOwner.view = self
        surfaceOwner.onScrollbarChange = { [weak self] scrollbar in
            self?.onScrollbarChange?(scrollbar)
        }
        surfaceOwner.onOutputFlush = { [weak self] chunks in
            self?.processOutputBatch(chunks)
        }
        surfaceOwner.onCellSizeChange = { [weak self] cellSize in
            self?.onCellSizeChange?(cellSize)
        }
        setup()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    deinit {
        surfaceOwner.teardown()
    }

    override var acceptsFirstResponder: Bool { true }
    override var isOpaque: Bool { true }

    // Ghostty's focus follows the responder chain, so clicking into the
    // terminal, or out of it, also draws its cursor and reports focus.

    override func becomeFirstResponder() -> Bool {
        guard super.becomeFirstResponder() else { return false }
        syncGhosttyFocus(true)
        return true
    }

    override func resignFirstResponder() -> Bool {
        guard super.resignFirstResponder() else { return false }
        syncGhosttyFocus(false)
        return true
    }

    private func syncGhosttyFocus(_ focused: Bool) {
        surfaceOwner.desiredFocus = focused
        if let surface = surfaceOwner.surface {
            ghostty_surface_set_focus(surface, focused)
        }
    }

    override func makeBackingLayer() -> CALayer {
        let layer = CAMetalLayer()
        layer.pixelFormat = .bgra8Unorm
        layer.framebufferOnly = false
        layer.isOpaque = true
        layer.backgroundColor = NSColor.black.cgColor
        return layer
    }

    private func setup() {
        wantsLayer = true
        layer?.masksToBounds = true
        postsFrameChangedNotifications = true
        // Pointer movement reaches Ghostty for its cursor shapes, link hover,
        // and programs that track the mouse.
        addTrackingArea(NSTrackingArea(
            rect: .zero,
            options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self
        ))
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil {
            createSurfaceIfNeeded()
            // Mounting is no request for keyboard focus: a focus asked for
            // before the view had a window lands now.
            if surfaceOwner.desiredFocus { setTerminalFocus(true) }
        }
        updateSurfaceSize()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        updateSurfaceSize()
    }

    override func layout() {
        super.layout()
        updateSurfaceSize()
    }

    override func keyDown(with event: NSEvent) {
        guard let surface = surfaceOwner.surface else {
            super.keyDown(with: event)
            return
        }

        var key = ghosttyKey(from: event)
        if let characters = event.charactersIgnoringModifiers ?? event.characters,
           !characters.isEmpty,
           !isFunctionKeyText(characters) {
            let handled = characters.withCString { pointer in
                key.text = pointer
                return ghostty_surface_key(surface, key)
            }
            if handled { return }
        } else if ghostty_surface_key(surface, key) {
            return
        }

        super.keyDown(with: event)
    }

    /// AppKit encodes arrows, function keys, Home/End, and the rest of the
    /// non-character keys as Unicode private-use code points (U+F700–F8FF,
    /// `NSUpArrowFunctionKey` etc.). Those must never reach ghostty as literal
    /// text — it would write their raw UTF-8 to the PTY as typed characters —
    /// so the key goes through keycode-only encoding instead.
    private func isFunctionKeyText(_ characters: String) -> Bool {
        guard let scalar = characters.unicodeScalars.first else { return false }
        return (0xF700...0xF8FF).contains(scalar.value)
    }

    func processOutput(_ data: Data) {
        surfaceOwner.bufferOutput(data)
    }

    func teardownTerminalSurface() {
        surfaceOwner.teardown()
    }

    func setTerminalFocus(_ focused: Bool) {
        surfaceOwner.desiredFocus = focused
        if focused {
            if window != nil {
                window?.makeFirstResponder(self)
            }
        } else if window?.firstResponder === self {
            window?.makeFirstResponder(nil)
        }
        if let surface = surfaceOwner.surface {
            ghostty_surface_set_focus(surface, focused)
        }
    }

    func setResizeReportingEnabled(_ isEnabled: Bool) {
        guard resizeReportingEnabled != isEnabled else { return }
        resizeReportingEnabled = isEnabled
        if isEnabled {
            lastReportedGrid = nil
            updateSurfaceSize()
        }
    }

    private func processOutputBatch(_ chunks: [Data]) {
        guard let surface = surfaceOwner.surface, !chunks.isEmpty else { return }
        for data in chunks {
            data.withUnsafeBytes { buffer in
                guard let baseAddress = buffer.baseAddress else { return }
                ghostty_surface_process_output(
                    surface,
                    baseAddress.assumingMemoryBound(to: CChar.self),
                    UInt(data.count)
                )
            }
        }
        ghostty_surface_refresh(surface)
    }

    private func flushPendingOutput(on surface: ghostty_surface_t) {
        surfaceOwner.scheduleOutputFlush()
    }

    private func handleManualInput(_ data: Data) {
        configuration.onManualInput?(data)
    }

    func handleConfigurationReload() {
        lastPixelSize = .zero
        lastAppliedScale = 0
        lastDrawableSize = .zero
        lastReportedGrid = nil
        updateSurfaceSize()
        surfaceOwner.scheduleOutputFlush()
        if let surface = surfaceOwner.surface {
            ghostty_surface_refresh(surface)
        }
    }

    override func flagsChanged(with event: NSEvent) {
        super.flagsChanged(with: event)
    }

    // Ghostty owns the mouse: selection, word and line clicks, and reporting
    // to programs that ask for it. Positions keep reporting past the edges
    // during a drag, so a selection scrolls.

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        // A double or triple click selects around the first click's position.
        if event.clickCount == 1 { sendMousePosition(event) }
        sendMouseButton(GHOSTTY_MOUSE_PRESS, event)
    }

    override func mouseUp(with event: NSEvent) {
        sendMouseButton(GHOSTTY_MOUSE_RELEASE, event)
    }

    override func rightMouseDown(with event: NSEvent) {
        sendMousePosition(event)
        if !sendMouseButton(GHOSTTY_MOUSE_PRESS, event) { super.rightMouseDown(with: event) }
    }

    override func rightMouseUp(with event: NSEvent) {
        if !sendMouseButton(GHOSTTY_MOUSE_RELEASE, event) { super.rightMouseUp(with: event) }
    }

    override func otherMouseDown(with event: NSEvent) {
        sendMousePosition(event)
        sendMouseButton(GHOSTTY_MOUSE_PRESS, event)
    }

    override func otherMouseUp(with event: NSEvent) {
        sendMouseButton(GHOSTTY_MOUSE_RELEASE, event)
    }

    override func mouseMoved(with event: NSEvent) { sendMousePosition(event) }
    override func mouseDragged(with event: NSEvent) { sendMousePosition(event) }
    override func rightMouseDragged(with event: NSEvent) { sendMousePosition(event) }
    override func otherMouseDragged(with event: NSEvent) { sendMousePosition(event) }
    override func mouseEntered(with event: NSEvent) { sendMousePosition(event) }

    override func mouseExited(with event: NSEvent) {
        // A drag keeps reporting outside; otherwise the pointer is gone.
        guard NSEvent.pressedMouseButtons == 0, let surface = surfaceOwner.surface else { return }
        ghostty_surface_mouse_pos(surface, -1, -1, modifiers(from: event))
    }

    /// Ghostty measures from the top left, in points.
    private func sendMousePosition(_ event: NSEvent) {
        guard let surface = surfaceOwner.surface else { return }
        let point = convert(event.locationInWindow, from: nil)
        ghostty_surface_mouse_pos(surface, point.x, bounds.height - point.y, modifiers(from: event))
    }

    /// Whether Ghostty used the click; a right click it leaves falls through.
    @discardableResult
    private func sendMouseButton(_ state: ghostty_input_mouse_state_e, _ event: NSEvent) -> Bool {
        // AppKit numbers buttons from 0; Ghostty's 0 is unknown.
        let button = UInt32(event.buttonNumber + 1)
        guard let surface = surfaceOwner.surface, button <= GHOSTTY_MOUSE_ELEVEN.rawValue else { return false }
        return ghostty_surface_mouse_button(
            surface, state, ghostty_input_mouse_button_e(rawValue: button), modifiers(from: event)
        )
    }

    func completeClipboardRequest(_ text: String, state: UnsafeMutableRawPointer?, confirmed: Bool) {
        guard let surface = surfaceOwner.surface else { return }
        "text/plain".withCString { mime in
            text.withCString { data in
                var content = ghostty_clipboard_content_s(mime: mime, data: data, len: text.utf8.count)
                withUnsafePointer(to: &content) { contents in
                    var completion = ghostty_clipboard_complete_s(
                        contents: contents,
                        contents_len: 1,
                        available: nil,
                        available_len: 0,
                        confirmed: confirmed,
                        remember: false
                    )
                    ghostty_surface_complete_clipboard_request(surface, &completion, state)
                }
            }
        }
    }

    func completeClipboardListing(state: UnsafeMutableRawPointer?) {
        guard let surface = surfaceOwner.surface else { return }
        "text/plain".withCString { mime in
            var type: UnsafePointer<CChar>? = mime
            withUnsafePointer(to: &type) { available in
                var completion = ghostty_clipboard_complete_s(
                    contents: nil,
                    contents_len: 0,
                    available: available,
                    available_len: 1,
                    confirmed: false,
                    remember: false
                )
                ghostty_surface_complete_clipboard_request(surface, &completion, state)
            }
        }
    }

    func denyClipboardRequest(state: UnsafeMutableRawPointer?) {
        guard let surface = surfaceOwner.surface else { return }
        ghostty_surface_deny_clipboard_request(surface, state)
    }

    @objc func copy(_ sender: Any?) {
        _ = performBindingAction("copy_to_clipboard")
    }

    @objc func paste(_ sender: Any?) {
        _ = performBindingAction("paste_from_clipboard")
    }

    override func scrollWheel(with event: NSEvent) {
        guard let surface = surfaceOwner.surface else {
            super.scrollWheel(with: event)
            return
        }
        let scroll = ghosttyScroll(from: event)
        ghostty_surface_mouse_scroll(
            surface,
            scroll.x,
            scroll.y,
            scroll.mods
        )
    }

    func performBindingAction(_ action: String) -> Bool {
        guard let surface = surfaceOwner.surface else { return false }
        return action.withCString { cString in
            ghostty_surface_binding_action(surface, cString, UInt(action.utf8.count))
        }
    }

    private func createSurfaceIfNeeded() {
        guard surfaceOwner.surface == nil else { return }
        guard let app = GhosttyRuntime.shared.app else {
            onStateChange("Ghostty unavailable")
            return
        }

        var surfaceConfig = ghostty_surface_config_new()
        surfaceConfig.platform_tag = GHOSTTY_PLATFORM_MACOS
        surfaceConfig.platform = ghostty_platform_u(
            macos: ghostty_platform_macos_s(nsview: Unmanaged.passUnretained(self).toOpaque())
        )
        surfaceConfig.userdata = Unmanaged.passUnretained(self).toOpaque()
        surfaceConfig.scale_factor = Double(window?.backingScaleFactor ?? 1)
        // Not a constant: this field wins over the config's `font-size`, so a
        // hardcode here silently defeats every layer below it.
        surfaceConfig.font_size = Float(GhosttyRuntime.shared.currentFontPoints)
        surfaceConfig.context = GHOSTTY_SURFACE_CONTEXT_WINDOW

        let context = Unmanaged.passRetained(SurfaceContext(view: self))
        surfaceOwner.replaceRetainedContext(context)
        surfaceConfig.userdata = context.toOpaque()

        configuration.workingDirectory.withCString { workingDirectoryPointer in
            surfaceConfig.working_directory = workingDirectoryPointer
            if configuration.manualIO {
                surfaceConfig.io_mode = GHOSTTY_SURFACE_IO_MANUAL
                surfaceConfig.io_write_userdata = context.toOpaque()
                surfaceConfig.io_write_cb = { userdata, bytes, count in
                    guard let userdata, let bytes else { return }
                    let context = Unmanaged<SurfaceContext>
                        .fromOpaque(userdata)
                        .takeUnretainedValue()
                    let data = Data(bytes: bytes, count: Int(count))
                    context.view?.handleManualInput(data)
                }
                surfaceOwner.install(surface: ghostty_surface_new(app, &surfaceConfig), context: context)
            } else if let startupCommand = configuration.startupCommand {
                startupCommand.withCString { commandPointer in
                    surfaceConfig.command = commandPointer
                    surfaceOwner.install(surface: ghostty_surface_new(app, &surfaceConfig), context: context)
                }
            } else {
                surfaceOwner.install(surface: ghostty_surface_new(app, &surfaceConfig), context: context)
            }
        }

        guard let surface = surfaceOwner.surface else {
            onStateChange("Surface failed")
            return
        }
        GhosttyRuntime.shared.register(surfaceOwner, for: surface)

        if let displayID = window?.screen?.displayID ?? NSScreen.main?.displayID {
            surfaceOwner.displayID = displayID
            ghostty_surface_set_display_id(surface, displayID)
        }
        ghostty_surface_set_focus(surface, surfaceOwner.desiredFocus)
        flushPendingOutput(on: surface)
        onStateChange("Ghostty running")
        updateSurfaceSize()
        ghostty_surface_refresh(surface)
    }

    private func ghosttyScroll(from event: NSEvent) -> (x: Double, y: Double, mods: ghostty_input_scroll_mods_t) {
        var x = event.scrollingDeltaX
        var y = event.scrollingDeltaY
        var mods: Int32 = 0

        if event.hasPreciseScrollingDeltas {
            x *= 2
            y *= 2
            mods |= 0b0000_0001
        }

        let momentum: Int32
        switch event.momentumPhase {
        case .began:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_BEGAN.rawValue)
        case .stationary:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_STATIONARY.rawValue)
        case .changed:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_CHANGED.rawValue)
        case .ended:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_ENDED.rawValue)
        case .cancelled:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_CANCELLED.rawValue)
        case .mayBegin:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_MAY_BEGIN.rawValue)
        default:
            momentum = Int32(GHOSTTY_MOUSE_MOMENTUM_NONE.rawValue)
        }
        mods |= momentum << 1

        return (x, y, ghostty_input_scroll_mods_t(mods))
    }

    private func updateSurfaceSize() {
        guard let surface = surfaceOwner.surface,
              let window,
              bounds.width > 0,
              bounds.height > 0 else { return }

        let scale = max(1.0, window.backingScaleFactor)
        let pixelSize = CGSize(
            width: floor(bounds.width * scale),
            height: floor(bounds.height * scale)
        )
        guard pixelSize.width > 0, pixelSize.height > 0 else { return }

        let scaleChanged = abs(scale - lastAppliedScale) > 0.0001
        let pixelSizeChanged = pixelSize != lastPixelSize
        let drawableSizeChanged = pixelSize != lastDrawableSize

        if scaleChanged || drawableSizeChanged {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            layer?.contentsScale = scale
            if let metalLayer = layer as? CAMetalLayer {
                metalLayer.drawableSize = pixelSize
            }
            CATransaction.commit()
            lastAppliedScale = scale
            lastDrawableSize = pixelSize
        }

        guard scaleChanged || pixelSizeChanged else { return }
        lastPixelSize = pixelSize
        if scaleChanged {
            ghostty_surface_set_content_scale(surface, Double(scale), Double(scale))
        }
        if pixelSizeChanged {
            ghostty_surface_set_size(surface, UInt32(pixelSize.width), UInt32(pixelSize.height))
        }
        let size = ghostty_surface_size(surface)
        let grid = (rows: size.rows, cols: size.columns)
        if resizeReportingEnabled,
           (lastReportedGrid?.rows != grid.rows || lastReportedGrid?.cols != grid.cols) {
            lastReportedGrid = grid
            configuration.onResize?(grid.rows, grid.cols)
        }
        ghostty_surface_refresh(surface)
    }

    private func ghosttyKey(from event: NSEvent) -> ghostty_input_key_s {
        var key = ghostty_input_key_s()
        key.action = event.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS
        key.keycode = UInt32(event.keyCode)
        key.mods = modifiers(from: event)
        key.consumed_mods = GHOSTTY_MODS_NONE
        key.composing = false
        key.text = nil
        key.unshifted_codepoint = unshiftedCodepoint(from: event)
        return key
    }

    private func modifiers(from event: NSEvent) -> ghostty_input_mods_e {
        var raw = GHOSTTY_MODS_NONE.rawValue
        let flags = event.modifierFlags
        if flags.contains(.shift) { raw |= GHOSTTY_MODS_SHIFT.rawValue }
        if flags.contains(.control) { raw |= GHOSTTY_MODS_CTRL.rawValue }
        if flags.contains(.option) { raw |= GHOSTTY_MODS_ALT.rawValue }
        if flags.contains(.command) { raw |= GHOSTTY_MODS_SUPER.rawValue }
        if flags.contains(.capsLock) { raw |= GHOSTTY_MODS_CAPS.rawValue }
        return ghostty_input_mods_e(rawValue: raw)
    }

    private func unshiftedCodepoint(from event: NSEvent) -> UInt32 {
        guard let scalar = (event.charactersIgnoringModifiers ?? event.characters)?.unicodeScalars.first,
              !(0xF700...0xF8FF).contains(scalar.value) else {
            return 0
        }
        return scalar.value
    }
}

extension EmbeddedGhosttyView: NSMenuItemValidation {
    /// Copy needs a selection; Paste needs text to paste.
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(copy(_:)):
            surfaceOwner.surface.map(ghostty_surface_has_selection) ?? false
        case #selector(paste(_:)):
            NSPasteboard.general.string(forType: .string) != nil
        default:
            true
        }
    }
}

private final class SurfaceContext {
    weak var view: EmbeddedGhosttyView?

    init(view: EmbeddedGhosttyView) {
        self.view = view
    }
}

private extension NSScreen {
    var displayID: UInt32? {
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        if let value = deviceDescription[key] as? UInt32 { return value }
        if let value = deviceDescription[key] as? Int { return UInt32(value) }
        if let value = deviceDescription[key] as? NSNumber { return value.uint32Value }
        return nil
    }
}
