import AppKit
import CoreText
import OttamMarkdownEngine
import QuartzCore

struct TranscriptTextMeasurement {
    struct Geometry {
        let textWidth: CGFloat
        let horizontalInset: CGFloat
        let verticalInset: CGFloat
    }

    /// Between a quote and the reply under it in one bubble.
    static let bubbleParagraphSpacing: CGFloat = 6

    static func height(
        for text: TranscriptBlock.Text,
        width: CGFloat,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil,
        options: TranscriptScrollableBlockOptions = .init()
    ) -> CGFloat {
        if case .thematicBreak? = text.node?.kind { return 13 }
        let body = bodyHeight(
            for: bodyText(for: text, theme: theme, isExpanded: options.isExpanded),
            width: width,
            theme: theme,
            resourceRoot: resourceRoot
        )
        // Only a legacy scroller makes the natural width matter for height,
        // so skip the second layout pass otherwise.
        let natural = options.horizontalScrollerInset > 0 && text.isHorizontallyScrollable
            ? naturalWidth(for: text, theme: theme, resourceRoot: resourceRoot)
            : nil
        return blockHeight(
            bodyHeight: body,
            text: text,
            width: width,
            naturalWidth: natural,
            theme: theme,
            options: options
        )
    }

    /// The text whose Core Text layout sets the block's body height: the block
    /// itself, or a collapsed code block's leading lines. Measuring the real
    /// leading lines (rather than a formula) keeps the fold exact by
    /// construction.
    static func bodyText(
        for text: TranscriptBlock.Text,
        theme: MarkdownTheme,
        isExpanded: Bool
    ) -> TranscriptBlock.Text {
        guard case .codeBlock? = text.node?.kind,
              !isExpanded,
              TranscriptCodeBlockChrome.isTruncatable(text.source, theme: theme)
        else { return text }
        return TranscriptBlock.Text(
            source: TranscriptCodeBlockChrome.collapsedCode(text.source, theme: theme),
            syntax: text.syntax,
            container: text.container,
            node: text.node
        )
    }

    /// Wraps a Core Text body height in the block's chrome: fenced code adds
    /// its header, its footer whenever the fence is long enough to fold, and
    /// the strip a legacy horizontal scroller takes when the code overflows.
    static func blockHeight(
        bodyHeight: CGFloat,
        text: TranscriptBlock.Text,
        width: CGFloat,
        naturalWidth: CGFloat?,
        theme: MarkdownTheme,
        options: TranscriptScrollableBlockOptions
    ) -> CGFloat {
        let scroller = text.isHorizontallyScrollable
            ? options.scrollerInset(naturalWidth: naturalWidth, width: width)
            : 0
        switch text.node?.kind {
        case .codeBlock?:
            let footer = TranscriptCodeBlockChrome.isTruncatable(text.source, theme: theme)
                ? TranscriptCodeBlockChrome.footerHeight(theme: theme)
                : 0
            return TranscriptCodeBlockChrome.headerHeight(theme: theme) + bodyHeight + scroller + footer
        case .table?:
            return TranscriptTableChrome.headerHeight(theme: theme) + bodyHeight + scroller
        default:
            return options.quoteLeadIn + bodyHeight + scroller
        }
    }

    static func bodyHeight(
        for text: TranscriptBlock.Text,
        width: CGFloat,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil
    ) -> CGFloat {
        // A table's height is its rows by construction; the framesetter
        // would drop the last row's trailing paragraph spacing.
        if let layout = tableLayout(for: text, theme: theme) {
            return CGFloat(layout.rowCount) * TranscriptTableChrome.rowHeight(theme: theme)
        }
        let attributed = attributedString(for: text, theme: theme, resourceRoot: resourceRoot)
        let metrics = geometry(
            for: text,
            width: width,
            theme: theme,
            attributed: attributed
        )
        let layoutAttributed = CoreTextImageAttachmentLayout.applying(
            to: attributed,
            containerWidth: metrics.textWidth
        )
        let framesetter = CTFramesetterCreateWithAttributedString(layoutAttributed)
        let constraintWidth = text.isHorizontallyScrollable ? 100_000 : metrics.textWidth
        let constraint = CGSize(width: constraintWidth, height: .greatestFiniteMagnitude)
        let size = CTFramesetterSuggestFrameSizeWithConstraints(
            framesetter, CFRange(location: 0, length: layoutAttributed.length), nil, constraint, nil
        )
        return max(theme.body.pointSize + metrics.verticalInset * 2, ceil(size.height) + metrics.verticalInset * 2)
    }

    static func naturalWidth(
        for text: TranscriptBlock.Text,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil
    ) -> CGFloat {
        // A table's width is its grid, trailing padding included; the
        // framesetter would stop at the last glyph.
        if let layout = tableLayout(for: text, theme: theme) { return layout.naturalWidth }
        let attributed = attributedString(for: text, theme: theme, resourceRoot: resourceRoot)
        let layoutAttributed = CoreTextImageAttachmentLayout.applying(
            to: attributed,
            containerWidth: 100_000
        )
        let framesetter = CTFramesetterCreateWithAttributedString(layoutAttributed)
        let size = CTFramesetterSuggestFrameSizeWithConstraints(
            framesetter,
            CFRange(location: 0, length: layoutAttributed.length),
            nil,
            CGSize(width: 100_000, height: CGFloat.greatestFiniteMagnitude),
            nil
        )
        return ceil(size.width) + theme.metrics.codeBlockInset * 2
    }

    /// The measured column grid of a table block, stretched to `width` when
    /// narrower; nil for every other kind.
    static func tableLayout(
        for text: TranscriptBlock.Text,
        theme: MarkdownTheme,
        width: CGFloat = 0
    ) -> TranscriptTableLayout? {
        guard case .table(let header, let rows, let alignments)? = text.node?.kind else { return nil }
        return TranscriptTableLayout.measure(
            header: header, rows: rows, alignments: alignments, theme: theme, fillWidth: width
        )
    }

    static func attributedString(
        for text: TranscriptBlock.Text,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil,
        tableLayout: TranscriptTableLayout? = nil,
        now: Date = Date()
    ) -> NSAttributedString {
        if let node = text.node {
            switch node.kind {
            case .codeBlock(let languageHint, _):
                // Shared tokenizer + pinned line height; the item owns geometry.
                return MarkdownStyler(theme: theme).highlightedCode(
                    text.source,
                    hint: languageHint
                )
            case .table:
                // Cells on measured tab stops; the artifact passes the layout
                // it already measured so the grid is built once. Links wear
                // the same appearance as in prose.
                let layout = tableLayout ?? Self.tableLayout(for: text, theme: theme)
                return applyingNativeLinkAppearance(
                    to: InlineCodeFileLinks.applying(
                        to: layout?.attributedString(theme: theme) ?? NSAttributedString(),
                        resourceRoot: resourceRoot,
                        now: now
                    )
                )
            case .thematicBreak:
                return NSAttributedString()
            default:
                return applyingInlineCodePillPadding(
                    to: applyingNativeLinkAppearance(
                        to: InlineCodeFileLinks.applying(
                            to: MarkdownStyler(
                                theme: theme,
                                resourceRoot: resourceRoot
                            ).attributedString(from: [node]),
                            resourceRoot: resourceRoot,
                            now: now
                        )
                    ),
                    theme: theme
                )
            }
        }
        if !text.paragraphs.isEmpty {
            let styled = NSMutableAttributedString(
                attributedString: applyingInlineCodePillPadding(
                    to: applyingNativeLinkAppearance(
                        to: InlineCodeFileLinks.applying(
                            to: MarkdownStyler(theme: theme, resourceRoot: resourceRoot)
                                .attributedString(from: text.paragraphs),
                            resourceRoot: resourceRoot,
                            now: now
                        )
                    ),
                    theme: theme
                )
            )
            // The layout spaces separate blocks; inside one tile the
            // paragraphs space themselves.
            styled.enumerateAttribute(
                .paragraphStyle,
                in: NSRange(location: 0, length: styled.length)
            ) { value, range, _ in
                let style = ((value as? NSParagraphStyle)?.mutableCopy() as? NSMutableParagraphStyle)
                    ?? NSMutableParagraphStyle()
                style.paragraphSpacing = bubbleParagraphSpacing
                styled.addAttribute(.paragraphStyle, value: style, range: range)
            }
            return styled
        }
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = theme.metrics.lineSpacing
        return NSAttributedString(
            string: text.source,
            attributes: [
                .font: theme.body,
                .foregroundColor: theme.textColor,
                .paragraphStyle: paragraph,
            ]
        )
    }

    private static func applyingNativeLinkAppearance(
        to value: NSAttributedString
    ) -> NSAttributedString {
        var ranges: [NSRange] = []
        value.enumerateAttribute(
            .link,
            in: NSRange(location: 0, length: value.length),
            options: [.longestEffectiveRangeNotRequired]
        ) { link, range, _ in
            if link != nil { ranges.append(range) }
        }
        guard !ranges.isEmpty else { return value }
        let result = NSMutableAttributedString(attributedString: value)
        for range in ranges {
            // Color alone marks a link, as in Notes and Mail; an underline
            // through every reference breaks the prose's texture.
            result.addAttribute(.foregroundColor, value: NSColor.linkColor, range: range)
        }
        return result
    }

    /// Code spans lose the styler's flat run background — the item draws a
    /// rounded pill for them instead — and gain their horizontal padding as
    /// kerning: on the glyph before the span and on the span's last glyph, so
    /// the pill has room without touching the copied text.
    static func applyingInlineCodePillPadding(
        to value: NSAttributedString,
        theme: MarkdownTheme
    ) -> NSAttributedString {
        let ranges = inlineCodeRanges(in: value)
        guard !ranges.isEmpty else { return value }
        let padding = theme.metrics.inlineCodePadding
        let result = NSMutableAttributedString(attributedString: value)
        // Added to any kerning already there — a list marker padded out to
        // its column keeps that padding when code opens the item's text.
        func pad(at location: Int) {
            let existing = result.attribute(.kern, at: location, effectiveRange: nil) as? CGFloat ?? 0
            result.addAttribute(.kern, value: existing + padding, range: NSRange(location: location, length: 1))
        }
        for range in ranges {
            result.removeAttribute(.backgroundColor, range: range)
            pad(at: range.upperBound - 1)
            if range.location > 0, !ranges.contains(where: { $0.upperBound == range.location }) {
                pad(at: range.location - 1)
            }
        }
        return result
    }

    static func inlineCodeRanges(in value: NSAttributedString) -> [NSRange] {
        var ranges: [NSRange] = []
        value.enumerateAttribute(
            MarkdownAttribute.inlineRole,
            in: NSRange(location: 0, length: value.length)
        ) { role, range, _ in
            if MarkdownInlineRole(attributeValue: role) == .code { ranges.append(range) }
        }
        return ranges
    }

