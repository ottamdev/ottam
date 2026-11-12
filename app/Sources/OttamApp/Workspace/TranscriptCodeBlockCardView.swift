import AppKit
import OttamAccessibility
import OttamMarkdownEngine
import SwiftUI

/// Reader and system state that changes a horizontally scrolling block's
/// measured height without changing its content. Captured on the main actor
/// and carried into every measurement, on or off main, so all of them agree.
struct TranscriptScrollableBlockOptions: Equatable, Hashable, Sendable {
    /// Whether a code block past the fold shows every line.
    var isExpanded = false
    /// Height a legacy (always-visible) horizontal scroller takes from the
    /// body when a code block or table overflows; zero under overlay
    /// scrollers.
    var horizontalScrollerInset: CGFloat = 0
    /// Space above the text that the block draws itself because the block
    /// before it is also quoted: the layout leaves no gap there, and the
    /// quote bars for the first `continuedQuoteDepth` levels run through it.
    /// See `MarkdownBlockSpacing.quoteLeadIn`.
    var quoteLeadIn: CGFloat = 0
    var continuedQuoteDepth: Int = 0

    /// The strip a block of `naturalWidth` needs at `width`: the scroller's
    /// height when it overflows, nothing otherwise.
    func scrollerInset(naturalWidth: CGFloat?, width: CGFloat) -> CGFloat {
        (naturalWidth ?? 0) > width + 1 ? horizontalScrollerInset : 0
    }
}

/// Height and label rules shared by measurement and the card view, so the
/// layout's block height and the rendered chrome can never disagree. Every
/// value is a pure function of the theme (and the code for line counts), which
/// is what keeps a streaming fence's height deterministic.
enum TranscriptCodeBlockChrome {
    /// One code line, pinned by `MarkdownStyler.highlightedCode`.
    static func lineHeight(theme: MarkdownTheme) -> CGFloat {
        let font = theme.code
        return ceil(font.ascender - font.descender + font.leading)
    }

    /// The band above the code carrying the language name and Copy. Every
    /// fence gets one, so the affordance is always in the same place and the
    /// height formula keeps a single shape.
    static func headerHeight(theme: MarkdownTheme) -> CGFloat {
        lineHeight(theme: theme) + 16
    }

    /// The strip below the code holding the expand control, present whenever
    /// the fence is long enough to collapse, in both states, so "Show less"
    /// stays reachable.
    static func footerHeight(theme: MarkdownTheme) -> CGFloat {
        lineHeight(theme: theme) + 12
    }

    static func lineCount(of code: String) -> Int {
        code.reduce(into: 1) { count, character in
            if character == "\n" { count += 1 }
        }
    }

    static func isTruncatable(_ code: String, theme: MarkdownTheme) -> Bool {
        lineCount(of: code) > theme.metrics.codeBlockCollapsedLineLimit
    }

    /// The leading `codeBlockCollapsedLineLimit` lines. Scans only as far as
    /// the cut, so a collapsed fence costs O(limit) per streamed chunk.
    static func collapsedCode(_ code: String, theme: MarkdownTheme) -> String {
        let limit = theme.metrics.codeBlockCollapsedLineLimit
        let characters = code as NSString
        var newlines = 0
        for index in 0..<characters.length where characters.character(at: index) == 0x0A {
            newlines += 1
            if newlines == limit { return characters.substring(to: index) }
        }
        return code
    }

    static func font(theme: MarkdownTheme) -> NSFont {
        theme.codeChrome
    }

    /// How the fence's info string is spelled in the header, or nil when the
    /// fence carries none. Distinct from the raw hint, which selection copy
    /// re-emits exactly as written.
    static func displayLanguage(for hint: String?) -> String? {
        guard let hint else { return nil }
        let key = hint.trimmingCharacters(in: .whitespaces).lowercased()
        guard !key.isEmpty else { return nil }
        if let known = displayLanguageNames[key] { return known }
        return key.prefix(1).uppercased() + key.dropFirst()
    }

