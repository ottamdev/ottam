import OttamInstrumentation
import Foundation

/// Structure-only Markdown parsing shared by viewers, editors and transcript
/// projection.
///
/// Foundation's `AttributedString(markdown:)` does the parsing (`.full` syntax so
/// `PresentationIntent` block attributes survive); the walk flattens the intent
/// runs into `MarkdownBlockNode`s. Nodes carry no fonts or colors — presentation
/// is `MarkdownStyler`'s job — but inline styling intents, links, and image URLs
/// are preserved so the styler can project them.

public enum MarkdownSyntax: Hashable, Sendable {
    /// Full block-level markdown — agent replies.
    case full
    /// Inline styling only, newlines preserved — user-typed text, where reflowing
    /// single line breaks into one paragraph would misrepresent the input.
    case inline
}

public struct MarkdownBlockNode: Equatable, Sendable {
    public enum ListMarker: Equatable, Sendable {
        case bullet
        case ordinal(Int)
        /// A GFM task item (`- [ ]` / `- [x]`). Foundation's parser leaves
        /// the brackets in the item's text; the walk lifts them out here so a
        /// renderer can draw a checkbox and the copied text stays as written.
        case task(checked: Bool)

        /// Whether two markers belong to the same kind of list: a numbered
        /// item after a bullet starts a new list, a task after a bullet does
        /// not (both are unordered).
        public var isOrdered: Bool {
            if case .ordinal = self { return true }
            return false
        }
    }

    public enum Kind: Equatable, Sendable {
        case paragraph
        case heading(level: Int)
        case codeBlock(languageHint: String?, code: String)
        /// `isContinuation` marks a follow-on paragraph inside the same list item;
        /// it continues under the marker rather than repeating it.
        case listItem(depth: Int, marker: ListMarker, isContinuation: Bool)
        case thematicBreak
        /// A parsed grid. Every row has exactly `alignments.count` cells (the
        /// delimiter row fixes the column count; short rows pad with empty
        /// cells, long rows drop the surplus, as GFM renders them). A cell is
        /// inline content like a paragraph's, so emphasis, code and links in
        /// cells survive.
        case table(
            header: [AttributedString]?,
            rows: [[AttributedString]],
            alignments: [MarkdownTableAlignment]
        )
    }

    public let kind: Kind
    /// How many blockquote levels enclose this block. A quote is *containment*,
    /// not a kind: a list or a fence inside a quote keeps its own kind and
    /// gains a depth, which is what the old `.blockquote` kind could not
    /// express (it lost whichever of the two lost the branch race).
    public let quoteDepth: Int
    /// The block's inline content with `presentationIntent` stripped but
    /// `inlinePresentationIntent`, `link`, and `imageURL` preserved. Empty for
    /// code blocks, tables, and thematic breaks, which carry their content in `kind`.
    public let inline: AttributedString

    public init(
        kind: Kind,
        inline: AttributedString = AttributedString(),
        quoteDepth: Int = 0
    ) {
        self.kind = kind
        self.inline = inline
        self.quoteDepth = quoteDepth
    }
}

/// How a table column's cells sit, from the delimiter row (`:--`, `:-:`, `--:`).
public enum MarkdownTableAlignment: Equatable, Sendable {
    case left
    case center
    case right
}

/// A table being assembled cell by cell as the intent walk streams past. Rows
/// close on a row-identity change rather than on a lookahead, so a table that
/// ends the document closes on the final flush like any other block.
private struct PendingTable {
    let identity: Int
    let alignments: [MarkdownTableAlignment]
    var quoteDepth = 0
    var rowIdentity: Int?
    var rowIsHeader = false
    /// Sized to the column count up front so cells land by index and a short
    /// row pads with empties.
    var cells: [AttributedString]
    var hasCells = false
    var header: [AttributedString]?
    var rows: [[AttributedString]] = []

    init(identity: Int, alignments: [MarkdownTableAlignment]) {
        self.identity = identity
        self.alignments = alignments
        cells = Array(repeating: AttributedString(), count: alignments.count)
    }

    mutating func place(_ cell: AttributedString, at column: Int) {
        guard column >= 0, column < cells.count else { return }
        cells[column] = cell
        hasCells = true
    }

    mutating func closeRow() {
        guard hasCells else { return }
        if rowIsHeader, header == nil {
            header = cells
        } else {
            rows.append(cells)
        }
        cells = Array(repeating: AttributedString(), count: alignments.count)
        hasCells = false
    }
}

public enum MarkdownBlockTreeParser {
    public static func nodes(from text: String, syntax: MarkdownSyntax = .full) -> [MarkdownBlockNode] {
        TranscriptSignpost.markdownParse(
            bytes: text.utf8.count,
            syntax: syntax == .full ? "full" : "inline"
        ) {
            parse(text, syntax: syntax)
        }
    }