    static func geometry(
        for text: TranscriptBlock.Text,
        width: CGFloat,
        theme: MarkdownTheme,
        attributed: NSAttributedString? = nil
    ) -> Geometry {
        if text.container == .bubble {
            let inset = theme.metrics.bubbleInset
            let maximumTextWidth = max(
                1,
                min(theme.metrics.bubbleMaxWidth, width) - inset * 2
            )
            let fittedTextWidth = attributed.map {
                fittedBubbleTextWidth($0, maximum: maximumTextWidth)
            } ?? maximumTextWidth
            return Geometry(
                textWidth: fittedTextWidth,
                horizontalInset: inset,
                verticalInset: theme.metrics.bubbleVerticalInset
            )
        }
        if let node = text.node {
            switch node.kind {
            case .codeBlock:
                return Geometry(
                    textWidth: max(1, width - theme.metrics.codeBlockInset * 2),
                    horizontalInset: theme.metrics.codeBlockInset,
                    verticalInset: theme.metrics.codeBlockVerticalInset
                )
            case .table:
                // The grid's own cell padding is the inset; row rules run to
                // the tile's edges. Vertically, the rows' shared inset
                // centers each line in its row (see `TranscriptTableLayout`).
                return Geometry(
                    textWidth: max(1, width),
                    horizontalInset: 0,
                    verticalInset: attributed.map(TranscriptTableLayout.rowInset(in:)) ?? 0
                )
            default:
                if node.quoteDepth > 0 {
                    let inset = theme.metrics.quoteInset * CGFloat(node.quoteDepth)
                    return Geometry(
                        textWidth: max(1, width - inset),
                        horizontalInset: inset,
                        verticalInset: 0
                    )
                }
            }
        }
        return Geometry(textWidth: max(1, width), horizontalInset: 0, verticalInset: 0)
    }

    /// Lays the bubble out once at its maximum width, then hugs the widest
    /// rendered line. The one-point slack prevents the fitted second layout
    /// from changing a line break because of Core Text rounding.
    private static func fittedBubbleTextWidth(
        _ attributed: NSAttributedString,
        maximum: CGFloat
    ) -> CGFloat {
        guard attributed.length > 0 else { return 1 }
        let framesetter = CTFramesetterCreateWithAttributedString(attributed)
        let suggested = CTFramesetterSuggestFrameSizeWithConstraints(
            framesetter,
            CFRange(location: 0, length: attributed.length),
            nil,
            CGSize(width: maximum, height: .greatestFiniteMagnitude),
            nil
        )
        let path = CGPath(
            rect: CGRect(
                x: 0,
                y: 0,
                width: maximum,
                height: max(1, ceil(suggested.height) + 1)
            ),
            transform: nil
        )
        let frame = CTFramesetterCreateFrame(
            framesetter,
            CFRange(location: 0, length: attributed.length),
            path,
            nil
        )
        let lines = CTFrameGetLines(frame) as NSArray
        let string = attributed.string as NSString
        var widest: CGFloat = 0
        for element in lines {
            let line = element as! CTLine
            let typographic = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
            let trailingWhitespace = CGFloat(CTLineGetTrailingWhitespaceWidth(line))
            // A quoted line's indent counts toward its width. Read it from the
            // paragraph style, not the line origin: a right-aligned line's
            // origin is its flush offset, which would fill the maximum.
            let start = CTLineGetStringRange(line).location
            let style = attributed.attribute(.paragraphStyle, at: start, effectiveRange: nil) as? NSParagraphStyle
            let opensParagraph = string.paragraphRange(for: NSRange(location: start, length: 0)).location == start
            let indent = (opensParagraph ? style?.firstLineHeadIndent : style?.headIndent) ?? 0
            widest = max(widest, indent + typographic - trailingWhitespace)
        }
        return min(maximum, max(1, ceil(widest) + 1))
    }
}

/// `NSTextAttachment.attachmentBounds` participates in TextKit layout, but this
/// renderer lays the same attributed string out directly with Core Text. A run
/// delegate is Core Text's native way to reserve the attachment footprint.
private enum CoreTextImageAttachmentLayout {
    private final class Metrics {
        let size: CGSize

        init(size: CGSize) { self.size = size }
    }

    private static let runDelegateKey = NSAttributedString.Key(
        kCTRunDelegateAttributeName as String
    )

    static func applying(
        to value: NSAttributedString,
        containerWidth: CGFloat
    ) -> NSAttributedString {
        guard value.length > 0 else { return value }
        var attachments: [(NSRange, MarkdownImageAttachment)] = []
        value.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: value.length),
            options: [.longestEffectiveRangeNotRequired]
        ) { attachment, range, _ in
            if let attachment = attachment as? MarkdownImageAttachment {
                attachments.append((range, attachment))
            }
        }
        guard !attachments.isEmpty else { return value }

        let result = NSMutableAttributedString(attributedString: value)
        for (range, attachment) in attachments {
            let size = attachment.displaySize(containerWidth: containerWidth)
            let metrics = Unmanaged.passRetained(Metrics(size: size))
            var callbacks = CTRunDelegateCallbacks(
                version: kCTRunDelegateCurrentVersion,
                dealloc: { pointer in
                    Unmanaged<Metrics>.fromOpaque(pointer).release()
                },
                getAscent: { pointer in
                    return Unmanaged<Metrics>.fromOpaque(pointer).takeUnretainedValue().size.height
                },
                getDescent: { _ in 0 },
                getWidth: { pointer in
                    return Unmanaged<Metrics>.fromOpaque(pointer).takeUnretainedValue().size.width
                }
            )
            guard let delegate = CTRunDelegateCreate(&callbacks, metrics.toOpaque()) else {
                metrics.release()
                continue
            }
            result.addAttribute(runDelegateKey, value: delegate, range: range)
        }
        return result
    }
}

/// One prepared text representation shared by exact measurement and a visible
/// collection item. Attachment payloads may change on main; refreshing their
/// geometry preserves the styled string and invalidates only Core Text values.
final class TranscriptTextRenderArtifact {
    let blockID: TranscriptBlockID
    let text: TranscriptBlock.Text
    let width: CGFloat
    let theme: MarkdownTheme
    let resourceRoot: URL?
    /// Reader/system state that changes a code block's height. Part of the
    /// artifact's identity so a change rebuilds it through the cache.
    let options: TranscriptScrollableBlockOptions
    let attributed: NSAttributedString
    /// Which code spans named markdown files when `attributed` was built;
    /// the artifact is rebuilt once one appears or goes away.
    private let fileLinks: InlineCodeFileLinks.Answers
    /// The clock that decides when a cached file answer has expired. Injected
    /// so a test can cross `InlineCodeFileLinks.recheckInterval` without
    /// sleeping; production uses the wall clock.
    private let now: () -> Date
    /// The measured grid when the block is a table: the item draws its
    /// hairlines from it and the card sizes the document to it.
    let tableLayout: TranscriptTableLayout?
    private(set) var geometry: TranscriptTextMeasurement.Geometry
    private(set) var layoutAttributed: NSAttributedString
    private(set) var measuredHeight: CGFloat
    private(set) var naturalWidth: CGFloat?
    private(set) var attachmentRevision: UInt64 = 0
    let containsAttachments: Bool
    private var attachmentSizeSignature: [ObjectIdentifier: CGSize] = [:]

    init(
        blockID: TranscriptBlockID,
        text: TranscriptBlock.Text,
        width: CGFloat,
        theme: MarkdownTheme,
        resourceRoot: URL?,
        options: TranscriptScrollableBlockOptions = .init(),
        now: @escaping () -> Date = Date.init
    ) {
        self.blockID = blockID
        self.text = text
        self.width = width
        self.theme = theme
        self.resourceRoot = resourceRoot
        self.options = options
        self.now = now
        tableLayout = TranscriptTextMeasurement.tableLayout(for: text, theme: theme, width: width)
        // One reading for the whole build: styling and the answers it keeps
        // must stamp the shared file cache with the same clock.
        let builtAt = now()
        attributed = TranscriptTextMeasurement.attributedString(
            for: text,
            theme: theme,
            resourceRoot: resourceRoot,
            tableLayout: tableLayout,
            now: builtAt
        )
        fileLinks = InlineCodeFileLinks.answers(
            in: attributed,
            resourceRoot: resourceRoot,
            now: builtAt
        )
        geometry = TranscriptTextMeasurement.geometry(
            for: text,
            width: width,
            theme: theme,
            attributed: attributed
        )
        layoutAttributed = CoreTextImageAttachmentLayout.applying(
            to: attributed,
            containerWidth: geometry.textWidth
        )
        containsAttachments = Self.hasAttachments(in: attributed)
        attachmentSizeSignature = Self.attachmentSizes(in: attributed)
        measuredHeight = 1
        naturalWidth = nil
        recomputeGeometry()
    }

    func matches(
        blockID: TranscriptBlockID,
        text: TranscriptBlock.Text,
        width: CGFloat,
        theme: MarkdownTheme,
        resourceRoot: URL?,
        options: TranscriptScrollableBlockOptions = .init()
    ) -> Bool {
        self.blockID == blockID
            && self.text == text
            && self.width == width
            && self.theme == theme
            && self.resourceRoot == resourceRoot
            && self.options == options
            && fileLinks.areCurrent(in: resourceRoot, now: now())
    }

    func attachmentGeometryDidChange() {
        guard containsAttachments else { return }
        attachmentSizeSignature = Self.attachmentSizes(in: attributed)
        attachmentRevision &+= 1
        recomputeGeometry()
    }

    @discardableResult
    func refreshAttachmentGeometryIfNeeded() -> Bool {
        guard containsAttachments else { return false }
        let signature = Self.attachmentSizes(in: attributed)
        guard signature != attachmentSizeSignature else { return false }
        attachmentSizeSignature = signature
        attachmentRevision &+= 1
        recomputeGeometry()
        return true
    }