    /// Only spellings whose display form is not just a capitalization;
    /// anything absent falls through to capitalized, which is right for Swift,
    /// Ruby, Kotlin and friends.
    private static let displayLanguageNames: [String: String] = [
        "js": "JavaScript", "jsx": "JavaScript", "javascript": "JavaScript",
        "mjs": "JavaScript", "cjs": "JavaScript",
        "ts": "TypeScript", "tsx": "TypeScript", "typescript": "TypeScript",
        "py": "Python", "rb": "Ruby", "rs": "Rust", "kt": "Kotlin",
        "sh": "Shell", "bash": "Shell", "zsh": "Shell", "shell": "Shell",
        "console": "Shell", "terminal": "Shell",
        "yml": "YAML", "yaml": "YAML", "json": "JSON", "jsonc": "JSON",
        "html": "HTML", "css": "CSS", "scss": "SCSS", "sql": "SQL",
        "xml": "XML", "toml": "TOML", "ini": "INI", "csv": "CSV",
        "md": "Markdown", "objc": "Objective-C", "objective-c": "Objective-C",
        "cpp": "C++", "c++": "C++", "cc": "C++", "cs": "C#", "csharp": "C#",
        "c": "C", "php": "PHP", "graphql": "GraphQL", "gql": "GraphQL",
        "plain": "Code", "plaintext": "Code", "text": "Code", "txt": "Code",
    ]
}

/// The tile around a fenced code block or a table: background, border, the
/// header band with language (code only) and Copy, and the horizontally
/// scrolling body the collection item hands in. Owns no content and no
/// measurement; the item supplies the theme, hint and natural width on every
/// configure.
@MainActor
final class TranscriptCodeBlockCardView: NSView {
    /// Which block the tile wraps. Code wears the header band with language
    /// and Copy and may fold; a table is the bare tile around its grid
    /// (selection copies it as a pipe table).
    enum Kind {
        case code
        case table
    }

    private var kind: Kind = .code

    /// The band above the body: the code header, nothing for a table.
    private func headerHeight(theme: MarkdownTheme) -> CGFloat {
        kind == .table ? 0 : TranscriptCodeBlockChrome.headerHeight(theme: theme)
    }

    private let headerLabel = NSTextField(labelWithString: "")
    /// Copy is the turn trailer's SwiftUI button, hosted. AppKit has no
    /// symbol-effect API on `NSButton`, and driving `NSImageView`'s replace
    /// transition by hand scaled `checkmark` about a point below its ink, so
    /// the glyph visibly rose on every swap. Hosting the same view is the
    /// same animation by construction.
    private let copyModel = TranscriptCodeBlockCopyModel()
    private let copyHost: NSHostingView<TranscriptCodeBlockCopyButton>
    private let expandButton = PointingHandButton()
    private let overflowFade = OverflowFadeView()
    private weak var body: NSView?
    private var clipObserver: NSObjectProtocol?
    private var theme: MarkdownTheme?
    private var naturalWidth: CGFloat?
    private var showsFooter = false
    private var copiedReset: DispatchWorkItem?

    var debugShowsOverflowCue: Bool { !overflowFade.isHidden }

    var onCopy: (() -> Void)?
    var onToggleExpand: (() -> Void)?

