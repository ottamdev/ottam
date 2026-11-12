import AppKit

/// Everything the styler needs to draw: concrete fonts and colors, no closures,
/// so themes compare cheaply and key caches via `signature`.
///
/// Colors may be dynamic (`NSColor(name:)` providers) so one theme serves light
/// and dark; fonts are concrete because a typography change means a new theme.
public struct MarkdownTheme: Equatable {
    public var body: NSFont
    public var code: NSFont
    /// Code spans inside prose. Sits between `code` and `body` so the span
    /// neither shrinks out of the line nor towers over it.
    public var inlineCode: NSFont
    /// Chrome text on a code tile — the fence's language label and the
    /// "Show all" control — in the UI family, a step under `code` so it never
    /// outgrows its band.
    public var codeChrome: NSFont
    public var heading1: NSFont
    public var heading2: NSFont
    public var heading3: NSFont
    public var heading4: NSFont

    public var textColor: NSColor
    public var secondaryTextColor: NSColor
    public var inlineCodeBackground: NSColor
    public var codeBackground: NSColor
    public var codeBorder: NSColor
    public var quoteBar: NSColor
    public var ruleColor: NSColor
    /// Fill of a `ContainerStyle.bubble` tile (the transcript's user turn).
    public var bubbleBackground: NSColor
    public var syntaxColors: [CodeTokenizer.TokenKind: NSColor]

    public struct Metrics: Equatable, Sendable {
        /// Space between lines within a paragraph (~1.45 leading at 14pt).
        public var lineSpacing: CGFloat = 4
        /// Rhythm between blocks in a single styled document (the viewer/editor
        /// projection, where blocks share one text container).
        public var paragraphSpacing: CGFloat = 10
        /// Tighter rhythm between consecutive list items in that projection.
        public var listParagraphSpacing: CGFloat = 4
        /// Rhythm between blocks laid out as separate frames (the transcript,
        /// where each block is its own collection item and paragraph spacing
        /// never reaches the page).
        public var blockSpacing = MarkdownBlockSpacing()
        /// Inline code pill: horizontal padding is real advance (kerning on
        /// the neighbouring glyphs) so the pill never overlaps prose;
        /// vertical padding grows the pill past the run's ascent/descent.
        public var inlineCodePadding: CGFloat = 3
        public var inlineCodeVerticalPadding: CGFloat = 1
        public var inlineCodeCornerRadius: CGFloat = 4
        /// Indent added per list nesting level.
        public var listIndentPerDepth: CGFloat = 20
        /// Code/table block interior horizontal padding.
        public var codeBlockInset: CGFloat = 12
        /// Code/table block interior vertical padding.
        public var codeBlockVerticalInset: CGFloat = 10
        /// Lines shown before a fenced code block collapses behind a
        /// "Show all" control. Collapse rather than in-block vertical scroll:
        /// a tall scrollable tile inside the transcript would be a scroll trap.
        public var codeBlockCollapsedLineLimit: Int = 80
        /// Text inset to the right of a quote bar.
        public var quoteInset: CGFloat = 12
        /// Bubble tile interior horizontal padding (text inset off the tile
        /// edges); also where a quote bar inside a bubble starts.
        public var bubbleInset: CGFloat = 14
        /// Bubble tile interior vertical padding above the first line and
        /// below the last.
        public var bubbleVerticalInset: CGFloat = 10
        public var bubbleCornerRadius: CGFloat = 16
        /// Gap between a bubble tile's bottom edge and the element that
        /// follows it — a turn boundary. An element marked
        /// `attachesToPrevious` (a delivery note under the bubble) keeps plain
        /// paragraph spacing instead.
        public var bubbleSpacingAfter: CGFloat = 15
        /// Widest a bubble tile gets; narrower columns shrink it. The tile is
        /// trailing-aligned and hugs its text up to this width.
        public var bubbleMaxWidth: CGFloat = 560
        public init() {}
    }
    public var metrics = Metrics()

    /// Caller-supplied stable spelling of the font/color selection, for caches
    /// that bake the theme into styled output and must miss on a theme change.
    public var signature: String