    private func recomputeGeometry() {
        geometry = TranscriptTextMeasurement.geometry(
            for: text,
            width: width,
            theme: theme,
            attributed: attributed
        )
        layoutAttributed = CoreTextImageAttachmentLayout.applying(
            to: attributed,
            containerWidth: geometry.textWidth
        )
        if let tableLayout {
            naturalWidth = tableLayout.naturalWidth
        } else if text.isHorizontallyScrollable {
            let framesetter = CTFramesetterCreateWithAttributedString(layoutAttributed)
            let size = CTFramesetterSuggestFrameSizeWithConstraints(
                framesetter,
                CFRange(location: 0, length: layoutAttributed.length),
                nil,
                CGSize(width: 100_000, height: CGFloat.greatestFiniteMagnitude),
                nil
            )
            naturalWidth = ceil(size.width) + theme.metrics.codeBlockInset * 2
        } else {
            naturalWidth = nil
        }
        if case .thematicBreak? = text.node?.kind {
            measuredHeight = 13
        } else {
            let bodyText = TranscriptTextMeasurement.bodyText(
                for: text, theme: theme, isExpanded: options.isExpanded
            )
            let body: CGFloat
            if bodyText == text {
                let framesetter = CTFramesetterCreateWithAttributedString(layoutAttributed)
                let constraintWidth = text.isHorizontallyScrollable ? 100_000 : geometry.textWidth
                let size = CTFramesetterSuggestFrameSizeWithConstraints(
                    framesetter,
                    CFRange(location: 0, length: layoutAttributed.length),
                    nil,
                    CGSize(width: constraintWidth, height: .greatestFiniteMagnitude),
                    nil
                )
                body = max(
                    theme.body.pointSize + geometry.verticalInset * 2,
                    ceil(size.height) + geometry.verticalInset * 2
                )
            } else {
                // Collapsed: the drawing text stays complete, only the
                // measured body shrinks to the leading lines.
                body = TranscriptTextMeasurement.bodyHeight(
                    for: bodyText, width: width, theme: theme, resourceRoot: resourceRoot
                )
            }
            measuredHeight = TranscriptTextMeasurement.blockHeight(
                bodyHeight: body,
                text: text,
                width: width,
                naturalWidth: naturalWidth,
                theme: theme,
                options: options
            )
        }
    }

    private static func hasAttachments(in value: NSAttributedString) -> Bool {
        var result = false
        value.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: value.length),
            options: [.longestEffectiveRangeNotRequired]
        ) { attachment, _, stop in
            if attachment != nil {
                result = true
                stop.pointee = true
            }
        }
        return result
    }

    private static func attachmentSizes(
        in value: NSAttributedString
    ) -> [ObjectIdentifier: CGSize] {
        var result: [ObjectIdentifier: CGSize] = [:]
        value.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: value.length),
            options: [.longestEffectiveRangeNotRequired]
        ) { value, _, _ in
            guard let attachment = value as? MarkdownImageAttachment,
                  let size = attachment.imageSize
            else { return }
            result[ObjectIdentifier(attachment)] = size
        }
        return result
    }
}

@MainActor
final class TranscriptTextArtifactCache {
    static let maximumCount = 256
    private var artifacts: [TranscriptBlockID: TranscriptTextRenderArtifact] = [:]
    private var recency: [TranscriptBlockID: UInt64] = [:]
    private var accessCounter: UInt64 = 0
    private(set) var buildCount = 0
    var count: Int { artifacts.count }

    /// Decides when a cached file answer has expired. Defaults to the wall
    /// clock; a test injects a manual clock to cross the recheck interval
    /// without sleeping.
    private let now: () -> Date

    init(now: @escaping () -> Date = Date.init) {
        self.now = now
    }

    func artifact(
        for block: TranscriptBlock,
        width: CGFloat,
        theme: MarkdownTheme,
        resourceRoot: URL?,
        options: TranscriptScrollableBlockOptions = .init()
    ) -> TranscriptTextRenderArtifact? {
        guard case .text(let text) = block.content else { return nil }
        if let cached = artifacts[block.id], cached.matches(
            blockID: block.id,
            text: text,
            width: width,
            theme: theme,
            resourceRoot: resourceRoot,
            options: options
        ) {
            touch(block.id)
            return cached
        }
        let artifact = TranscriptTextRenderArtifact(
            blockID: block.id,
            text: text,
            width: width,
            theme: theme,
            resourceRoot: resourceRoot,
            options: options,
            now: now
        )
        artifacts[block.id] = artifact
        touch(block.id)
        trim()
        buildCount += 1
        return artifact
    }

    func remove<S: Sequence>(_ ids: S) where S.Element == TranscriptBlockID {
        for id in ids {
            artifacts.removeValue(forKey: id)
            recency.removeValue(forKey: id)
        }
    }

    func retain(_ ids: Set<TranscriptBlockID>) {
        artifacts = artifacts.filter { ids.contains($0.key) }
        recency = recency.filter { ids.contains($0.key) }
    }

    func removeAll() {
        artifacts.removeAll(keepingCapacity: true)
        recency.removeAll(keepingCapacity: true)
    }

    private func touch(_ id: TranscriptBlockID) {
        accessCounter &+= 1
        recency[id] = accessCounter
    }

    /// Keeps the `count` most recently used artifacts, `maximumCount` when nil.
    func trim(toMostRecent count: Int? = nil) {
        let count = count ?? Self.maximumCount
        while artifacts.count > count,
              let oldest = recency.min(by: { $0.value < $1.value })?.key {
            artifacts.removeValue(forKey: oldest)
            recency.removeValue(forKey: oldest)
        }
    }
}

enum TranscriptLinkTarget {
    /// A link's destination: a file must stay inside `resourceRoot`, and a
    /// relative path starts in its folder (see `MarkdownImageAttachment.resolve`).
    static func resolve(_ value: Any, relativeTo resourceRoot: URL?) -> URL? {
        let url: URL?
        switch value {
        case let value as URL:
            url = value
        case let value as NSURL:
            url = value as URL
        case let value as String:
            url = URL(string: value)
        default:
            url = nil
        }
        guard let url = url.map(withoutLineSuffix) else { return nil }
        guard url.scheme == nil else {
            if url.isFileURL {
                guard let resourceRoot,
                      MarkdownImageAttachment.isContained(url, in: resourceRoot)
                else { return nil }
            }
            return url
        }
        let resolved = MarkdownImageAttachment.resolve(url, relativeTo: resourceRoot)
        return resolved.isFileURL ? resolved : nil
    }

    /// `docs/foo.md:12` keeps the line in its path, and `foo.md:12` parses as
    /// scheme `foo.md`; a dotted scheme followed by only a line (and column)
    /// is a path with a line suffix.
    private static func withoutLineSuffix(_ url: URL) -> URL {
        if url.scheme == nil {
            let path = FileReference.path(url.path)
            guard path != url.path, !path.isEmpty,
                  let encoded = path.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed)
            else { return url }
            return URL(string: encoded) ?? url
        }
        guard let scheme = url.scheme, scheme.contains("."),
              FileReference.isLineSuffix(":" + url.absoluteString.dropFirst(scheme.count + 1)),
              let path = URL(string: scheme)
        else { return url }
        return path
    }
}

/// A path as agents write it, which may end in a line: `a.md:12`,
/// `a.md:12:4`, `a.md#L12`, or `a.md#L12-L20`.
enum FileReference {
    private static let suffix = #/(?::\d+(?::\d+)?|#L\d+(?:-L?\d+)?)$/#

    static func isLineSuffix(_ text: some StringProtocol) -> Bool {
        String(text).wholeMatch(of: suffix) != nil
    }

    /// `text` without a trailing line suffix.
    static func path(_ text: String) -> String {
        guard let match = text.firstMatch(of: suffix) else { return text }
        return String(text[..<match.range.lowerBound])
    }
}

private final class TranscriptLinkAccessibilityElement: NSAccessibilityElement {
    var press: (() -> Void)?

    override func accessibilityPerformPress() -> Bool {
        guard let press else { return false }
        press()
        return true
    }
}

/// Core Text-backed selectable transcript text. It owns no canonical content;
/// collection-item reuse replaces the immutable source and drawing cache.
@MainActor
final class TranscriptTextItemView: NSView {
    private struct SemanticAccessibilityChild {
        let range: NSRange
        let element: NSAccessibilityElement
    }

    var itemID: String?
    var copyMessage: (() -> Void)?
    var canCopyMessage: () -> Bool = { true }
    var hasTranscriptSelection: (() -> Bool)?
    var onSelectionBegan: ((Int, NSRange?) -> Void)?
    var onSelectionDragged: ((NSEvent) -> Void)?
    var onSelectionEnded: ((NSEvent) -> Void)?
    var onSelectionCancelled: (() -> Void)?
    var onAttachmentLayoutChanged: (() -> Void)?
    var openLink: (URL) -> Void = { NSWorkspace.shared.open($0) }

    private var blockID: TranscriptBlockID?
    private var artifact: TranscriptTextRenderArtifact?
    private var text: TranscriptBlock.Text?
    private var theme: MarkdownTheme?
    private var attributed = NSAttributedString()
    private var selectedRange = NSRange(location: 0, length: 0)
    private var isDraggingSelection = false
    private var mouseDownIndex = 0
    /// Where the press began, in window coordinates, until the pointer moves
    /// past `selectionDragThreshold`; nil once the press is a drag.
    private var selectionDragOrigin: NSPoint?
    /// A click often moves a point or two. Below this it stays a click, so
    /// a press forwarded from empty space never extends toward whatever row
    /// lies under that jitter. AppKit exposes no public drag threshold.
    private static let selectionDragThreshold: CGFloat = 3
    private var fadeBatches: [LiveBatchAnimationCoordinator.Batch] = []
    private var fadeOverlays: [UInt64: TranscriptGlyphOverlayView] = [:]
    private(set) var debugFadeSurfaceRefreshCount = 0
    private var geometryCache: (width: CGFloat, value: TranscriptTextMeasurement.Geometry)?
    private struct CoreTextLayout {
        let lines: [(line: CTLine, origin: CGPoint)]
        let hasAttachments: Bool
    }
    private var attributedLayoutCache: (size: CGSize, layout: CoreTextLayout)?
    private var semanticAccessibilityChildren: [SemanticAccessibilityChild] = []
    private let imageLoader = MarkdownImageLoader()