    private static func parse(_ text: String, syntax: MarkdownSyntax) -> [MarkdownBlockNode] {
        if syntax == .inline, let segments = quoteSegments(text) {
            return segments.flatMap { segment in
                parse(segment.text, syntax: .inline).map { node in
                    MarkdownBlockNode(
                        kind: node.kind,
                        inline: node.inline,
                        quoteDepth: node.quoteDepth + (segment.isQuote ? 1 : 0)
                    )
                }
            }
        }
        let options = AttributedString.MarkdownParsingOptions(
            allowsExtendedAttributes: true,
            interpretedSyntax: syntax == .full ? .full : .inlineOnlyPreservingWhitespace,
            failurePolicy: .returnPartiallyParsedIfPossible
        )
        guard let attributed = try? AttributedString(markdown: text, options: options) else {
            // Agent output must never crash or vanish; fall back to the raw text.
            return fallback(text)
        }

        var nodes: [MarkdownBlockNode] = []
        var lastListItemIdentity: Int?
        var pendingTable: PendingTable?

        func flushTable() {
            guard var pending = pendingTable else { return }
            pendingTable = nil
            pending.closeRow()
            nodes.append(MarkdownBlockNode(
                kind: .table(
                    header: pending.header,
                    rows: pending.rows,
                    alignments: pending.alignments
                ),
                quoteDepth: pending.quoteDepth
            ))
        }

        for (intent, range) in attributed.runs[\.presentationIntent] {
            let rawText = String(attributed[range].characters)

            guard let intent else {
                flushTable()
                let slice = inlineSlice(attributed[range])
                if !slice.characters.isEmpty {
                    nodes.append(MarkdownBlockNode(kind: .paragraph, inline: slice))
                }
                continue
            }

            var headerLevel: Int?
            var codeHint: String?
            var isCode = false
            var quoteDepth = 0
            var isThematicBreak = false
            var listItemOrdinal: Int?
            var listItemIdentity: Int?
            var innermostListIsOrdered: Bool?
            var listDepth = 0
            var tableIdentity: Int?
            var tableAlignments: [MarkdownTableAlignment] = []
            var tableRowIdentity: Int?
            var tableRowIsHeader = false
            var tableColumn = 0

            // Components run innermost-first, so the first list component seen is the
            // one whose marker style applies to this item.
            for component in intent.components {
                switch component.kind {
                case .header(let level):
                    headerLevel = level
                case .codeBlock(let languageHint):
                    isCode = true
                    codeHint = languageHint
                case .blockQuote:
                    quoteDepth += 1
                case .thematicBreak:
                    isThematicBreak = true
                case .listItem(let ordinal):
                    if listItemIdentity == nil {
                        listItemOrdinal = ordinal
                        listItemIdentity = component.identity
                    }
                case .orderedList:
                    listDepth += 1
                    if innermostListIsOrdered == nil { innermostListIsOrdered = true }
                case .unorderedList:
                    listDepth += 1
                    if innermostListIsOrdered == nil { innermostListIsOrdered = false }
                case .table(let columns):
                    tableIdentity = component.identity
                    tableAlignments = columns.map { column in
                        switch column.alignment {
                        case .center: return .center
                        case .right: return .right
                        default: return .left
                        }
                    }
                case .tableRow:
                    if tableRowIdentity == nil { tableRowIdentity = component.identity }
                case .tableHeaderRow:
                    if tableRowIdentity == nil {
                        tableRowIdentity = component.identity
                        tableRowIsHeader = true
                    }
                case .tableCell(let columnIndex):
                    tableColumn = columnIndex
                case .paragraph:
                    break
                @unknown default:
                    break
                }
            }

            if let tableIdentity {
                let cell = trimmedInlineSlice(attributed[range])
                if var pending = pendingTable, pending.identity == tableIdentity {
                    if pending.rowIdentity != tableRowIdentity {
                        pending.closeRow()
                        pending.rowIdentity = tableRowIdentity
                        pending.rowIsHeader = tableRowIsHeader
                    }
                    pending.place(cell, at: tableColumn)
                    pendingTable = pending
                } else {
                    flushTable()
                    var fresh = PendingTable(identity: tableIdentity, alignments: tableAlignments)
                    fresh.quoteDepth = quoteDepth
                    fresh.rowIdentity = tableRowIdentity
                    fresh.rowIsHeader = tableRowIsHeader
                    fresh.place(cell, at: tableColumn)
                    pendingTable = fresh
                }
                continue
            }
            flushTable()

            if isThematicBreak {
                nodes.append(MarkdownBlockNode(kind: .thematicBreak, quoteDepth: quoteDepth))
            } else if isCode {
                let code = rawText.hasSuffix("\n") ? String(rawText.dropLast()) : rawText
                nodes.append(MarkdownBlockNode(
                    kind: .codeBlock(languageHint: normalizedHint(codeHint), code: code),
                    quoteDepth: quoteDepth
                ))
            } else if let headerLevel {
                nodes.append(MarkdownBlockNode(
                    kind: .heading(level: headerLevel),
                    inline: inlineSlice(attributed[range]),
                    quoteDepth: quoteDepth
                ))
            } else if let listItemIdentity {
                let isContinuation = listItemIdentity == lastListItemIdentity
                var inline = inlineSlice(attributed[range])
                let marker: MarkdownBlockNode.ListMarker
                if innermostListIsOrdered == true, let ordinal = listItemOrdinal {
                    marker = .ordinal(ordinal)
                } else if !isContinuation, let checked = liftTaskCheckbox(from: &inline) {
                    marker = .task(checked: checked)
                } else {
                    marker = .bullet
                }
                nodes.append(MarkdownBlockNode(
                    kind: .listItem(
                        depth: max(listDepth, 1),
                        marker: marker,
                        isContinuation: isContinuation
                    ),
                    inline: inline,
                    quoteDepth: quoteDepth
                ))
                lastListItemIdentity = listItemIdentity
                continue
            } else {
                let slice = inlineSlice(attributed[range])
                if !slice.characters.isEmpty {
                    nodes.append(MarkdownBlockNode(
                        kind: .paragraph, inline: slice, quoteDepth: quoteDepth
                    ))
                }
            }
            lastListItemIdentity = nil
        }
        flushTable()

        if nodes.isEmpty, !text.isEmpty {
            return fallback(text)
        }
        return nodes
    }