    public init(
        body: NSFont,
        code: NSFont,
        heading1: NSFont,
        heading2: NSFont,
        heading3: NSFont,
        heading4: NSFont,
        textColor: NSColor,
        secondaryTextColor: NSColor,
        inlineCodeBackground: NSColor,
        codeBackground: NSColor,
        codeBorder: NSColor,
        quoteBar: NSColor,
        ruleColor: NSColor,
        bubbleBackground: NSColor = NSColor.labelColor.withAlphaComponent(0.06),
        syntaxColors: [CodeTokenizer.TokenKind: NSColor],
        inlineCode: NSFont? = nil,
        codeChrome: NSFont? = nil,
        signature: String
    ) {
        self.body = body
        self.code = code
        self.inlineCode = inlineCode ?? code
        self.codeChrome = codeChrome
            ?? NSFont(descriptor: body.fontDescriptor, size: max(9, code.pointSize - 1))
            ?? body
        self.heading1 = heading1
        self.heading2 = heading2
        self.heading3 = heading3
        self.heading4 = heading4
        self.textColor = textColor
        self.secondaryTextColor = secondaryTextColor
        self.inlineCodeBackground = inlineCodeBackground
        self.codeBackground = codeBackground
        self.codeBorder = codeBorder
        self.quoteBar = quoteBar
        self.ruleColor = ruleColor
        self.bubbleBackground = bubbleBackground
        self.syntaxColors = syntaxColors
        self.signature = signature
    }

    // Dynamic NSColors compare by identity, which would make value-equal themes
    // unequal; the signature is the identity that matters.
    public static func == (lhs: MarkdownTheme, rhs: MarkdownTheme) -> Bool {
        lhs.signature == rhs.signature && lhs.metrics == rhs.metrics
    }

    /// A code span sized for the run it sits in. `inlineCode` is the span
    /// inside body prose; inside a heading or a table's bold header the same
    /// ratio applies to that run's size, so the span never shrinks to body
    /// size in the middle of a 19pt title.
    func inlineCodeFont(within baseFont: NSFont) -> NSFont {
        guard abs(baseFont.pointSize - body.pointSize) > 0.5 else { return inlineCode }
        let size = (baseFont.pointSize * inlineCode.pointSize / body.pointSize).rounded()
        return NSFont(descriptor: inlineCode.fontDescriptor, size: size) ?? inlineCode
    }

    func headingFont(level: Int) -> NSFont {
        switch level {
        case 1: return heading1
        case 2: return heading2
        case 3: return heading3
        default: return heading4
        }
    }
}

/// Markdown → `NSAttributedString` projection: real `NSParagraphStyle`s,
/// theme fonts/colors, semantic attributes, and inline image attachments.
/// Consumers own layout, scrolling and interaction.
public struct MarkdownStyler {
    public let theme: MarkdownTheme
    /// Directory a schemeless image destination resolves against, and the
    /// boundary it may not escape — the conversation's working root. Nil leaves
    /// such destinations unresolved, so they fail to load
    /// (`MarkdownImageAttachment.resolve`).
    public let resourceRoot: URL?

    public init(theme: MarkdownTheme, resourceRoot: URL? = nil) {
        self.theme = theme
        self.resourceRoot = resourceRoot
    }

    public func attributedString(from markdown: String, syntax: MarkdownSyntax = .full) -> NSAttributedString {
        attributedString(from: MarkdownBlockTreeParser.nodes(from: markdown, syntax: syntax))
    }

    /// `firstBlockID` and `previousNodeIsListItem` let an editor or viewer style
    /// a document suffix while block IDs and list rhythm stay consistent.
    public func attributedString(
        from nodes: [MarkdownBlockNode],
        firstBlockID: Int = 0,
        previousNodeIsListItem: Bool = false,
        endsDocument: Bool = true
    ) -> NSAttributedString {
        let result = NSMutableAttributedString()
        for (index, node) in nodes.enumerated() {
            let followsListItem = index > 0 ? isListItem(nodes[index - 1]) : previousNodeIsListItem
            let block = render(node, followsListItem: followsListItem)
            if index < nodes.count - 1 || !endsDocument {
                // The terminating newline is part of this block's paragraph, so it
                // carries this block's attributes — semantic ranges stay contiguous.
                block.append(NSAttributedString(string: "\n", attributes: separatorAttributes(matching: block)))
            }
            applyQuoteIndent(block, depth: node.quoteDepth)
            var attributes: [NSAttributedString.Key: Any] = [
                MarkdownAttribute.blockRole: NSNumber(value: node.role.rawValue),
                MarkdownAttribute.blockID: NSNumber(value: firstBlockID + index)
            ]
            if node.quoteDepth > 0 {
                attributes[MarkdownAttribute.quoteDepth] = NSNumber(value: node.quoteDepth)
            }
            if case .listItem(let depth, _, _) = node.kind {
                attributes[MarkdownAttribute.listDepth] = NSNumber(value: depth)
            }
            if case .codeBlock(let hint, _) = node.kind, let hint {
                attributes[MarkdownAttribute.codeLanguage] = hint as NSString
            }
            block.addAttributes(attributes, range: NSRange(location: 0, length: block.length))
            result.append(block)
        }
        return result
    }