    var hasSelection: Bool { selectedRange.length > 0 || isDraggingSelection }
    var debugSelectedRange: NSRange { selectedRange }
    var debugThemeSignature: String? { theme?.signature }
    var debugLoadedImageCount: Int {
        var count = 0
        attributed.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: attributed.length)
        ) { value, _, _ in
            if (value as? MarkdownImageAttachment)?.imageSize != nil { count += 1 }
        }
        return count
    }
    var debugArtifactIdentity: ObjectIdentifier? { artifact.map(ObjectIdentifier.init) }
    var debugFadeOverlayIdentities: Set<ObjectIdentifier> {
        Set(fadeOverlays.values.map(ObjectIdentifier.init))
    }

    func debugBaseDrawingAttributes(at index: Int) -> [NSAttributedString.Key: Any] {
        baseAttributedTextForDrawing().attributes(at: index, effectiveRange: nil)
    }

    func setSelectedRange(_ range: NSRange, forceAccessibility: Bool = false) {
        let previous = selectedRange
        let location = min(max(0, range.location), attributed.length)
        let next = NSRange(
            location: location,
            length: min(max(0, range.length), attributed.length - location)
        )
        guard forceAccessibility || previous != next else { return }
        selectedRange = next
        setAccessibilitySelectedTextRange(selectedRange)
        setAccessibilitySelectedText(
            selectedRange.length > 0
                ? attributed.attributedSubstring(from: selectedRange).string
                : nil
        )
        if previous != selectedRange, window != nil {
            NSAccessibility.post(element: self, notification: .selectedTextChanged)
        }
        needsDisplay = true
    }

    func debugSelect(range: NSRange) { setSelectedRange(range) }

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override var isOpaque: Bool { false }

    func configure(
        blockID: TranscriptBlockID,
        text: TranscriptBlock.Text,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil,
        fadeBatches: [LiveBatchAnimationCoordinator.Batch] = [],
        selectedRange: NSRange = NSRange(location: 0, length: 0)
    ) {
        let artifact = TranscriptTextRenderArtifact(
            blockID: blockID,
            text: text,
            width: max(1, bounds.width),
            theme: theme,
            resourceRoot: resourceRoot
        )
        configure(
            artifact: artifact,
            fadeBatches: fadeBatches,
            selectedRange: selectedRange
        )
    }

    func configure(
        artifact: TranscriptTextRenderArtifact,
        fadeBatches: [LiveBatchAnimationCoordinator.Batch] = [],
        selectedRange: NSRange = NSRange(location: 0, length: 0)
    ) {
        let attachmentGeometryChanged = artifact.refreshAttachmentGeometryIfNeeded()
        if self.artifact === artifact {
            setSelectedRange(selectedRange)
            self.fadeBatches = fadeBatches.filter {
                $0.range.location >= 0 && NSMaxRange($0.range) <= attributed.length
            }
            reconcileFadeOverlays()
            if attachmentGeometryChanged { onAttachmentLayoutChanged?() }
            return
        }
        self.blockID = artifact.blockID
        self.artifact = artifact
        text = artifact.text
        theme = artifact.theme
        attributed = artifact.attributed
        isDraggingSelection = false
        setSelectedRange(selectedRange, forceAccessibility: true)
        self.fadeBatches = fadeBatches.filter {
            $0.range.location >= 0 && NSMaxRange($0.range) <= attributed.length
        }
        geometryCache = nil
        attributedLayoutCache = nil
        reconcileFadeOverlays()
        refreshRetainedFadeSurfaces()
        setAccessibilityRole(.staticText)
        setAccessibilityValue(accessibilityTextValue())
        setAccessibilityVisibleCharacterRange(NSRange(location: 0, length: attributed.length))
        setAccessibilityNumberOfCharacters(attributed.length)
        rebuildSemanticAccessibilityChildren()
        kickImageLoads(for: artifact.blockID)
        if attachmentGeometryChanged { onAttachmentLayoutChanged?() }
        needsDisplay = true
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        if isDraggingSelection { onSelectionCancelled?() }
        itemID = nil
        copyMessage = nil
        hasTranscriptSelection = nil
        onSelectionBegan = nil
        onSelectionDragged = nil
        onSelectionEnded = nil
        onSelectionCancelled = nil
        onAttachmentLayoutChanged = nil
        openLink = { NSWorkspace.shared.open($0) }
        blockID = nil
        artifact = nil
        text = nil
        theme = nil
        attributed = NSAttributedString()
        selectedRange = NSRange(location: 0, length: 0)
        isDraggingSelection = false
        setAccessibilitySelectedTextRange(selectedRange)
        setAccessibilitySelectedText(nil)
        setAccessibilityValue(nil)
        setAccessibilityVisibleCharacterRange(selectedRange)
        setAccessibilityNumberOfCharacters(0)
        semanticAccessibilityChildren.removeAll(keepingCapacity: true)
        fadeBatches.removeAll()
        geometryCache = nil
        attributedLayoutCache = nil
        fadeOverlays.values.forEach { $0.removeFromSuperview() }
        fadeOverlays.removeAll(keepingCapacity: true)
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let text, let theme else { return }
        let geometry = geometry(forWidth: bounds.width, text: text, theme: theme)
        let bubbleWidth = geometry.textWidth + geometry.horizontalInset * 2
        let originX = text.container == .bubble ? bounds.maxX - bubbleWidth : bounds.minX
        if text.container == .bubble {
            theme.bubbleBackground.setFill()
            NSBezierPath(
                roundedRect: NSRect(x: originX, y: 0, width: bubbleWidth, height: bounds.height),
                xRadius: theme.metrics.bubbleCornerRadius,
                yRadius: theme.metrics.bubbleCornerRadius
            ).fill()
        }

        if let node = text.node {
            switch node.kind {
            case .codeBlock:
                // The collection item's card draws the tile so the border
                // stays put while the body scrolls horizontally.
                break
            case .table:
                // The card draws the tile; the grid's own lines scroll with
                // the cells, as a grid should.
                if let layout = artifact?.tableLayout {
                    drawTableGrid(layout, theme: theme)
                }
            case .thematicBreak:
                theme.ruleColor.setFill()
                NSRect(x: 0, y: floor(bounds.midY), width: bounds.width, height: 1).fill()
                return
            default:
                if node.quoteDepth > 0 {
                    // Levels the block above also has run through the
                    // lead-in, so one bar spans both; deeper levels start
                    // with this block's own text.
                    theme.quoteBar.setFill()
                    let options = artifact?.options ?? .init()
                    for depth in 0..<node.quoteDepth {
                        let top = depth < options.continuedQuoteDepth ? 0 : options.quoteLeadIn
                        NSRect(
                            x: CGFloat(depth) * theme.metrics.quoteInset,
                            y: top,
                            width: 3,
                            height: bounds.height - top
                        ).fill()
                    }
                }
            }
        }

        let textRect = NSRect(
            x: originX + geometry.horizontalInset,
            y: geometry.verticalInset + quoteLeadIn,
            width: geometry.textWidth,
            height: max(1, bounds.height - geometry.verticalInset * 2 - quoteLeadIn)
        )
        if !text.paragraphs.isEmpty {
            drawBubbleQuoteBars(in: textRect, theme: theme)
        }
        drawSelection(in: textRect)
        drawAttributedText(
            baseAttributedTextForDrawing(),
            in: textRect,
            dirtyRect: dirtyRect,
            suppressingPillsIn: fadeBatches.map(\.range)
        )
    }

    /// A bubble's quotes are paragraphs of its one string, so their bars run
    /// beside the quoted lines inside the tile rather than at the column edge.
    private func drawBubbleQuoteBars(in textRect: NSRect, theme: MarkdownTheme) {
        theme.quoteBar.setFill()
        attributed.enumerateAttribute(
            MarkdownAttribute.quoteDepth,
            in: NSRange(location: 0, length: attributed.length)
        ) { value, range, _ in
            guard let depth = (value as? NSNumber)?.intValue, depth > 0 else { return }
            let rects = rangeRects(range, in: textRect)
            guard let top = rects.map(\.minY).min(), let bottom = rects.map(\.maxY).max() else { return }
            for level in 0..<depth {
                NSRect(
                    x: textRect.minX + CGFloat(level) * theme.metrics.quoteInset,
                    y: top,
                    width: 3,
                    height: bottom - top
                ).fill()
            }
        }
    }

    /// Header band, row separators and column rules, from the same measured
    /// columns and pinned row height the text was laid out on.
    private func drawTableGrid(_ layout: TranscriptTableLayout, theme: MarkdownTheme) {
        let rowHeight = TranscriptTableChrome.rowHeight(theme: theme)
        let width = max(bounds.width, layout.width)
        theme.codeBorder.setFill()
        for row in 1..<max(1, layout.rowCount) {
            NSRect(x: 0, y: CGFloat(row) * rowHeight - 1, width: width, height: 1).fill()
        }
    }

    /// Active fade overlays own the complete visual presentation of their
    /// ranges. Suppressing the inline-code background here keeps its pill and
    /// glyphs on the same opacity curve instead of revealing the pill first.
    private func baseAttributedTextForDrawing() -> NSAttributedString {
        guard !fadeBatches.isEmpty else { return attributed }
        let base = NSMutableAttributedString(attributedString: attributed)
        for batch in fadeBatches {
            base.addAttributes(
                [.foregroundColor: NSColor.clear, .backgroundColor: NSColor.clear],
                range: batch.range
            )
        }
        return base
    }

    // MARK: Hover cursor

    private var hoverTrackingArea: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverTrackingArea { removeTrackingArea(hoverTrackingArea) }
        let area = NSTrackingArea(
            rect: .zero,
            options: [.mouseMoved, .cursorUpdate, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        hoverTrackingArea = area
    }

    override func cursorUpdate(with event: NSEvent) {
        applyHoverCursor(for: event)
    }

    override func mouseMoved(with event: NSEvent) {
        applyHoverCursor(for: event)
    }

    /// Tracking areas report by position, even under a view drawn on top
    /// (the quote reply capsule), and that view owns the cursor there.
    private func applyHoverCursor(for event: NSEvent) {
        if !isDraggingSelection,
           let hit = window?.contentView?.hitTest(event.locationInWindow),
           hit !== self, !hit.isDescendant(of: self) {
            NSCursor.arrow.set()
            return
        }
        applyHoverCursor(at: convert(event.locationInWindow, from: nil))
    }

    /// AppKit delivers `mouseDragged`, not `mouseMoved`, while a button is
    /// down, so a drag that starts on a link would otherwise keep the hand
    /// until the next plain move. Selection always wants the I-beam.
    ///
    /// A drag keeps reporting to the view it started in even after the
    /// pointer crosses into a neighbouring block, and the selection follows
    /// it there, so the rect check must not run while dragging.
    fileprivate func applyHoverCursor(at point: NSPoint) {
        if isDraggingSelection {
            NSCursor.iBeam.set()
            return
        }
        guard attributed.length > 0, let rect = textRect(), rect.contains(point) else {
            NSCursor.arrow.set()
            return
        }
        (isOverLink(at: point) ? NSCursor.pointingHand : NSCursor.iBeam).set()
    }

    /// After a drag ends the pointer may sit over a different block. That
    /// block owns the cursor, not the one the drag started in.
    private func applyHoverCursorAfterRelease(with event: NSEvent) {
        let local = convert(event.locationInWindow, from: nil)
        if bounds.contains(local) {
            applyHoverCursor(at: local)
        } else if let other = window?.contentView?.hitTest(event.locationInWindow) as? TranscriptTextItemView {
            other.applyHoverCursor(at: other.convert(event.locationInWindow, from: nil))
        } else {
            NSCursor.arrow.set()
        }
    }

    /// True only when the point sits on a link's glyphs. `characterIndex(at:)`
    /// snaps to the nearest character, so hovering past a line's end would
    /// otherwise read as "over the last link on that line".
    private func isOverLink(at point: NSPoint) -> Bool {
        let index = characterIndex(at: point)
        guard index < attributed.length,
              attributed.attribute(.link, at: index, effectiveRange: nil) != nil,
              let rect = textRect(),
              let layout = lineLayout(in: rect) else { return false }
        let corePoint = CGPoint(x: point.x - rect.minX, y: rect.maxY - point.y)
        for entry in layout {
            let range = CTLineGetStringRange(entry.line)
            guard index >= range.location, index < range.location + range.length else { continue }
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            let width = CTLineGetTypographicBounds(entry.line, &ascent, &descent, nil)
            let insideX = corePoint.x >= entry.origin.x && corePoint.x <= entry.origin.x + width
            let insideY = corePoint.y >= entry.origin.y - descent && corePoint.y <= entry.origin.y + ascent
            return insideX && insideY
        }
        return false
    }

    /// Space above the text a quoted block absorbs from the layout so its
    /// bar can run through it; see `TranscriptScrollableBlockOptions`.
    private var quoteLeadIn: CGFloat { artifact?.options.quoteLeadIn ?? 0 }

    private func textRect() -> NSRect? {
        guard let text, let theme else { return nil }
        let geometry = geometry(forWidth: bounds.width, text: text, theme: theme)
        let bubbleWidth = geometry.textWidth + geometry.horizontalInset * 2
        let originX = text.container == .bubble ? bounds.maxX - bubbleWidth : bounds.minX
        return NSRect(
            x: originX + geometry.horizontalInset,
            y: geometry.verticalInset + quoteLeadIn,
            width: geometry.textWidth,
            height: max(1, bounds.height - geometry.verticalInset * 2 - quoteLeadIn)
        )
    }

    override func mouseDown(with event: NSEvent) {
        guard attributed.length > 0 else { return }
        finalizeFades()
        let index = characterIndex(at: convert(event.locationInWindow, from: nil))
        mouseDownIndex = index
        let initialRange: NSRange?
        if event.clickCount == 2 {
            initialRange = wordRange(at: index)
        } else if event.clickCount >= 3 {
            initialRange = paragraphRange(at: index)
        } else {
            initialRange = nil
        }
        onSelectionBegan?(index, initialRange)
        isDraggingSelection = event.clickCount == 1
        selectionDragOrigin = event.locationInWindow
    }

    override func mouseDragged(with event: NSEvent) {
        guard isDraggingSelection else { return }
        if let origin = selectionDragOrigin {
            let location = event.locationInWindow
            guard hypot(location.x - origin.x, location.y - origin.y) >= Self.selectionDragThreshold else { return }
            selectionDragOrigin = nil
        }
        applyHoverCursor(at: convert(event.locationInWindow, from: nil))
        onSelectionDragged?(event)
    }

    override func mouseUp(with event: NSEvent) {
        onSelectionEnded?(event)
        isDraggingSelection = false
        applyHoverCursorAfterRelease(with: event)
        // The press must land on the link's glyphs: one forwarded from a gap
        // or gutter snaps to the nearest character, which may be a link.
        if event.clickCount == 1, selectedRange.length == 0,
           mouseDownIndex < attributed.length,
           isOverLink(at: convert(event.locationInWindow, from: nil)),
           let target = attributed.attribute(.link, at: mouseDownIndex, effectiveRange: nil) {
            if let url = TranscriptLinkTarget.resolve(
                target,
                relativeTo: artifact?.resourceRoot
            ) {
                openLink(url)
            }
        }
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let menu = NSMenu()
        if hasTranscriptSelection?() == true {
            let copy = NSMenuItem(title: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "")
            copy.target = nil
            menu.addItem(copy)
            // Nil target: the transcript's collection view answers, as it does
            // for Copy, and disables it where its host takes no replies. Its
            // shortcut is handled there too.
            let reply = NSMenuItem(
                title: "Reply…",
                action: #selector(TranscriptQuoteReplying.replyToSelection(_:)),
                keyEquivalent: "r"
            )
            reply.keyEquivalentModifierMask = [.command, .shift]
            reply.target = nil
            menu.addItem(reply)
        }
        if copyMessage != nil, canCopyMessage() {
            let message = NSMenuItem(title: "Copy Message", action: #selector(copyWholeMessage), keyEquivalent: "")
            message.target = self
            menu.addItem(message)
        }
        return menu.items.isEmpty ? nil : menu
    }

    @objc private func copyWholeMessage() { if canCopyMessage() { copyMessage?() } }

    override func accessibilityChildren() -> [Any]? {
        guard let textRect = currentTextRect else { return [] }
        for child in semanticAccessibilityChildren {
            let rectangles = rangeRects(child.range, in: textRect)
            child.element.setAccessibilityFrameInParentSpace(
                rectangles.reduce(NSRect.null) { $0.union($1) }
            )
        }
        return semanticAccessibilityChildren.map(\.element)
    }

    private func rebuildSemanticAccessibilityChildren() {
        semanticAccessibilityChildren.removeAll(keepingCapacity: true)
        guard attributed.length > 0 else { return }
        attributed.enumerateAttribute(
            .link,
            in: NSRange(location: 0, length: attributed.length)
        ) { [weak self] value, range, _ in
            guard let self,
                  let value,
                  let url = TranscriptLinkTarget.resolve(value, relativeTo: artifact?.resourceRoot)
            else { return }
            let element = TranscriptLinkAccessibilityElement()
            element.setAccessibilityRole(.link)
            element.setAccessibilityParent(self)
            element.setAccessibilityLabel(attributed.attributedSubstring(from: range).string)
            element.setAccessibilityURL(url)
            element.press = { [weak self] in self?.openLink(url) }
            semanticAccessibilityChildren.append(.init(range: range, element: element))
        }
        attributed.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: attributed.length)
        ) { [weak self] value, range, _ in
            guard let self, let image = value as? MarkdownImageAttachment else { return }
            let element = NSAccessibilityElement()
            element.setAccessibilityRole(.image)
            element.setAccessibilityParent(self)
            element.setAccessibilityLabel(image.altText.isEmpty ? "Image" : image.altText)
            semanticAccessibilityChildren.append(.init(range: range, element: element))
        }
        semanticAccessibilityChildren.sort { $0.range.location < $1.range.location }
    }

    private func accessibilityTextValue() -> String {
        let value = NSMutableAttributedString(attributedString: attributed)
        var attachmentRanges: [NSRange] = []
        attributed.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: attributed.length)
        ) { attachment, range, _ in
            if attachment != nil { attachmentRanges.append(range) }
        }
        for range in attachmentRanges.reversed() {
            value.replaceCharacters(
                in: range,
                with: String(repeating: " ", count: range.length)
            )
        }
        return value.string
    }

    fileprivate func drawFadeBatch(_ range: NSRange, dirtyRect: NSRect) {
        guard let rect = currentTextRect else { return }
        let rectangles = rangeRects(range, in: rect)
        guard !rectangles.isEmpty else { return }
        let affectedRect = rectangles.reduce(NSRect.null) { $0.union($1) }
            .intersection(dirtyRect)
        guard !affectedRect.isEmpty else { return }
        NSGraphicsContext.saveGraphicsState()
        let path = NSBezierPath()
        for rectangle in rectangles { path.appendRect(rectangle) }
        path.addClip()
        drawAttributedText(attributed, in: rect, dirtyRect: affectedRect)
        NSGraphicsContext.restoreGraphicsState()
    }

    private func drawAttributedText(
        _ value: NSAttributedString,
        in rect: NSRect,
        dirtyRect: NSRect,
        suppressingPillsIn suppressedRanges: [NSRange] = []
    ) {
        guard value.length > 0,
              let context = NSGraphicsContext.current?.cgContext,
              let layout = coreTextLayout(for: value, in: rect)
        else { return }
        if let theme {
            drawInlineCodePills(
                layout: layout.lines,
                textRect: rect,
                dirtyRect: dirtyRect,
                theme: theme,
                suppressing: suppressedRanges
            )
        }
        context.saveGState()
        context.textMatrix = .identity
        context.translateBy(x: rect.minX, y: rect.maxY)
        context.scaleBy(x: 1, y: -1)
        for entry in layout.lines where lineRect(entry, in: rect).intersects(dirtyRect) {
            context.textPosition = entry.origin
            CTLineDraw(entry.line, context)
        }
        context.restoreGState()
        if layout.hasAttachments {
            drawAttachments(layout: layout.lines, textRect: rect, dirtyRect: dirtyRect)
        }
    }

    /// One rounded pill per code span per line, behind the glyphs. The span's
    /// trailing padding is already in its last glyph's advance (kerning), the
    /// leading padding in the glyph before it, so the pill runs from the span's
    /// origin minus the padding to its end. A span that Core Text split into
    /// several runs (font fallback) gets one pill; a span that wraps gets one
    /// per line. Ranges mid-fade are skipped: the fade overlay draws them.
    func inlineCodePillRects(
        layout: [(line: CTLine, origin: CGPoint)],
        textRect rect: NSRect,
        theme: MarkdownTheme,
        suppressing suppressedRanges: [NSRange] = []
    ) -> [NSRect] {
        let padding = theme.metrics.inlineCodePadding
        let verticalPadding = theme.metrics.inlineCodeVerticalPadding
        var rects: [NSRect] = []
        for (line, origin) in layout {
            let baselineY = rect.maxY - origin.y
            var current: (minX: CGFloat, maxX: CGFloat, ascent: CGFloat, descent: CGFloat, end: CFIndex)?
            func flush() {
                guard let pill = current else { return }
                // The leading padding rides on the glyph before the span; a
                // span that opens a line has none, so the pill stops at the
                // text edge rather than drawing outside the item and being
                // clipped to a square corner.
                let minX = max(rect.minX, rect.minX + origin.x + pill.minX - padding)
                let maxX = min(rect.maxX, rect.minX + origin.x + pill.maxX)
                rects.append(NSRect(
                    x: minX,
                    y: baselineY - pill.ascent - verticalPadding,
                    width: maxX - minX,
                    height: pill.ascent + pill.descent + verticalPadding * 2
                ))
                current = nil
            }
            let runs = CTLineGetGlyphRuns(line) as NSArray
            for case let run as CTRun in runs {
                let attributes = CTRunGetAttributes(run) as NSDictionary
                let range = CTRunGetStringRange(run)
                guard MarkdownInlineRole(attributeValue: attributes[MarkdownAttribute.inlineRole]) == .code,
                      !suppressedRanges.contains(where: {
                          NSIntersectionRange($0, NSRange(location: range.location, length: range.length)).length > 0
                      })
                else {
                    flush()
                    continue
                }
                var ascent: CGFloat = 0
                var descent: CGFloat = 0
                CTRunGetTypographicBounds(run, CFRange(location: 0, length: 0), &ascent, &descent, nil)
                // Glyph origins and advances, not caret offsets: Core Text
                // reports a kerned boundary's caret in the middle of the kern,
                // which would shave the trailing pad and fatten the leading one.
                let glyphCount = CTRunGetGlyphCount(run)
                guard glyphCount > 0 else { continue }
                var positions = [CGPoint](repeating: .zero, count: glyphCount)
                var advances = [CGSize](repeating: .zero, count: glyphCount)
                CTRunGetPositions(run, CFRange(location: 0, length: 0), &positions)
                CTRunGetAdvances(run, CFRange(location: 0, length: 0), &advances)
                let start = positions[0].x
                let end = positions[glyphCount - 1].x + advances[glyphCount - 1].width
                if let pill = current, pill.end == range.location {
                    current = (pill.minX, max(pill.maxX, end), max(pill.ascent, ascent), max(pill.descent, descent), range.location + range.length)
                } else {
                    flush()
                    current = (start, end, ascent, descent, range.location + range.length)
                }
            }
            flush()
        }
        return rects
    }

    private func drawInlineCodePills(
        layout: [(line: CTLine, origin: CGPoint)],
        textRect rect: NSRect,
        dirtyRect: NSRect,
        theme: MarkdownTheme,
        suppressing suppressedRanges: [NSRange]
    ) {
        guard !TranscriptTextMeasurement.inlineCodeRanges(in: attributed).isEmpty else { return }
        let radius = theme.metrics.inlineCodeCornerRadius
        for pill in inlineCodePillRects(layout: layout, textRect: rect, theme: theme, suppressing: suppressedRanges)
        where pill.intersects(dirtyRect) {
            theme.inlineCodeBackground.setFill()
            NSBezierPath(roundedRect: pill, xRadius: radius, yRadius: radius).fill()
            theme.codeBorder.setStroke()
            NSBezierPath(roundedRect: pill.insetBy(dx: 0.5, dy: 0.5), xRadius: radius, yRadius: radius).stroke()
        }
    }

    /// Test hook: the pills the base draw would paint right now, in view
    /// coordinates, honouring active fades.
    func debugInlineCodePillRects() -> [NSRect] {
        guard let theme, let rect = currentTextRect,
              let layout = coreTextLayout(for: attributed, in: rect)
        else { return [] }
        return inlineCodePillRects(
            layout: layout.lines,
            textRect: rect,
            theme: theme,
            suppressing: fadeBatches.map(\.range)
        )
    }

    private func drawAttachments(
        layout: [(line: CTLine, origin: CGPoint)],
        textRect rect: NSRect,
        dirtyRect: NSRect
    ) {
        for (line, origin) in layout {
            guard lineRect((line, origin), in: rect).intersects(dirtyRect) else { continue }
            let runs = CTLineGetGlyphRuns(line) as NSArray
            for case let run as CTRun in runs {
                let attributes = CTRunGetAttributes(run) as NSDictionary
                guard let attachment = attributes[NSAttributedString.Key.attachment]
                    as? MarkdownImageAttachment,
                      let image = attachment.image
                else { continue }
                let range = CTRunGetStringRange(run)
                let x = CTLineGetOffsetForStringIndex(line, range.location, nil)
                let size = attachment.displaySize(containerWidth: rect.width)
                let baselineY = rect.maxY - origin.y
                image.draw(in: NSRect(
                    x: rect.minX + origin.x + x,
                    y: baselineY - size.height,
                    width: size.width,
                    height: size.height
                ))
            }
        }
    }

    private func drawSelection(in rect: NSRect) {
        guard selectedRange.length > 0,
              let layout = lineLayout(in: rect)
        else { return }
        NSColor.selectedTextBackgroundColor.withAlphaComponent(0.55).setFill()
        for (line, origin) in layout {
            let range = CTLineGetStringRange(line)
            let lineRange = NSRange(location: range.location, length: range.length)
            let intersection = NSIntersectionRange(selectedRange, lineRange)
            guard intersection.length > 0 else { continue }
            let start = CTLineGetOffsetForStringIndex(line, intersection.location, nil)
            let end = CTLineGetOffsetForStringIndex(line, NSMaxRange(intersection), nil)
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            CTLineGetTypographicBounds(line, &ascent, &descent, nil)
            let y = rect.maxY - origin.y - ascent
            NSRect(x: rect.minX + origin.x + start, y: y, width: max(1, end - start), height: ascent + descent).fill()
        }
    }

    private func rangeRects(_ selected: NSRange, in rect: NSRect) -> [NSRect] {
        guard selected.length > 0, let layout = lineLayout(in: rect) else { return [] }
        return layout.compactMap { line, origin in
            let range = CTLineGetStringRange(line)
            let intersection = NSIntersectionRange(
                selected,
                NSRange(location: range.location, length: range.length)
            )
            guard intersection.length > 0 else { return nil }
            let start = CTLineGetOffsetForStringIndex(line, intersection.location, nil)
            let end = CTLineGetOffsetForStringIndex(line, NSMaxRange(intersection), nil)
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            CTLineGetTypographicBounds(line, &ascent, &descent, nil)
            return NSRect(
                x: rect.minX + origin.x + start,
                y: rect.maxY - origin.y - ascent,
                width: max(1, end - start),
                height: ascent + descent
            )
        }
    }

    private var currentTextRect: NSRect? { textRect() }

    /// Bounds of the selected glyph lines in this view's coordinates; nil when
    /// nothing in this block is selected.
    var selectionBounds: NSRect? {
        guard selectedRange.length > 0, let rect = currentTextRect else { return nil }
        let rectangles = rangeRects(selectedRange, in: rect)
        guard !rectangles.isEmpty else { return nil }
        return rectangles.reduce(NSRect.null) { $0.union($1) }
    }

    private func reconcileFadeOverlays() {
        let now = CACurrentMediaTime()
        let activeIDs = Set(fadeBatches.map(\.id))
        for id in fadeOverlays.keys.filter({ !activeIDs.contains($0) }) {
            fadeOverlays.removeValue(forKey: id)?.removeFromSuperview()
        }
        for batch in fadeBatches {
            if fadeOverlays[batch.id] != nil { continue }
            let overlay = TranscriptGlyphOverlayView(owner: self, range: batch.range)
            overlay.frame = bounds
            overlay.autoresizingMask = [.width, .height]
            overlay.alphaValue = batch.opacity(at: now)
            addSubview(overlay)
            fadeOverlays[batch.id] = overlay
            NSAnimationContext.runAnimationGroup { context in
                context.duration = batch.remaining(at: now)
                context.timingFunction = CAMediaTimingFunction(name: .easeOut)
                overlay.animator().alphaValue = 1
            } completionHandler: { [weak self, weak overlay] in
                MainActor.assumeIsolated {
                    guard let self,
                          let overlay,
                          self.fadeOverlays[batch.id] === overlay
                    else { return }
                    self.fadeBatches.removeAll { $0.id == batch.id }
                    self.fadeOverlays.removeValue(forKey: batch.id)
                    overlay.removeFromSuperview()
                    self.needsDisplay = true
                }
            }
        }
        needsDisplay = true
    }

    /// A retained overlay has compositor-owned opacity, but its backing pixels
    /// must follow the current artifact and its final item geometry. Refreshing
    /// the owner and every active surface in one AppKit display transaction
    /// prevents an old glyph image from being composited over the new layout.
    private func refreshRetainedFadeSurfaces() {
        guard !fadeOverlays.isEmpty else { return }
        debugFadeSurfaceRefreshCount += 1
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        needsDisplay = true
        for overlay in fadeOverlays.values {
            overlay.frame = bounds
            overlay.needsDisplay = true
        }
        if window != nil { displayIfNeeded() }
        CATransaction.commit()
    }

    private func finalizeFades() {
        fadeBatches.removeAll()
        fadeOverlays.values.forEach { $0.removeFromSuperview() }
        fadeOverlays.removeAll(keepingCapacity: true)
        needsDisplay = true
    }

    fileprivate func characterIndex(at point: NSPoint) -> Int {
        guard let rect = textRect(),
              let layout = lineLayout(in: rect), !layout.isEmpty else { return 0 }
        let corePoint = CGPoint(x: point.x - rect.minX, y: rect.maxY - point.y)
        var closest = layout[0]
        var closestDistance = CGFloat.greatestFiniteMagnitude
        for entry in layout {
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            CTLineGetTypographicBounds(entry.line, &ascent, &descent, nil)
            let distance = abs(corePoint.y - entry.origin.y)
            if distance < closestDistance {
                closest = entry
                closestDistance = distance
            }
            if corePoint.y >= entry.origin.y - descent, corePoint.y <= entry.origin.y + ascent {
                closest = entry
                break
            }
        }
        let value = CTLineGetStringIndexForPosition(
            closest.line,
            CGPoint(x: corePoint.x - closest.origin.x, y: 0)
        )
        return max(0, min(attributed.length, value == kCFNotFound ? attributed.length : value))
    }

    private func lineLayout(in rect: NSRect) -> [(line: CTLine, origin: CGPoint)]? {
        lineLayout(for: attributed, in: rect)
    }

    private func lineLayout(
        for value: NSAttributedString,
        in rect: NSRect
    ) -> [(line: CTLine, origin: CGPoint)]? {
        coreTextLayout(for: value, in: rect)?.lines
    }

    private func coreTextLayout(
        for value: NSAttributedString,
        in rect: NSRect
    ) -> CoreTextLayout? {
        guard value.length > 0 else { return nil }
        if value === attributed,
           let attributedLayoutCache,
           attributedLayoutCache.size == rect.size {
            return attributedLayoutCache.layout
        }
        let layoutValue = value === attributed
                && artifact.map({ abs($0.width - bounds.width) < 0.5 }) == true
            ? artifact?.layoutAttributed ?? value
            : CoreTextImageAttachmentLayout.applying(to: value, containerWidth: rect.width)
        // A table's text rect is exactly its pinned row stack, and Core Text
        // drops a line that does not wholly fit the path: a rect a fraction
        // short (a clip height after tiling) loses the last row. Its rows are
        // pinned and the view clips, so lay it out with room below. Origins
        // are relative to the path's bottom, so they shed the slack.
        let slack: CGFloat = if case .table? = text?.node?.kind { rect.height } else { 0 }
        let path = CGPath(
            rect: CGRect(x: 0, y: 0, width: rect.width, height: rect.height + slack),
            transform: nil
        )
        let frame = CTFramesetterCreateFrame(
            CTFramesetterCreateWithAttributedString(layoutValue),
            CFRange(location: 0, length: layoutValue.length), path, nil
        )
        let rawLines = CTFrameGetLines(frame) as NSArray
        var origins = Array(repeating: CGPoint.zero, count: rawLines.count)
        CTFrameGetLineOrigins(frame, CFRange(location: 0, length: 0), &origins)
        let lines = rawLines.enumerated().compactMap { index, value -> (line: CTLine, origin: CGPoint)? in
            guard CFGetTypeID(value as CFTypeRef) == CTLineGetTypeID() else { return nil }
            return (value as! CTLine, CGPoint(x: origins[index].x, y: origins[index].y - slack))
        }
        var hasAttachments = false
        value.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: value.length),
            options: [.longestEffectiveRangeNotRequired]
        ) { attachment, _, stop in
            if attachment != nil {
                hasAttachments = true
                stop.pointee = true
            }
        }
        let layout = CoreTextLayout(lines: lines, hasAttachments: hasAttachments)
        if value === attributed { attributedLayoutCache = (rect.size, layout) }
        return layout
    }

    private func lineRect(
        _ entry: (line: CTLine, origin: CGPoint),
        in rect: NSRect
    ) -> NSRect {
        var ascent: CGFloat = 0
        var descent: CGFloat = 0
        let width = CGFloat(CTLineGetTypographicBounds(entry.line, &ascent, &descent, nil))
        return NSRect(
            x: rect.minX + entry.origin.x,
            y: rect.maxY - entry.origin.y - ascent,
            width: max(1, width),
            height: ascent + descent
        )
    }

    private func kickImageLoads(for configuredBlockID: TranscriptBlockID) {
        guard attributed.length > 0 else { return }
        attributed.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: attributed.length)
        ) { [weak self] value, _, _ in
            guard let self, let attachment = value as? MarkdownImageAttachment else { return }
            imageLoader.loadIfNeeded(attachment) { [weak self, weak attachment] loaded in
                guard let self,
                      let attachment,
                      attachment === loaded,
                      self.blockID == configuredBlockID,
                      self.containsAttachment(attachment)
                else { return }
                self.artifact?.attachmentGeometryDidChange()
                self.attributedLayoutCache = nil
                self.needsDisplay = true
                self.onAttachmentLayoutChanged?()
            }
        }
    }

    private func containsAttachment(_ sought: MarkdownImageAttachment) -> Bool {
        var found = false
        attributed.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: attributed.length),
            options: [.longestEffectiveRangeNotRequired]
        ) { value, _, stop in
            if value as AnyObject === sought {
                found = true
                stop.pointee = true
            }
        }
        return found
    }

    func measuredHeight(forWidth width: CGFloat) -> CGFloat? {
        guard let text, let theme else { return nil }
        if let artifact, abs(artifact.width - width) < 0.5 {
            return artifact.measuredHeight
        }
        if case .thematicBreak? = text.node?.kind { return 13 }
        return TranscriptTextMeasurement.height(
            for: text,
            width: width,
            theme: theme,
            resourceRoot: artifact?.resourceRoot,
            options: artifact?.options ?? .init()
        )
    }

    private func geometry(
        forWidth width: CGFloat,
        text: TranscriptBlock.Text,
        theme: MarkdownTheme
    ) -> TranscriptTextMeasurement.Geometry {
        if let geometryCache, abs(geometryCache.width - width) < 0.5 {
            return geometryCache.value
        }
        if let artifact, abs(artifact.width - width) < 0.5 {
            geometryCache = (width, artifact.geometry)
            return artifact.geometry
        }
        let value = TranscriptTextMeasurement.geometry(
            for: text,
            width: width,
            theme: theme,
            attributed: attributed
        )
        geometryCache = (width, value)
        return value
    }

    private func wordRange(at index: Int) -> NSRange {
        let string = attributed.string as NSString
        guard string.length > 0 else { return NSRange(location: 0, length: 0) }
        let safe = min(max(0, index), string.length - 1)
        let scalars = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "_-"))
        guard let scalar = UnicodeScalar(string.character(at: safe)), scalars.contains(scalar) else {
            return string.rangeOfComposedCharacterSequence(at: safe)
        }
        var lower = safe
        var upper = safe + 1
        while lower > 0,
              let value = UnicodeScalar(string.character(at: lower - 1)),
              scalars.contains(value) { lower -= 1 }
        while upper < string.length,
              let value = UnicodeScalar(string.character(at: upper)),
              scalars.contains(value) { upper += 1 }
        return NSRange(location: lower, length: upper - lower)
    }

    private func paragraphRange(at index: Int) -> NSRange {
        (attributed.string as NSString).paragraphRange(for: NSRange(location: min(index, attributed.length), length: 0))
    }
}

