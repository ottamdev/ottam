import AppKit
import CoreText
import OttamMarkdownEngine

/// Height, face and spacing rules shared by table measurement, the item's
/// grid drawing and the card, so the layout's block height and the rendered
/// chrome can never disagree. Every value is a pure function of the theme.
enum TranscriptTableChrome {
    /// Space between a column boundary and the cell text on either side.
    static let cellPadding: CGFloat = 10

    /// Body cells read as prose, so they use the body face; the header is
    /// the same face emboldened. A cell's inline code still takes the code
    /// face, which is why the row height pins to the taller of the two.
    static func bodyFont(theme: MarkdownTheme) -> NSFont { theme.body }

    static func headerFont(theme: MarkdownTheme) -> NSFont {
        NSFontManager.shared.convert(theme.body, toHaveTrait: .boldFontMask)
    }

    /// Space above and below a row's line.
    static let rowPadding: CGFloat = 8

    /// One row, pinned so inline code or emphasis cannot bump a row's height:
    /// the taller of the body and code faces, plus the row padding.
    static func rowHeight(theme: MarkdownTheme) -> CGFloat {
        let body = theme.body.ascender - theme.body.descender + theme.body.leading
        let code = theme.code.ascender - theme.code.descender + theme.code.leading
        return ceil(max(body, code)) + rowPadding * 2
    }

    /// A table's tile has no band above the grid; the rows start at the
    /// tile's top edge.
    static func headerHeight(theme: MarkdownTheme) -> CGFloat { 0 }
}

/// Where each column sits: measured once per (node, theme), consumed by the
/// tab stops that align the text, by the natural width that decides
/// overflow, and by the row rules the item draws.
struct TranscriptTableLayout: Equatable {
    struct Column: Equatable {
        /// Leading edge of the column, including its padding.
        let x: CGFloat
        /// Full column width, padding on both sides included.
        let width: CGFloat
        let alignment: MarkdownTableAlignment

        var maxX: CGFloat { x + width }
    }

    let columns: [Column]
    let rowCount: Int
    /// The grid at its content width, before any stretch to the item; what
    /// decides whether the table overflows.
    let naturalWidth: CGFloat

    /// Total grid width: the last column's trailing edge.
    var width: CGFloat { columns.last?.maxX ?? 0 }

    /// The styled rows, header first when present. Kept alongside the
    /// columns so the attributed string is built from the same cells that
    /// were measured.
    let rows: [[NSAttributedString]]
    let hasHeader: Bool

    /// `fillWidth` is the item's width: a grid narrower than it stretches
    /// every column in proportion so the table fills its tile, the way a
    /// full-width HTML table does; a wider grid keeps its content width and
    /// overflows.
    static func measure(
        header: [AttributedString]?,
        rows: [[AttributedString]],
        alignments: [MarkdownTableAlignment],
        theme: MarkdownTheme,
        fillWidth: CGFloat = 0
    ) -> TranscriptTableLayout {
        let styler = MarkdownStyler(theme: theme)
        let headerFont = TranscriptTableChrome.headerFont(theme: theme)
        let bodyFont = TranscriptTableChrome.bodyFont(theme: theme)
        // Cells take the same pill treatment as prose, and take it before the
        // columns are measured so the padding is part of the cell's width.
        func cell(_ inline: AttributedString, font: NSFont) -> NSAttributedString {
            TranscriptTextMeasurement.applyingInlineCodePillPadding(
                to: styler.inlineAttributedString(
                    TranscriptTableSource.flatteningImages(inline), baseFont: font, baseColor: theme.textColor
                ),
                theme: theme
            )
        }
        var styledRows: [[NSAttributedString]] = []
        if let header {
            styledRows.append(header.map { cell($0, font: headerFont) })
        }
        for row in rows {
            styledRows.append(row.map { cell($0, font: bodyFont) })
        }

        let columnCount = alignments.count
        var contentWidths = Array(repeating: CGFloat(0), count: columnCount)
        for row in styledRows {
            for (index, cell) in row.prefix(columnCount).enumerated() where cell.length > 0 {
                let line = CTLineCreateWithAttributedString(cell)
                let width = CTLineGetTypographicBounds(line, nil, nil, nil)
                contentWidths[index] = max(contentWidths[index], ceil(width))
            }
        }

        var widths = contentWidths.map { $0 + TranscriptTableChrome.cellPadding * 2 }
        let naturalWidth = widths.reduce(0, +)
        if naturalWidth > 0, fillWidth > naturalWidth {
            // Whole points keep the stops crisp; the last column takes the
            // rounding remainder so the grid ends exactly at the edge.
            let scale = fillWidth / naturalWidth
            widths = widths.map { floor($0 * scale) }
            widths[widths.count - 1] += fillWidth - widths.reduce(0, +)
        }
        var columns: [Column] = []
        var x: CGFloat = 0
        for (index, alignment) in alignments.enumerated() {
            columns.append(Column(x: x, width: widths[index], alignment: alignment))
            x += widths[index]
        }
        return TranscriptTableLayout(
            columns: columns,
            rowCount: styledRows.count,
            naturalWidth: naturalWidth,
            rows: styledRows,
            hasHeader: header != nil
        )
    }