    /// Shifts a block right by one quote inset per enclosing level, on top of
    /// whatever indent its own kind set. Applied after rendering so quoting
    /// composes with every block kind.
    private func applyQuoteIndent(_ block: NSMutableAttributedString, depth: Int) {
        guard depth > 0, block.length > 0 else { return }
        let inset = theme.metrics.quoteInset * CGFloat(depth)
        block.editParagraphStyles { style in
            style.firstLineHeadIndent += inset
            style.headIndent += inset
        }
    }

    // MARK: - Blocks

    private func isListItem(_ node: MarkdownBlockNode) -> Bool {
        if case .listItem = node.kind { return true }
        return false
    }

    private func render(_ node: MarkdownBlockNode, followsListItem: Bool) -> NSMutableAttributedString {
        switch node.kind {
        case .paragraph:
            // Quoted prose still reads as secondary; the bar alone is a weaker
            // signal than the old dimmed-and-barred pairing.
            let block = inlineText(
                node.inline,
                baseFont: theme.body,
                baseColor: node.quoteDepth > 0 ? theme.secondaryTextColor : theme.textColor
            )
            block.applyParagraphStyle(baseParagraphStyle())
            return block

        case .heading(let level):
            let block = inlineText(node.inline, baseFont: theme.headingFont(level: level), baseColor: theme.textColor)
            block.applyParagraphStyle(baseParagraphStyle())
            return block

        case .codeBlock(let hint, let code):
            return codeBlock(code, hint: hint)

        case .listItem(let depth, let marker, let isContinuation):
            return listItem(
                node.inline,
                depth: depth,
                marker: marker,
                isContinuation: isContinuation,
                tight: followsListItem
            )

        case .thematicBreak:
            let block = NSMutableAttributedString(string: "────────", attributes: [
                .font: theme.body,
                .foregroundColor: theme.ruleColor,
            ])
            block.applyParagraphStyle(baseParagraphStyle())
            return block

        case .table(let header, let rows, _):
            return tableBlock(header: header, rows: rows)
        }
    }

    /// Syntax-highlighted code with a stable line height. Geometry, scrolling
    /// and optional chrome belong to the consuming view.
    public func highlightedCode(_ code: String, hint: String?) -> NSAttributedString {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byClipping
        style.lineSpacing = 2
        let font = theme.code
        let lineHeight = ceil(font.ascender - font.descender + font.leading)
        style.minimumLineHeight = lineHeight
        style.maximumLineHeight = lineHeight

        let block = NSMutableAttributedString(string: code, attributes: [
            .font: theme.code,
            .foregroundColor: theme.textColor,
            .paragraphStyle: style
        ])
        let language = CodeTokenizer.Language(hint: hint)
        (block.string as NSString).enumerateSubstrings(
            in: NSRange(location: 0, length: block.length),
            options: [.byLines, .substringNotRequired]
        ) { _, lineRange, _, _ in
            let line = (block.string as NSString).substring(with: lineRange)
            for token in CodeTokenizer.tokens(in: line, language: language) {
                guard let color = self.theme.syntaxColors[token.kind] else { continue }
                let shifted = NSRange(location: lineRange.location + token.range.location, length: token.range.length)
                block.addAttribute(.foregroundColor, value: color, range: shifted)
            }
        }
        return block
    }

    private func codeBlock(_ code: String, hint: String?) -> NSMutableAttributedString {
        let block = NSMutableAttributedString(attributedString: highlightedCode(code, hint: hint))
        block.addAttribute(.backgroundColor, value: theme.codeBackground, range: NSRange(location: 0, length: block.length))
        block.editParagraphStyles { style in
            style.paragraphSpacing = theme.metrics.paragraphSpacing
        }
        return block
    }