    private static func fallback(_ text: String) -> [MarkdownBlockNode] {
        text.isEmpty ? [] : [MarkdownBlockNode(kind: .paragraph, inline: AttributedString(text))]
    }

    /// Inline text split into runs of `>` lines and the text between them, or
    /// nil when no line is quoted. Inline syntax drops every block construct,
    /// but a quote is how a reply cites the transcript, so it is the one block
    /// user text keeps. A quote run loses one level of markers, so nested
    /// quotes recurse; the blank lines that set a quote apart are dropped.
    private static func quoteSegments(_ text: String) -> [(text: String, isQuote: Bool)]? {
        func quoted(_ line: Substring) -> Substring? {
            let indent = line.prefix(while: { $0 == " " })
            guard indent.count <= 3 else { return nil }
            let rest = line.dropFirst(indent.count)
            guard rest.first == ">" else { return nil }
            let body = rest.dropFirst()
            return body.first == " " ? body.dropFirst() : body
        }
        var segments: [(lines: [Substring], isQuote: Bool)] = []
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let body = quoted(line)
            let isQuote = body != nil
            if segments.last?.isQuote == isQuote {
                segments[segments.count - 1].lines.append(body ?? line)
            } else {
                segments.append(([body ?? line], isQuote))
            }
        }
        guard segments.contains(where: \.isQuote) else { return nil }
        return segments.compactMap { segment in
            var lines = segment.lines[...]
            while lines.first?.allSatisfy(\.isWhitespace) == true { lines = lines.dropFirst() }
            while lines.last?.allSatisfy(\.isWhitespace) == true { lines = lines.dropLast() }
            return lines.isEmpty ? nil : (lines.joined(separator: "\n"), segment.isQuote)
        }
    }

    /// Strips a leading `[ ] ` / `[x] ` from a list item's text and returns
    /// its state, or nil when the item is not a task. GFM requires the box
    /// to open the item and to be followed by whitespace; a bare `[ ]` with
    /// nothing after it still counts, as it does on GitHub.
    private static func liftTaskCheckbox(from inline: inout AttributedString) -> Bool? {
        let characters = inline.characters
        var index = characters.startIndex
        guard index < characters.endIndex, characters[index] == "[" else { return nil }
        index = characters.index(after: index)
        guard index < characters.endIndex else { return nil }
        let checked: Bool
        switch characters[index] {
        case " ": checked = false
        case "x", "X": checked = true
        default: return nil
        }
        index = characters.index(after: index)
        guard index < characters.endIndex, characters[index] == "]" else { return nil }
        index = characters.index(after: index)
        if index < characters.endIndex {
            guard characters[index].isWhitespace else { return nil }
            index = characters.index(after: index)
        }
        inline.removeSubrange(characters.startIndex..<index)
        return checked
    }

    /// Drops the block-level intent (the styler supplies block presentation) but keeps
    /// inline intents, links, and image URLs for the styler to project.
    private static func inlineSlice(_ slice: AttributedSubstring) -> AttributedString {
        var copy = AttributedString(slice)
        copy.presentationIntent = nil
        return copy
    }

    /// A table cell's inline content without the padding spaces the pipe
    /// syntax carries around it. A literal tab or line break inside a cell
    /// becomes a space: a renderer lays cells out on tab stops, and a cell
    /// must not be able to move to the next column or row.
    private static func trimmedInlineSlice(_ slice: AttributedSubstring) -> AttributedString {
        let characters = slice.characters
        guard let first = characters.firstIndex(where: { !$0.isWhitespace }),
              let last = characters.lastIndex(where: { !$0.isWhitespace })
        else { return AttributedString() }
        var cell = inlineSlice(slice[first...last])
        var index = cell.characters.startIndex
        while index < cell.characters.endIndex {
            let character = cell.characters[index]
            if character == "\t" || character.isNewline {
                let next = cell.characters.index(after: index)
                cell.characters.replaceSubrange(index..<next, with: " ")
            }
            index = cell.characters.index(after: index)
        }
        return cell
    }

    private static func normalizedHint(_ hint: String?) -> String? {
        guard let trimmed = hint?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              !trimmed.isEmpty else { return nil }
        return trimmed
    }
}