    /// One tab stop per column, at the position Core Text aligns that
    /// column's text to: its leading padding edge for left, its center for
    /// centered, its trailing padding edge for right. Every row starts with a
    /// tab so the first column is a stop like the others and can carry its own
    /// alignment (a paragraph alignment would shift the whole row).
    var tabStops: [NSTextTab] {
        columns.map { column in
            let padding = TranscriptTableChrome.cellPadding
            switch column.alignment {
            case .left:
                return NSTextTab(textAlignment: .left, location: column.x + padding)
            case .center:
                return NSTextTab(textAlignment: .center, location: column.x + column.width / 2)
            case .right:
                return NSTextTab(textAlignment: .right, location: column.maxX - padding)
            }
        }
    }

    /// The grid as one Core Text document: rows on their own lines, each
    /// row a leading tab and tab-separated cells, on this layout's stops with
    /// a pinned row height. Its characters equal the block's display source,
    /// which is what keeps selection offsets and hit-testing honest.
    ///
    /// Each row is its own paragraph, and the pin is paragraph spacing below
    /// the row's natural line rather than a minimum line height: a
    /// line-height pin inflates the line's ascent and descent, and Core Text
    /// paints an inline-code background over that whole box, so the pill
    /// would fill the row. Spacing keeps the line's own metrics, so the pill
    /// stays as tight as it is in prose.
    ///
    /// Core Text ignores `paragraphSpacingBefore` on a frame's first
    /// paragraph, so the space above a row is one inset shared by every row
    /// (`rowInset`, sized for the tallest row) that the item applies as the
    /// text rect's vertical inset; each row's `paragraphSpacing` then takes
    /// up whatever its own line leaves of the row height. The inset is also
    /// recorded as every row's `paragraphSpacingBefore` so geometry can read
    /// it back from the string.
    func attributedString(theme: MarkdownTheme) -> NSAttributedString {
        let rowHeight = TranscriptTableChrome.rowHeight(theme: theme)
        let separatorAttributes: [NSAttributedString.Key: Any] = [
            .font: TranscriptTableChrome.bodyFont(theme: theme),
            .foregroundColor: theme.textColor,
        ]
        var rowTexts: [NSMutableAttributedString] = []
        var naturalHeights: [CGFloat] = []
        for row in rows {
            let rowText = NSMutableAttributedString()
            for cell in row {
                rowText.append(NSAttributedString(string: "\t", attributes: separatorAttributes))
                rowText.append(cell)
            }
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            var leading: CGFloat = 0
            let line = CTLineCreateWithAttributedString(rowText)
            CTLineGetTypographicBounds(line, &ascent, &descent, &leading)
            rowTexts.append(rowText)
            // Whole points: font metrics are fractional and Core Text's line
            // advance rounds its own way, so the row's line height is pinned
            // to this value below and the pitch stays exact on any font.
            naturalHeights.append(ceil(ascent + descent + leading))
        }
        let inset = Self.rowInset(rowHeight: rowHeight, naturalHeights: naturalHeights)
        let result = NSMutableAttributedString()
        for (rowIndex, rowText) in rowTexts.enumerated() {
            // The newline that ends a row is part of its paragraph.
            if rowIndex < rowTexts.count - 1 {
                rowText.append(NSAttributedString(string: "\n", attributes: separatorAttributes))
            }
            let style = NSMutableParagraphStyle()
            style.tabStops = tabStops
            style.defaultTabInterval = 100_000
            style.lineBreakMode = .byClipping
            style.lineSpacing = 0
            // The line's own height, rounded up: at most a point over the
            // glyph box, so an inline-code pill stays tight.
            style.minimumLineHeight = naturalHeights[rowIndex]
            style.maximumLineHeight = naturalHeights[rowIndex]
            style.paragraphSpacingBefore = inset
            style.paragraphSpacing = max(0, rowHeight - inset - naturalHeights[rowIndex])
            rowText.addAttribute(.paragraphStyle, value: style, range: NSRange(location: 0, length: rowText.length))
            result.append(rowText)
        }
        return result
    }

    /// The space above every row's line: centers the tallest row, so every
    /// row fits under its rule.
    static func rowInset(rowHeight: CGFloat, naturalHeights: [CGFloat]) -> CGFloat {
        max(0, floor((rowHeight - (naturalHeights.max() ?? 0)) / 2))
    }

    /// The inset `attributedString(theme:)` recorded, read back from a
    /// table's string by geometry.
    static func rowInset(in attributed: NSAttributedString) -> CGFloat {
        guard attributed.length > 0,
              let style = attributed.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        else { return 0 }
        return style.paragraphSpacingBefore
    }
}