private extension TranscriptBlock.Text {
    var isHorizontallyScrollable: Bool {
        guard let node else { return false }
        switch node.kind {
        case .codeBlock, .table: return true
        default: return false
        }
    }
}

@MainActor
private final class TranscriptGlyphOverlayView: NSView {
    weak var owner: TranscriptTextItemView?
    let range: NSRange

    init(owner: TranscriptTextItemView, range: NSRange) {
        self.owner = owner
        self.range = range
        super.init(frame: .zero)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
        layer?.needsDisplayOnBoundsChange = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    override func draw(_ dirtyRect: NSRect) { owner?.drawFadeBatch(range, dirtyRect: dirtyRect) }
}

struct TranscriptTextPresentation {
    let itemID: String
    let artifact: TranscriptTextRenderArtifact
    let fadeBatches: [LiveBatchAnimationCoordinator.Batch]
    let selectedRange: NSRange
    let isAccessibilityPresented: Bool

    init(
        itemID: String,
        artifact: TranscriptTextRenderArtifact,
        fadeBatches: [LiveBatchAnimationCoordinator.Batch],
        selectedRange: NSRange,
        isAccessibilityPresented: Bool = true
    ) {
        self.itemID = itemID
        self.artifact = artifact
        self.fadeBatches = fadeBatches
        self.selectedRange = selectedRange
        self.isAccessibilityPresented = isAccessibilityPresented
    }
}

@MainActor
protocol TranscriptTextCollectionItemDelegate: AnyObject {
    func transcriptTextItemHasSelection(_ item: TranscriptTextCollectionItem) -> Bool
    func transcriptTextItem(
        _ item: TranscriptTextCollectionItem,
        beganAt offset: Int,
        initialRange: NSRange?,
        blockID: TranscriptBlockID
    )
    func transcriptTextItem(_ item: TranscriptTextCollectionItem, dragged event: NSEvent)
    func transcriptTextItem(_ item: TranscriptTextCollectionItem, ended event: NSEvent)
    func transcriptTextItemCancelledSelection(_ item: TranscriptTextCollectionItem)
    func transcriptTextItem(
        _ item: TranscriptTextCollectionItem,
        attachmentLayoutChangedFor blockID: TranscriptBlockID,
        measuredHeight: CGFloat
    )
    func transcriptTextItem(_ item: TranscriptTextCollectionItem, copyMessage itemID: String)
    func transcriptTextItem(_ item: TranscriptTextCollectionItem, canCopyMessage itemID: String) -> Bool
    func transcriptTextItem(
        _ item: TranscriptTextCollectionItem,
        toggleExpansionFor blockID: TranscriptBlockID
    )
    func transcriptTextItem(_ item: TranscriptTextCollectionItem, openLink url: URL)
}

@MainActor
final class TranscriptTextCollectionItem: NSCollectionViewItem {
    /// How the text view is hosted: directly for prose, inside the bare
    /// horizontal scroll view for any other overflowing kind, and inside the
    /// card (which in turn holds the scroll view) for fenced code and tables.
    private enum HostLayout {
        case plain
        case scrolling
        case codeCard
    }