    /// The viewer/editor projection: one line per row, cells divided by a bar.
    /// The transcript lays tables out on measured tab stops instead.
    private func tableBlock(
        header: [AttributedString]?,
        rows: [[AttributedString]]
    ) -> NSMutableAttributedString {
        let separator: [NSAttributedString.Key: Any] = [
            .font: theme.code,
            .foregroundColor: theme.secondaryTextColor,
        ]
        let block = NSMutableAttributedString()
        for (rowIndex, row) in ((header.map { [$0] } ?? []) + rows).enumerated() {
            if rowIndex > 0 { block.append(NSAttributedString(string: "\n", attributes: separator)) }
            for (column, cell) in row.enumerated() {
                if column > 0 { block.append(NSAttributedString(string: " │ ", attributes: separator)) }
                block.append(inlineText(cell, baseFont: theme.code, baseColor: theme.textColor))
            }
        }
        block.addAttribute(
            .backgroundColor, value: theme.codeBackground, range: NSRange(location: 0, length: block.length)
        )
        block.applyParagraphStyle(baseParagraphStyle())
        return block
    }

    /// Inline content styled on a base face: what a table cell, a paragraph
    /// or a list item's text is made of. Public so the transcript can measure
    /// and place table cells with the same styling the viewer uses.
    public func inlineAttributedString(
        _ inline: AttributedString,
        baseFont: NSFont,
        baseColor: NSColor
    ) -> NSAttributedString {
        inlineText(inline, baseFont: baseFont, baseColor: baseColor)
    }

    private func listItem(
        _ inline: AttributedString,
        depth: Int,
        marker: MarkdownBlockNode.ListMarker,
        isContinuation: Bool,
        tight: Bool
    ) -> NSMutableAttributedString {
        let block = NSMutableAttributedString()
        let indent = CGFloat(max(depth - 1, 0)) * theme.metrics.listIndentPerDepth

        let markerText = Self.markerText(for: marker, depth: depth)
        let markerFont = Self.markerFont(for: marker, theme: theme)
        let measuredMarkerWidth = ceil((markerText as NSString).size(withAttributes: [.font: markerFont]).width)
        // Ordinals share one text column, wide enough for two digits, so a
        // list that runs past 9 keeps its text aligned; the marker's trailing
        // space is kerned out to the column. Bullets hug their text.
        let markerWidth: CGFloat
        if case .ordinal = marker {
            let column = ceil(("00. " as NSString).size(withAttributes: [.font: theme.body]).width)
            markerWidth = max(column, measuredMarkerWidth)
        } else {
            markerWidth = measuredMarkerWidth
        }

        if !isContinuation {
            let markerString = NSMutableAttributedString(string: markerText, attributes: [
                .font: markerFont,
                .foregroundColor: theme.secondaryTextColor
            ])
            if markerWidth > measuredMarkerWidth {
                markerString.addAttribute(
                    .kern,
                    value: markerWidth - measuredMarkerWidth,
                    range: NSRange(location: markerString.length - 1, length: 1)
                )
            }
            block.append(markerString)
        }
        // A finished task reads as done: its text dims to the marker's color.
        let textColor: NSColor
        if case .task(checked: true) = marker {
            textColor = theme.secondaryTextColor
        } else {
            textColor = theme.textColor
        }
        block.append(inlineText(inline, baseFont: theme.body, baseColor: textColor))

        // Hanging indent: wrapped lines (and continuation paragraphs) align under
        // the item's text, not under the marker.
        let style = baseParagraphStyle()
        style.firstLineHeadIndent = isContinuation ? indent + markerWidth : indent
        style.headIndent = indent + markerWidth
        style.paragraphSpacing = theme.metrics.listParagraphSpacing
        if tight {
            style.paragraphSpacingBefore = 0
        }
        block.applyParagraphStyle(style)
        return block
    }

    /// The characters a list marker renders as. Shared with the transcript's
    /// display source, whose characters must equal the styled string's so
    /// selection offsets index both alike. Bullets alternate by depth the way
    /// GitHub and Apple do — • ◦ ▪ — so nesting reads without counting indent.
    public static func markerText(for marker: MarkdownBlockNode.ListMarker, depth: Int) -> String {
        switch marker {
        case .bullet:
            switch depth {
            case ...1: return "• "
            case 2: return "◦ "
            default: return "▪ "
            }
        case .ordinal(let ordinal): return "\(ordinal). "
        case .task(let checked): return checked ? "☑ " : "☐ "
        }
    }

    /// The font a marker is set in. Ballot boxes come from a fallback face
    /// whose glyphs sit well below the bullet's visual weight at body size,
    /// so tasks step up two points to read as boxes rather than specks.
    static func markerFont(for marker: MarkdownBlockNode.ListMarker, theme: MarkdownTheme) -> NSFont {
        guard case .task = marker else { return theme.body }
        return NSFont(descriptor: theme.body.fontDescriptor, size: theme.body.pointSize + 2) ?? theme.body
    }