    var debugLanguageLabel: String? { headerLabel.isHidden ? nil : headerLabel.stringValue }
    var debugExpandTitle: String? { expandButton.isHidden ? nil : expandButton.title }
    var debugShowsCopied: Bool { copiedReset != nil }
    func debugPressCopy() { copyPressed() }
    func debugPressExpand() { expandPressed() }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }

    init() {
        copyHost = NSHostingView(rootView: TranscriptCodeBlockCopyButton(model: copyModel))
        super.init(frame: .zero)
        headerLabel.lineBreakMode = .byTruncatingTail
        headerLabel.isSelectable = false
        addSubview(headerLabel)

        copyModel.onPress = { [weak self] in self?.copyPressed() }
        copyHost.sizingOptions = []
        addSubview(copyHost)

        expandButton.isBordered = false
        expandButton.bezelStyle = .regularSquare
        expandButton.controlSize = .small
        expandButton.contentTintColor = .controlAccentColor
        expandButton.target = self
        expandButton.action = #selector(expandPressed)
        expandButton.isHidden = true
        expandButton.setAccessibilityIdentifier(AccessibilityID.chatCodeBlockExpand)
        addSubview(expandButton)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    /// The scrolling body sits under the header. The card positions it; the
    /// item still owns it so scroll offset and reuse stay in one place.
    func install(body: NSView) {
        detachBody()
        self.body = body
        body.autoresizingMask = []
        addSubview(body)
        addSubview(overflowFade, positioned: .above, relativeTo: body)
        if let scrollView = body as? NSScrollView {
            scrollView.contentView.postsBoundsChangedNotifications = true
            clipObserver = NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification,
                object: scrollView.contentView,
                queue: .main
            ) { [weak self] _ in MainActor.assumeIsolated { self?.updateOverflowCue() } }
        }
        needsLayout = true
    }

    private func detachBody() {
        if let clipObserver { NotificationCenter.default.removeObserver(clipObserver) }
        clipObserver = nil
        body?.removeFromSuperview()
        overflowFade.removeFromSuperview()
        body = nil
    }

    /// Under overlay scrollers nothing hints that a line runs past the tile
    /// until the reader drags; the trailing fade is that hint. A legacy
    /// scroller is its own cue, so the fade stays hidden there.
    private func updateOverflowCue() {
        guard let scrollView = body as? NSScrollView,
              let document = scrollView.documentView,
              scrollView.scrollerStyle == .overlay
        else {
            overflowFade.isHidden = true
            return
        }
        let clip = scrollView.contentView.bounds
        overflowFade.isHidden = document.frame.maxX <= clip.maxX + 1
    }

    func apply(
        kind: Kind = .code,
        theme: MarkdownTheme,
        languageHint: String?,
        naturalWidth: CGFloat?,
        lineCount: Int,
        isExpanded: Bool
    ) {
        self.theme = theme
        self.naturalWidth = naturalWidth
        self.kind = kind
        copyHost.isHidden = kind == .table
        let language = kind == .code ? TranscriptCodeBlockChrome.displayLanguage(for: languageHint) : nil
        headerLabel.isHidden = language == nil
        headerLabel.stringValue = language ?? ""
        headerLabel.font = TranscriptCodeBlockChrome.font(theme: theme)
        headerLabel.textColor = theme.secondaryTextColor
        // Published: only touch it on a real change so a same-theme configure
        // does not re-evaluate the hosted button.
        let tint = Color(nsColor: theme.secondaryTextColor)
        if copyModel.tint != tint { copyModel.tint = tint }
        overflowFade.color = theme.codeBackground

        showsFooter = lineCount > theme.metrics.codeBlockCollapsedLineLimit
        expandButton.isHidden = !showsFooter
        if showsFooter {
            expandButton.font = TranscriptCodeBlockChrome.font(theme: theme)
            expandButton.title = isExpanded ? "Show less" : "Show all \(lineCount) lines"
            expandButton.setAccessibilityLabel(expandButton.title)
            expandButton.sizeToFit()
        }
        needsLayout = true
        needsDisplay = true
    }

    func setAccessibilityPresented(_ presented: Bool) {
        headerLabel.setAccessibilityElement(presented && !headerLabel.isHidden)
        copyHost.setAccessibilityElement(presented && !copyHost.isHidden)
        expandButton.setAccessibilityElement(presented && showsFooter)
    }

    func resetForReuse() {
        copiedReset?.cancel()
        copiedReset = nil
        // Silent reset: a recycled tile must not replay a stale transition.
        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) { copyModel.showsCopied = false }
        detachBody()
        naturalWidth = nil
    }

    deinit {
        if let clipObserver { NotificationCenter.default.removeObserver(clipObserver) }
    }

    override func layout() {
        super.layout()
        guard let theme else { return }
        let inset = theme.metrics.codeBlockInset
        let header = headerHeight(theme: theme)
        let buttonSize: CGFloat = 22
        let buttonX = bounds.width - buttonSize - 6
        copyHost.frame = NSRect(
            x: buttonX,
            y: (header - buttonSize) / 2,
            width: buttonSize,
            height: buttonSize
        )
        let labelHeight = headerLabel.fittingSize.height
        headerLabel.frame = NSRect(
            x: inset,
            y: (header - labelHeight) / 2,
            width: max(0, buttonX - inset - 6),
            height: labelHeight
        )
        let footer = showsFooter ? TranscriptCodeBlockChrome.footerHeight(theme: theme) : 0
        if showsFooter {
            let size = expandButton.fittingSize
            expandButton.frame = NSRect(
                x: inset - 2,
                y: bounds.height - footer + (footer - size.height) / 2,
                width: size.width,
                height: size.height
            )
        }
        guard let body else { return }
        let bodyFrame = NSRect(
            x: 0,
            y: header,
            width: bounds.width,
            height: max(0, bounds.height - header - footer)
        )
        body.frame = bodyFrame
        if let scrollView = body as? NSScrollView, let document = scrollView.documentView {
            // The legacy scroller reserves its strip from the clip view; the
            // document fills what is left so the last line never hides
            // behind the bar. Tile first: a scroller-style change with an
            // unchanged frame leaves the clip bounds stale until then.
            scrollView.tile()
            document.frame = NSRect(
                x: 0,
                y: 0,
                width: max(bodyFrame.width, naturalWidth ?? bodyFrame.width),
                height: scrollView.contentView.bounds.height
            )
        }
        let fadeWidth: CGFloat = 28
        overflowFade.frame = NSRect(
            x: bodyFrame.maxX - fadeWidth - 1,
            y: bodyFrame.minY,
            width: fadeWidth,
            height: bodyFrame.height
        )
        updateOverflowCue()
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let theme else { return }
        theme.codeBackground.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 8, yRadius: 8).fill()
        theme.codeBorder.setStroke()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 0.5, dy: 0.5), xRadius: 8, yRadius: 8).stroke()
        theme.codeBorder.setFill()
        let header = headerHeight(theme: theme)
        if header > 0 {
            // Full-bleed rule between the header band and the body.
            NSRect(x: 0, y: header - 1, width: bounds.width, height: 1).fill()
        }
        if showsFooter {
            let footer = TranscriptCodeBlockChrome.footerHeight(theme: theme)
            NSRect(x: 0, y: bounds.height - footer, width: bounds.width, height: 1).fill()
        }
    }

    @objc private func expandPressed() {
        onToggleExpand?()
    }

    private func copyPressed() {
        // A table's tile has no Copy; nothing to press.
        guard !copyHost.isHidden else { return }
        onCopy?()
        copyModel.showsCopied = true
        copiedReset?.cancel()
        // Same glyph pair and dwell as the turn trailer's Copy, so the two
        // read as one gesture.
        let reset = DispatchWorkItem { [weak self] in
            guard let self else { return }
            copiedReset = nil
            copyModel.showsCopied = false
        }
        copiedReset = reset
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5, execute: reset)
    }
}