    private let textItemView = TranscriptTextItemView()
    private let scrollView = TranscriptHorizontalScrollView()
    private let container = NSView()
    /// Built the first time an item hosts a fence: the card carries a hosted
    /// SwiftUI Copy button, and most pooled items only ever show prose.
    private var codeCard: TranscriptCodeBlockCardView?
    private var blockID: TranscriptBlockID?
    private var hostLayout: HostLayout?
    /// What the card's Copy puts on the pasteboard: raw code for a fence,
    /// a pipe table for a table.
    private var copySource: String?
    private var presentation: TranscriptTextPresentation?
    /// Card Copy sink; tests swap in a private pasteboard.
    var copyCode: (String) -> Void = WorkspacePasteboard.copy
    /// Expansion sink for the closure-configured path; the delegate carries
    /// it in production.
    var toggleExpansion: ((TranscriptBlockID) -> Void)?

    var debugCodeLanguageLabel: String? { codeCard?.debugLanguageLabel }
    var debugShowsOverflowCue: Bool { codeCard?.debugShowsOverflowCue ?? false }
    /// Pins the tile's scroller style so a test does not depend on the
    /// runner's system setting.
    func debugSetScrollerStyle(_ style: NSScroller.Style) {
        scrollView.scrollerStyle = style
        codeCard?.needsLayout = true
        codeCard?.layoutSubtreeIfNeeded()
    }
    var debugCodeExpandTitle: String? { codeCard?.debugExpandTitle }
    var debugShowsCopiedState: Bool { codeCard?.debugShowsCopied ?? false }
    func debugPressCopyCode() { codeCard?.debugPressCopy() }
    func debugPressExpandCode() { codeCard?.debugPressExpand() }
    private var delegateCallbacksInstalled = false
    weak var delegate: (any TranscriptTextCollectionItemDelegate)?