/// The pipe-table spelling of a parsed table: what a whole-block selection
/// copies, so a pasted table parses back to the same grid.
enum TranscriptTableSource {
    static func pipeTable(
        header: [AttributedString]?,
        rows: [[AttributedString]],
        alignments: [MarkdownTableAlignment]
    ) -> String {
        let columnCount = alignments.count
        func spelled(_ row: [AttributedString]) -> [String] {
            var cells = row.prefix(columnCount).map { cell in
                cellSource(cell).replacingOccurrences(of: "|", with: "\\|")
            }
            while cells.count < columnCount { cells.append("") }
            return cells
        }
        let headerCells = spelled(header ?? Array(repeating: AttributedString(), count: columnCount))
        let bodyCells = rows.map(spelled)
        var widths = alignments.map { _ in 3 }
        for row in [headerCells] + bodyCells {
            for (index, cell) in row.enumerated() {
                widths[index] = max(widths[index], cell.count)
            }
        }
        func pad(_ cell: String, to width: Int, alignment: MarkdownTableAlignment) -> String {
            let missing = max(0, width - cell.count)
            switch alignment {
            case .left: return cell + String(repeating: " ", count: missing)
            case .right: return String(repeating: " ", count: missing) + cell
            case .center:
                let leading = missing / 2
                return String(repeating: " ", count: leading) + cell
                    + String(repeating: " ", count: missing - leading)
            }
        }
        func line(_ cells: [String]) -> String {
            "| " + zip(cells, zip(widths, alignments)).map { cell, spec in
                pad(cell, to: spec.0, alignment: spec.1)
            }.joined(separator: " | ") + " |"
        }
        let rule = zip(widths, alignments).map { width, alignment -> String in
            switch alignment {
            case .left: return String(repeating: "-", count: width)
            case .center: return ":" + String(repeating: "-", count: max(1, width - 2)) + ":"
            case .right: return String(repeating: "-", count: max(1, width - 1)) + ":"
            }
        }
        return ([line(headerCells), line(rule)] + bodyCells.map(line)).joined(separator: "\n")
    }

    /// A cell's Markdown, re-spelled from its inline runs so the copy parses
    /// back to the same cell: plain text has its syntax characters escaped,
    /// code spans take a backtick run longer than any inside them, and a
    /// style that spans several runs (bold around italic, a link around
    /// bold) opens once and closes once, tracked across run boundaries.
    static func cellSource(_ cell: AttributedString) -> String {
        var result = ""
        var open: [CellMarker] = []
        func close(downTo count: Int) {
            while open.count > count { result += open.removeLast().closing }
        }
        for run in cell.runs {
            let text = String(cell[run.range].characters)
            if let imageURL = run.imageURL {
                close(downTo: 0)
                result += "![\(escaped(text))](\(imageURL.absoluteString))"
                continue
            }
            let intent = run.inlinePresentationIntent ?? []
            var wanted: [CellMarker] = []
            if let link = run.link { wanted.append(.link(link)) }
            if intent.contains(.stronglyEmphasized) { wanted.append(.strong) }
            if intent.contains(.emphasized) { wanted.append(.emphasis) }
            if intent.contains(.strikethrough) { wanted.append(.strikethrough) }
            let shared = zip(open, wanted).prefix { $0 == $1 }.count
            close(downTo: shared)
            for marker in wanted.dropFirst(shared) {
                result += marker.opening
                open.append(marker)
            }
            result += intent.contains(.code) ? codeSpan(text) : escaped(text)
        }
        close(downTo: 0)
        return result
    }

    private enum CellMarker: Equatable {
        case link(URL)
        case strong
        case emphasis
        case strikethrough

        var opening: String {
            switch self {
            case .link: return "["
            case .strong: return "**"
            case .emphasis: return "*"
            case .strikethrough: return "~~"
            }
        }

        var closing: String {
            switch self {
            case .link(let url): return "](\(url.absoluteString))"
            case .strong: return "**"
            case .emphasis: return "*"
            case .strikethrough: return "~~"
            }
        }
    }

    /// Plain text with every character that could open inline syntax
    /// escaped. `|` is left to the table serializer, which escapes it for
    /// every cell, code included.
    private static func escaped(_ text: String) -> String {
        var result = ""
        for character in text {
            if "\\*_`~[]<".contains(character) { result.append("\\") }
            result.append(character)
        }
        return result
    }

    /// A code span whose delimiter is one backtick longer than the longest
    /// run inside it, padded when the code starts or ends with a backtick.
    private static func codeSpan(_ code: String) -> String {
        var longest = 0
        var current = 0
        for character in code {
            current = character == "`" ? current + 1 : 0
            longest = max(longest, current)
        }
        let fence = String(repeating: "`", count: longest + 1)
        let padded = code.hasPrefix("`") || code.hasSuffix("`") ? " \(code) " : code
        return fence + padded + fence
    }

    /// A cell with its images as their alt text. A row is one line of a
    /// fixed height and an attachment's size is only known after it loads,
    /// so the grid renders the text and the copy keeps the image syntax.
    static func flatteningImages(_ cell: AttributedString) -> AttributedString {
        cell.transformingAttributes(\.imageURL) { $0.value = nil }
    }
}