@MainActor
private final class TranscriptCodeBlockCopyModel: ObservableObject {
    @Published var showsCopied = false
    @Published var tint: Color = .secondary
    var onPress: () -> Void = {}
}

/// The turn trailer's Copy button, verbatim: same glyphs, size, transition
/// and pace, so the two read as one control.
private struct TranscriptCodeBlockCopyButton: View {
    @ObservedObject var model: TranscriptCodeBlockCopyModel

    private var label: String { model.showsCopied ? "Copied" : "Copy code" }

    var body: some View {
        Button(action: model.onPress) {
            Image(systemName: model.showsCopied ? "checkmark" : "square.on.square")
                .font(.system(size: 12))
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 16, height: 16)
                .animation(.easeInOut(duration: 0.2), value: model.showsCopied)
        }
        .buttonStyle(.borderless)
        .foregroundStyle(model.tint)
        .pointerStyle(.link)
        .help(label)
        .accessibilityLabel(label)
        .accessibilityIdentifier(AccessibilityID.chatCodeBlockCopy)
        .frame(width: 22, height: 22)
    }
}

/// A borderless button whose hover cursor is the pointing hand, matching the
/// transcript's other copy affordances.
private final class PointingHandButton: NSButton {
    override func resetCursorRects() {
        super.resetCursorRects()
        addCursorRect(bounds, cursor: .pointingHand)
    }
}

/// A transparent-to-opaque strip over the trailing edge of an overflowing
/// body. Draws only; clicks and selection drags pass through to the code.
private final class OverflowFadeView: NSView {
    var color: NSColor = .clear {
        didSet { needsDisplay = true }
    }

    override var isOpaque: Bool { false }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        guard let gradient = NSGradient(starting: color.withAlphaComponent(0), ending: color)
        else { return }
        gradient.draw(in: bounds, angle: 0)
    }
}