    var hasSelection: Bool { textItemView.hasSelection }
    var debugSelectedRange: NSRange { textItemView.debugSelectedRange }
    var debugThemeSignature: String? { textItemView.debugThemeSignature }
    var debugLoadedImageCount: Int { textItemView.debugLoadedImageCount }
    var debugArtifactIdentity: ObjectIdentifier? { textItemView.debugArtifactIdentity }
    var debugFadeOverlayIdentities: Set<ObjectIdentifier> {
        textItemView.debugFadeOverlayIdentities
    }
    var debugFadeSurfaceRefreshCount: Int { textItemView.debugFadeSurfaceRefreshCount }
    var debugTextAutoresizingMask: NSView.AutoresizingMask { textItemView.autoresizingMask }
    var debugHorizontalOffsetX: CGFloat { scrollView.contentView.bounds.minX }
    /// Visible body height inside the horizontal scroll view after tiling.
    var debugScrollClipHeight: CGFloat {
        scrollView.tile()
        return scrollView.contentView.bounds.height
    }
    var debugScrollDocumentHeight: CGFloat { scrollView.documentView?.frame.height ?? 0 }
    func debugInlineCodePillRects() -> [NSRect] { textItemView.debugInlineCodePillRects() }

    func debugSelect(range: NSRange) { textItemView.debugSelect(range: range) }
    func applySelection(_ range: NSRange) { textItemView.setSelectedRange(range) }
    /// The selected lines' bounds in `target`'s coordinates, nil when this
    /// block holds no selection.
    func selectionRect(in target: NSView) -> NSRect? {
        textItemView.selectionBounds.map { textItemView.convert($0, to: target) }
    }
    func setAccessibilityPresented(_ presented: Bool) {
        textItemView.setAccessibilityElement(presented)
        codeCard?.setAccessibilityPresented(presented && hostLayout == .codeCard)
    }
    func characterIndex(at point: NSPoint, from coordinateView: NSView) -> Int {
        textItemView.characterIndex(at: textItemView.convert(point, from: coordinateView))
    }
    /// The view that tracks a selection gesture for this item, including one
    /// the transcript hands it from a press on empty space.
    var selectionTrackingView: NSView { textItemView }
    func debugScrollHorizontally(to x: CGFloat) {
        scrollView.contentView.scroll(to: NSPoint(x: x, y: 0))
        scrollView.reflectScrolledClipView(scrollView.contentView)
    }