    // MARK: - Inline runs

    private func inlineText(
        _ inline: AttributedString,
        baseFont: NSFont,
        baseColor: NSColor
    ) -> NSMutableAttributedString {
        let result = NSMutableAttributedString()
        for run in inline.runs {
            let text = String(inline[run.range].characters)
            var attributes: [NSAttributedString.Key: Any] = [
                .font: baseFont,
                .foregroundColor: baseColor
            ]

            if let intent = run.inlinePresentationIntent {
                if intent.contains(.code) {
                    attributes[.font] = theme.inlineCodeFont(within: baseFont)
                    attributes[.backgroundColor] = theme.inlineCodeBackground
                    attributes[MarkdownAttribute.inlineRole] = NSNumber(value: MarkdownInlineRole.code.rawValue)
                } else {
                    var font = baseFont
                    if intent.contains(.stronglyEmphasized) {
                        font = font.withTrait(.boldFontMask)
                    }
                    if intent.contains(.emphasized) {
                        font = font.withTrait(.italicFontMask)
                    }
                    attributes[.font] = font
                }
                if intent.contains(.strikethrough) {
                    attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
                }
            }

            if let link = run.link {
                attributes[.link] = link as NSURL
            }
            if let imageURL = run.imageURL {
                // The run's text is the alt text; the visible character is the
                // attachment. Show a placeholder immediately and load the
                // image out-of-band.
                let attachment = MarkdownImageAttachment(
                    url: MarkdownImageAttachment.resolveImage(imageURL, relativeTo: resourceRoot),
                    altText: text
                )
                let attachmentString = NSMutableAttributedString(attachment: attachment)
                attributes[MarkdownAttribute.inlineRole] =
                    NSNumber(value: MarkdownInlineRole.image.rawValue)
                attributes.removeValue(forKey: .backgroundColor)
                attachmentString.addAttributes(
                    attributes,
                    range: NSRange(location: 0, length: attachmentString.length)
                )
                result.append(attachmentString)
                continue
            }

            result.append(NSAttributedString(string: text, attributes: attributes))
        }
        return result
    }

    private func baseParagraphStyle() -> NSMutableParagraphStyle {
        let style = NSMutableParagraphStyle()
        style.lineSpacing = theme.metrics.lineSpacing
        style.paragraphSpacing = theme.metrics.paragraphSpacing
        style.lineBreakMode = .byWordWrapping
        return style
    }

    /// The terminating newline inherits its block's font and paragraph style (so the
    /// paragraph stays uniform) but never a background or link.
    private func separatorAttributes(matching block: NSAttributedString) -> [NSAttributedString.Key: Any] {
        guard block.length > 0 else { return [:] }
        var attributes = block.attributes(at: block.length - 1, effectiveRange: nil)
        attributes.removeValue(forKey: .backgroundColor)
        attributes.removeValue(forKey: .link)
        attributes.removeValue(forKey: .attachment)
        return attributes
    }
}

extension NSMutableAttributedString {
    /// Rewrites every paragraph style in `range` (the whole string by default)
    /// through `edit`, which receives a mutable copy of each run's style.
    ///
    /// Attribute-only: paragraph styles change, characters do not, so an
    /// image attachment inside the range keeps its identity. Runs arrive in
    /// order, so an edit may carry state across them.
    func editParagraphStyles(in range: NSRange? = nil, _ edit: (NSMutableParagraphStyle) -> Void) {
        enumerateAttribute(
            .paragraphStyle, in: range ?? NSRange(location: 0, length: length)
        ) { value, styleRange, _ in
            guard let style = (value as? NSParagraphStyle)?.mutableCopy() as? NSMutableParagraphStyle
            else { return }
            edit(style)
            addAttribute(.paragraphStyle, value: style, range: styleRange)
        }
    }
}

private extension NSMutableAttributedString {
    func applyParagraphStyle(_ style: NSParagraphStyle) {
        addAttribute(.paragraphStyle, value: style, range: NSRange(location: 0, length: length))
    }
}

private extension NSFont {
    func withTrait(_ trait: NSFontTraitMask) -> NSFont {
        var symbolicTraits = fontDescriptor.symbolicTraits
        if trait.contains(.boldFontMask) { symbolicTraits.insert(.bold) }
        if trait.contains(.italicFontMask) { symbolicTraits.insert(.italic) }
        let descriptor = fontDescriptor.withSymbolicTraits(symbolicTraits)
        return NSFont(descriptor: descriptor, size: pointSize) ?? self
    }
}