    override func loadView() {
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasHorizontalScroller = true
        scrollView.hasVerticalScroller = false
        scrollView.autohidesScrollers = true
        // No explicit scrollerStyle: the tile follows the system setting, and
        // measurement reserves the legacy strip through the block's options.
        scrollView.verticalScrollElasticity = .none
        view = container
        installDelegateCallbacks()
    }

    private func installDelegateCallbacks() {
        guard !delegateCallbacksInstalled else { return }
        delegateCallbacksInstalled = true
        textItemView.hasTranscriptSelection = { [weak self] in
            guard let self else { return false }
            return self.delegate?.transcriptTextItemHasSelection(self) == true
        }
        textItemView.onSelectionBegan = { [weak self] offset, range in
            guard let self, let blockID = self.presentation?.artifact.blockID else { return }
            self.delegate?.transcriptTextItem(
                self,
                beganAt: offset,
                initialRange: range,
                blockID: blockID
            )
        }
        textItemView.onSelectionDragged = { [weak self] event in
            guard let self else { return }
            self.delegate?.transcriptTextItem(self, dragged: event)
        }
        textItemView.onSelectionEnded = { [weak self] event in
            guard let self else { return }
            self.delegate?.transcriptTextItem(self, ended: event)
        }
        textItemView.onSelectionCancelled = { [weak self] in
            guard let self else { return }
            self.delegate?.transcriptTextItemCancelledSelection(self)
        }
        textItemView.onAttachmentLayoutChanged = { [weak self] in
            guard let self,
                  let presentation = self.presentation,
                  let height = self.measuredHeight(forWidth: presentation.artifact.width)
            else { return }
            self.delegate?.transcriptTextItem(
                self,
                attachmentLayoutChangedFor: presentation.artifact.blockID,
                measuredHeight: height
            )
        }
        textItemView.copyMessage = { [weak self] in
            guard let self, let itemID = self.presentation?.itemID else { return }
            self.delegate?.transcriptTextItem(self, copyMessage: itemID)
        }
        textItemView.canCopyMessage = { [weak self] in
            guard let self, let itemID = self.presentation?.itemID else { return false }
            return self.delegate?.transcriptTextItem(self, canCopyMessage: itemID) ?? false
        }
        textItemView.openLink = { [weak self] url in
            guard let self else { return }
            self.delegate?.transcriptTextItem(self, openLink: url)
        }
    }

    override func preferredLayoutAttributesFitting(
        _ layoutAttributes: NSCollectionViewLayoutAttributes
    ) -> NSCollectionViewLayoutAttributes {
        layoutAttributes
    }

    func apply(_ presentation: TranscriptTextPresentation) {
        installDelegateCallbacks()
        let identityChanged = blockID != presentation.artifact.blockID
        host(presentation.artifact, identityChanged: identityChanged)
        self.presentation = presentation
        textItemView.itemID = presentation.itemID
        textItemView.configure(
            artifact: presentation.artifact,
            fadeBatches: presentation.fadeBatches,
            selectedRange: presentation.selectedRange
        )
        setAccessibilityPresented(presentation.isAccessibilityPresented)
    }

    func configure(
        block: TranscriptBlock,
        artifact suppliedArtifact: TranscriptTextRenderArtifact? = nil,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil,
        fadeBatches: [LiveBatchAnimationCoordinator.Batch] = [],
        selectedRange: NSRange = NSRange(location: 0, length: 0),
        hasTranscriptSelection: @escaping () -> Bool = { false },
        onSelectionBegan: @escaping (Int, NSRange?) -> Void = { _, _ in },
        onSelectionDragged: @escaping (NSEvent) -> Void = { _ in },
        onSelectionEnded: @escaping (NSEvent) -> Void = { _ in },
        onAttachmentLayoutChanged: @escaping () -> Void = {},
        copyMessage: @escaping () -> Void
    ) {
        guard case .text(let text) = block.content else { return }
        let identityChanged = blockID != block.id
        textItemView.itemID = block.itemID
        delegateCallbacksInstalled = false
        textItemView.copyMessage = copyMessage
        textItemView.canCopyMessage = { true }
        textItemView.hasTranscriptSelection = hasTranscriptSelection
        textItemView.onSelectionBegan = onSelectionBegan
        textItemView.onSelectionDragged = onSelectionDragged
        textItemView.onSelectionEnded = onSelectionEnded
        textItemView.onAttachmentLayoutChanged = onAttachmentLayoutChanged
        let artifact = suppliedArtifact ?? TranscriptTextRenderArtifact(
            blockID: block.id,
            text: text,
            width: max(1, container.bounds.width),
            theme: theme,
            resourceRoot: resourceRoot
        )
        host(artifact, identityChanged: identityChanged)
        textItemView.configure(
            artifact: artifact,
            fadeBatches: fadeBatches,
            selectedRange: selectedRange
        )
    }

    func measuredHeight(forWidth width: CGFloat) -> CGFloat? {
        textItemView.measuredHeight(forWidth: width)
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        textItemView.removeFromSuperview()
        scrollView.removeFromSuperview()
        codeCard?.removeFromSuperview()
        codeCard?.resetForReuse()
        scrollView.documentView = nil
        scrollView.contentView.scroll(to: .zero)
        textItemView.autoresizingMask = []
        textItemView.prepareForReuse()
        blockID = nil
        copySource = nil
        toggleExpansion = nil
        presentation = nil
        delegate = nil
        delegateCallbacksInstalled = false
        hostLayout = nil
    }

    /// Installs the host for this artifact's block kind and sizes the text
    /// view inside it. Identity and kind changes rebuild the hierarchy; a
    /// same-block refresh only re-sizes.
    private func host(_ artifact: TranscriptTextRenderArtifact, identityChanged: Bool) {
        let text = artifact.text
        let layout: HostLayout
        var cardKind = TranscriptCodeBlockCardView.Kind.code
        switch text.node?.kind {
        case .codeBlock?:
            layout = .codeCard
            copySource = text.source
        case .table?:
            // The bare tile: no Copy, selection copies the pipe table.
            layout = .codeCard
            cardKind = .table
            copySource = nil
        default:
            layout = text.isHorizontallyScrollable ? .scrolling : .plain
            copySource = nil
        }
        installHost(layout, identityChanged: identityChanged)
        blockID = artifact.blockID
        switch layout {
        case .plain:
            textItemView.frame = container.bounds
        case .scrolling:
            scrollView.frame = container.bounds
            // The measured height reserved the legacy scroller's strip; the
            // document must give it back or the last row sits behind the bar.
            let scroller = artifact.options.scrollerInset(
                naturalWidth: artifact.naturalWidth,
                width: container.bounds.width
            )
            textItemView.frame = NSRect(
                x: 0,
                y: 0,
                width: max(container.bounds.width, artifact.naturalWidth ?? container.bounds.width),
                height: container.bounds.height - scroller
            )
        case .codeCard:
            guard let codeCard else { return }
            var languageHint: String?
            if case .codeBlock(let hint, _)? = text.node?.kind { languageHint = hint }
            codeCard.frame = container.bounds
            codeCard.onCopy = { [weak self] in
                guard let self, let copySource else { return }
                copyCode(copySource)
            }
            codeCard.onToggleExpand = { [weak self] in
                guard let self, let blockID else { return }
                delegate?.transcriptTextItem(self, toggleExpansionFor: blockID)
                toggleExpansion?(blockID)
            }
            codeCard.apply(
                kind: cardKind,
                theme: artifact.theme,
                languageHint: languageHint,
                naturalWidth: artifact.naturalWidth,
                lineCount: cardKind == .code ? TranscriptCodeBlockChrome.lineCount(of: text.source) : 0,
                isExpanded: artifact.options.isExpanded
            )
            codeCard.layoutSubtreeIfNeeded()
        }
    }

    private func installHost(_ layout: HostLayout, identityChanged: Bool) {
        guard identityChanged || hostLayout != layout else { return }
        hostLayout = layout
        textItemView.removeFromSuperview()
        scrollView.removeFromSuperview()
        codeCard?.removeFromSuperview()
        scrollView.documentView = nil
        switch layout {
        case .plain:
            container.addSubview(textItemView)
            textItemView.frame = container.bounds
            textItemView.autoresizingMask = [.width, .height]
        case .scrolling:
            container.addSubview(scrollView)
            scrollView.frame = container.bounds
            scrollView.autoresizingMask = [.width, .height]
            textItemView.autoresizingMask = []
            scrollView.documentView = textItemView
        case .codeCard:
            let codeCard = self.codeCard ?? TranscriptCodeBlockCardView()
            self.codeCard = codeCard
            container.addSubview(codeCard)
            codeCard.frame = container.bounds
            codeCard.autoresizingMask = [.width, .height]
            scrollView.autoresizingMask = []
            textItemView.autoresizingMask = []
            scrollView.documentView = textItemView
            codeCard.install(body: scrollView)
        }
        if identityChanged, layout != .plain {
            scrollView.contentView.scroll(to: .zero)
            scrollView.reflectScrolledClipView(scrollView.contentView)
        }
    }
}
