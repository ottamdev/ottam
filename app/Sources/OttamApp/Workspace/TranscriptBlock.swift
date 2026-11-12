import OttamMarkdownEngine
import Foundation

/// Stable identity for one materialized unit in the native transcript.
///
/// The outer item identity comes from the canonical transcript plan. A local
/// generation lets the Markdown projector add blocks without deriving identity
/// from the collection-view position.
struct TranscriptBlockID: Hashable, Sendable, CustomStringConvertible {
    let itemID: String
    let generation: UInt64

    var description: String { "\(itemID):\(generation)" }
}

/// Renderer-neutral input to the AppKit collection surface. Phase 2 uses one
/// literal text block per Markdown item; Phase 3 refines the text payload into
/// semantic Markdown blocks while keeping this identity boundary.
struct TranscriptBlock: Equatable, Identifiable {
    enum Content: Equatable {
        case text(Text)
        case card
    }

    struct Text: Equatable {
        var source: String
        let syntax: TranscriptMarkdownSyntax
        let container: TranscriptContainerStyle?
        let node: MarkdownBlockNode?
        /// A bubble with more than one paragraph (a quote and its reply) keeps
        /// them in one block, so one tile holds the whole message. `node` is
        /// nil then and `source` joins their text with newlines.
        var paragraphs: [MarkdownBlockNode] = []
    }

    let id: TranscriptBlockID
    let itemID: String
    let relation: TranscriptItem.Relation
    var content: Content
    /// The markdown block directly before this one in the same item, when
    /// both are blocks; what the item needs to carry a quote bar across the
    /// gap above it.
    var predecessor: MarkdownBlockSpacing.Neighbor? = nil

    /// What the layout spaces this block by; nil for cards and node-less text.
    var markdownNeighbor: MarkdownBlockSpacing.Neighbor? {
        guard case .text(let text) = content, let node = text.node else { return nil }
        return MarkdownBlockSpacing.Neighbor(node)
    }

    /// The space this block absorbs from the layout and draws itself, with
    /// the number of quote levels whose bars continue through it.
    func quoteContinuation(spacing: MarkdownBlockSpacing) -> (leadIn: CGFloat, depth: Int) {
        guard let predecessor, let neighbor = markdownNeighbor else { return (0, 0) }
        let leadIn = spacing.quoteLeadIn(from: predecessor, to: neighbor)
        guard leadIn > 0 else { return (0, 0) }
        return (leadIn, min(predecessor.quoteDepth, neighbor.quoteDepth))
    }
}

/// Sendable projection input for background parsing. Card payloads stay on the
/// main actor; the block projector needs only their identity and relation.
struct TranscriptProjectionItem: Equatable, Sendable {
    enum Content: Equatable, Sendable {
        case markdown(String, syntax: TranscriptMarkdownSyntax, container: TranscriptContainerStyle?)
        case card
    }

    let id: String
    let content: Content
    let relation: TranscriptItem.Relation

    init(_ item: TranscriptItem) {
        id = item.id
        relation = item.relation
        switch item.content {
        case .markdown(let source, let syntax, let container):
            content = .markdown(source, syntax: syntax, container: container)
        case .card:
            content = .card
        }
    }
}

/// Preserves block identity while transcript values refresh. The literal
/// skeleton intentionally has one block per transcript item, so live → finished
/// → persisted handoff inherits the assembler's stable item ID directly.
final class TranscriptBlockProjector: @unchecked Sendable {
    private struct ItemState {
        var source: String?
        var syntax: TranscriptMarkdownSyntax?
        var container: TranscriptContainerStyle?
        var nodes: [TranscriptBlock.Text]
        var IDs: [TranscriptBlockID]
        var tailSource: String
        var sealedNodeCount: Int
        var lastMutationStart: Int
        var lastAppendParsedBytes: Int
        var openFenceDelimiter: Character?
        var hasDeferredTail: Bool
    }

    private var states: [String: ItemState] = [:]
    private var nextGeneration: UInt64 = 1

    func project(_ items: [TranscriptItem]) -> [TranscriptBlock] {
        project(items.map(TranscriptProjectionItem.init))
    }

    func project(_ items: [TranscriptProjectionItem]) -> [TranscriptBlock] {
        var kept: [String: ItemState] = [:]
        var blocks: [TranscriptBlock] = []
        for item in items {
            switch item.content {
            case .markdown(let source, let syntax, let container):
                let previous = states[item.id]
                if let previous,
                   previous.source == source,
                   previous.syntax == syntax,
                   previous.container == container,
                   !previous.hasDeferredTail {
                    kept[item.id] = previous
                    blocks.append(contentsOf: Self.blocks(
                        itemID: item.id, relation: item.relation, nodes: previous.nodes, ids: previous.IDs
                    ))
                    continue
                }
                let projected = Self.projectText(source, syntax: syntax, container: container)
                var ids: [TranscriptBlockID] = []
                ids.reserveCapacity(projected.count)
                var prefix = 0
                while let previous,
                      prefix < previous.nodes.count,
                      prefix < projected.count,
                      previous.nodes[prefix] == projected[prefix] {
                    ids.append(previous.IDs[prefix])
                    prefix += 1
                }
                if let previous,
                   prefix < previous.nodes.count,
                   prefix < projected.count {
                    // The first changing block is the streaming/open tail.
                    ids.append(previous.IDs[prefix])
                    prefix += 1
                }
                while ids.count < projected.count { ids.append(mint(itemID: item.id)) }
                let state = ItemState(
                    source: source,
                    syntax: syntax,
                    container: container,
                    nodes: projected,
                    IDs: ids,
                    tailSource: source,
                    sealedNodeCount: 0,
                    lastMutationStart: 0,
                    lastAppendParsedBytes: source.utf8.count,
                    openFenceDelimiter: Self.unclosedFenceDelimiter(in: source),
                    hasDeferredTail: false
                )
                kept[item.id] = state
                blocks.append(contentsOf: Self.blocks(
                    itemID: item.id, relation: item.relation, nodes: projected, ids: ids
                ))
            case .card:
                let id = states[item.id]?.IDs.first ?? mint(itemID: item.id)
                kept[item.id] = ItemState(
                    source: nil,
                    syntax: nil,
                    container: nil,
                    nodes: [],
                    IDs: [id],
                    tailSource: "",
                    sealedNodeCount: 0,
                    lastMutationStart: 0,
                    lastAppendParsedBytes: 0,
                    openFenceDelimiter: nil,
                    hasDeferredTail: false
                )
                blocks.append(TranscriptBlock(
                    id: id, itemID: item.id, relation: item.relation, content: .card
                ))
            }
        }
        states = kept
        return blocks
    }

    func append(_ delta: String, to itemID: String, in items: inout [TranscriptItem]) -> Bool {
        guard !delta.isEmpty,
              let index = items.lastIndex(where: { $0.id == itemID }),
              case .markdown(var source, let syntax, let container) = items[index].content
        else { return false }
        source += delta
        items[index] = TranscriptItem(
            id: itemID,
            content: .markdown(source, syntax: syntax, container: container),
            relation: items[index].relation
        )
        if var state = states[itemID],
           state.source != nil,
           state.syntax == syntax,
           state.container == container {
            state.source = source
            let previousTailEndedInWhitespace = state.tailSource.last?.isWhitespace == true
            let previousTailEndedInNewline = state.tailSource.hasSuffix("\n")
            let previousTailRequiresHardBreak = state.tailSource.hasSuffix("  ")
                || state.tailSource.hasSuffix("\\")
            let previousTailHasOpenFenceBody = Self.openFenceBodyHasStarted(
                in: state.tailSource
            )
            let previousTailHasUnresolvedBlockPrefix = Self.hasUnresolvedBlockPrefix(
                in: state.tailSource
            )
            state.tailSource += delta
            if Self.shouldDeferUnstableMarkdownTail(state.tailSource) {
                // Keep the last stable projection visible while a delimiter is
                // still ambiguous. The canonical source continues to grow and
                // is reparsed as soon as the delimiter becomes conclusive.
                state.lastAppendParsedBytes = 0
                state.lastMutationStart = state.nodes.count
                state.hasDeferredTail = true
                states[itemID] = state
                return true
            }
            // Text withheld from the parser while a delimiter was ambiguous
            // must go through it once conclusive, even when the resolving
            // delta is a plain newline the shortcut would otherwise absorb.
            let previousTailWasDeferred = state.hasDeferredTail
            state.hasDeferredTail = false
            if !previousTailWasDeferred, Self.appendPlainTextWithoutParsing(
                delta,
                state: &state,
                syntax: syntax,
                container: container,
                previousTailEndedInWhitespace: previousTailEndedInWhitespace,
                previousTailEndedInNewline: previousTailEndedInNewline,
                previousTailRequiresHardBreak: previousTailRequiresHardBreak,
                previousTailHasOpenFenceBody: previousTailHasOpenFenceBody,
                previousTailHasUnresolvedBlockPrefix: previousTailHasUnresolvedBlockPrefix
            ) {
                state.lastAppendParsedBytes = 0
                if state.nodes.count - state.sealedNodeCount > 1 {
                    Self.advanceSealIfWorthwhile(
                        state: &state,
                        syntax: syntax,
                        container: container
                    )
                }
                states[itemID] = state
                return true
            }
            state.lastAppendParsedBytes = state.tailSource.utf8.count
            let tailNodes = Self.projectText(
                state.tailSource,
                syntax: syntax,
                container: container
            )
            let sealed = Array(state.nodes.prefix(state.sealedNodeCount))
            let oldTail = state.nodes.dropFirst(state.sealedNodeCount)
            var prefix = oldTail.elementsEqual(tailNodes.prefix(oldTail.count))
                ? min(oldTail.count, tailNodes.count)
                : Self.commonPrefixCount(oldTail, tailNodes[...])
            if !oldTail.isEmpty { prefix = min(prefix, oldTail.count - 1) }
            let retainedCount = state.sealedNodeCount + prefix
            var nextIDs = Array(state.IDs.prefix(retainedCount))
            if retainedCount < state.IDs.count, retainedCount < sealed.count + tailNodes.count {
                nextIDs.append(state.IDs[retainedCount])
            }
            let nextNodes = sealed + tailNodes
            while nextIDs.count < nextNodes.count { nextIDs.append(mint(itemID: itemID)) }
            state.nodes = nextNodes
            state.IDs = nextIDs
            state.lastMutationStart = retainedCount
            state.openFenceDelimiter = Self.unclosedFenceDelimiter(in: state.tailSource)
            Self.advanceSealIfWorthwhile(state: &state, syntax: syntax, container: container)
            states[itemID] = state
        }
        return true
    }

    func reset() {
        states.removeAll(keepingCapacity: true)
    }

    func debugUnsealedTailBytes(itemID: String) -> Int? {
        states[itemID]?.tailSource.utf8.count
    }

    func debugLastAppendParsedBytes(itemID: String) -> Int? {
        states[itemID]?.lastAppendParsedBytes
    }

    struct ProjectedUpdate {
        let startIndex: Int
        let blocks: [TranscriptBlock]
    }

    func projectedUpdate(
        itemID: String,
        relation: TranscriptItem.Relation
    ) -> ProjectedUpdate? {
        guard let state = states[itemID], !state.nodes.isEmpty else { return nil }
        let start = min(state.lastMutationStart, state.nodes.count)
        let blocks = Self.blocks(
            itemID: itemID, relation: relation, nodes: state.nodes, ids: state.IDs, from: start
        )
        return ProjectedUpdate(startIndex: start, blocks: blocks)
    }

    /// One block per node from `start`, the first carrying the item's own
    /// relation and every later one attaching to the block before it, which
    /// it also records as its predecessor.
    private static func blocks(
        itemID: String,
        relation: TranscriptItem.Relation,
        nodes: [TranscriptBlock.Text],
        ids: [TranscriptBlockID],
        from start: Int = 0
    ) -> [TranscriptBlock] {
        nodes.indices.dropFirst(start).map { index in
            var block = TranscriptBlock(
                id: ids[index],
                itemID: itemID,
                relation: index == 0 ? relation : .attachesToPrevious,
                content: .text(nodes[index])
            )
            if index > 0, let previous = nodes[index - 1].node {
                block.predecessor = MarkdownBlockSpacing.Neighbor(previous)
            }
            return block
        }
    }

    private func mint(itemID: String) -> TranscriptBlockID {
        defer { nextGeneration &+= 1 }
        return TranscriptBlockID(itemID: itemID, generation: nextGeneration)
    }

    private static func projectText(
        _ source: String,
        syntax: TranscriptMarkdownSyntax,
        container: TranscriptContainerStyle?
    ) -> [TranscriptBlock.Text] {
        let markdownSyntax: MarkdownSyntax = syntax == .full ? .full : .inline
        let provisionalFence = syntax == .full ? provisionalOpeningFence(in: source) : nil
        let parsedSource = provisionalFence?.prefix ?? source
        var nodes = MarkdownBlockTreeParser.nodes(from: parsedSource, syntax: markdownSyntax)
        if let provisionalFence {
            nodes.append(MarkdownBlockNode(
                kind: .codeBlock(languageHint: provisionalFence.languageHint, code: "")
            ))
        }
        guard !nodes.isEmpty else {
            return [.init(source: source, syntax: syntax, container: container, node: nil)]
        }
        // Quote bars in a bubble are drawn by the paragraphs path, so a lone
        // quoted node takes it too.
        if container == .bubble, nodes.count > 1 || nodes.contains(where: { $0.quoteDepth > 0 }) {
            return [.init(
                source: nodes.map(displaySource(for:)).joined(separator: "\n"),
                syntax: syntax,
                container: container,
                node: nil,
                paragraphs: nodes
            )]
        }
        return nodes.map { node in
            TranscriptBlock.Text(
                source: displaySource(for: node),
                syntax: syntax,
                container: container,
                node: node
            )
        }
    }

    /// Foundation's Markdown parser waits for the opening fence line's newline
    /// *and* a body line before producing a code block. During streaming,
    /// three line-leading delimiters are already conclusive, so publish a
    /// stable empty code block immediately — and keep it through the fence
    /// line's own newline — so the first body token fills that same block
    /// instead of minting a new one.
    private static func provisionalOpeningFence(
        in rawSource: String
    ) -> (prefix: String, languageHint: String?)? {
        let source = rawSource.hasSuffix("\n") ? String(rawSource.dropLast()) : rawSource
        guard !source.hasSuffix("\n") else { return nil }
        let lineStart = source.lastIndex(of: "\n").map { source.index(after: $0) }
            ?? source.startIndex
        let rawLine = source[lineStart...]
        let indentation = rawLine.prefix(while: { $0 == " " })
        guard indentation.count <= 3 else { return nil }
        let line = rawLine.dropFirst(indentation.count)
        guard let delimiter = line.first, delimiter == "`" || delimiter == "~" else { return nil }
        let run = line.prefix(while: { $0 == delimiter })
        guard run.count >= 3 else { return nil }
        let prefix = String(source[..<lineStart])
        guard unclosedFenceDelimiter(in: prefix) == nil else { return nil }
        let rawHint = line.dropFirst(run.count).trimmingCharacters(in: .whitespaces)
        let hint = rawHint.isEmpty ? nil : rawHint
        return (prefix, hint)
    }

    private static func displaySource(for node: MarkdownBlockNode) -> String {
        switch node.kind {
        case .paragraph, .heading:
            return renderedInlineSource(node.inline)
        case .listItem(let depth, let marker, let isContinuation):
            let prefix = isContinuation ? "" : MarkdownStyler.markerText(for: marker, depth: depth)
            return prefix + renderedInlineSource(node.inline)
        case .codeBlock(_, let code):
            return code
        case .thematicBreak:
            return ""
        case .table(let header, let rows, _):
            // One line per row, every cell led by a tab: the same characters
            // `TranscriptTableLayout.attributedString` lays out on the
            // measured stops (images as their alt text), so selection
            // offsets index both alike.
            return ([header].compactMap { $0 } + rows)
                .map { row in
                    row.map { "\t" + renderedInlineSource(TranscriptTableSource.flatteningImages($0)) }.joined()
                }
                .joined(separator: "\n")
        }
    }

    private static func renderedInlineSource(_ inline: AttributedString) -> String {
        inline.runs.reduce(into: "") { result, run in
            if run.imageURL != nil {
                result.append("\u{FFFC}")
            } else {
                result += String(inline[run.range].characters)
            }
        }
    }

    private static let sealThresholdBytes = 2_048

    /// Ordinary characters cannot alter block structure or close inline syntax.
    /// When the current inline run is also plain, extend the one open node
    /// directly and leave the parser asleep. Delimiters and newlines still take
    /// the conservative parse-and-reconcile path.
    private static func appendPlainTextWithoutParsing(
        _ delta: String,
        state: inout ItemState,
        syntax: TranscriptMarkdownSyntax,
        container: TranscriptContainerStyle?,
        previousTailEndedInWhitespace: Bool,
        previousTailEndedInNewline: Bool,
        previousTailRequiresHardBreak: Bool,
        previousTailHasOpenFenceBody: Bool,
        previousTailHasUnresolvedBlockPrefix: Bool
    ) -> Bool {
        guard !delta.isEmpty,
              let index = state.nodes.indices.last,
              let node = state.nodes[index].node
        else { return false }
        switch node.kind {
        case .codeBlock(let languageHint, var code):
            // A provisional block (fence line only) has no body line to
            // extend; its first token goes through the parser so leading
            // newlines are attributed exactly.
            guard !code.isEmpty,
                  previousTailHasOpenFenceBody,
                  let delimiter = state.openFenceDelimiter,
                  !delta.contains(delimiter)
            else { return false }
            if previousTailEndedInNewline { code += "\n" }
            code += delta
            if state.tailSource.hasSuffix("\n"), code.hasSuffix("\n") { code.removeLast() }
            let updatedNode = MarkdownBlockNode(
                kind: .codeBlock(languageHint: languageHint, code: code),
                quoteDepth: node.quoteDepth
            )
            state.nodes[index] = TranscriptBlock.Text(
                source: code,
                syntax: syntax,
                container: container,
                node: updatedNode
            )
            state.lastMutationStart = index
            return true
        case .paragraph, .heading:
            guard state.openFenceDelimiter == nil,
                  !previousTailHasUnresolvedBlockPrefix,
                  !delta.unicodeScalars.contains(where: { plainAppendDelimiters.contains($0) }),
                  plainBlockAppendIsSafe(
                      delta,
                      previousTailEndedInNewline: previousTailEndedInNewline,
                      previousTailRequiresHardBreak: previousTailRequiresHardBreak
                  ),
                  let run = node.inline.runs.last,
                  run.inlinePresentationIntent == nil,
                  run.link == nil,
                  run.imageURL == nil
            else { return false }
            var inline = node.inline
            let displayedDelta = normalizedSoftBreaks(
                in: delta,
                previousTailEndedInWhitespace: previousTailEndedInWhitespace,
                displayedTailEndedInWhitespace: node.inline.characters.last?.isWhitespace == true
            )
            inline.append(AttributedString(displayedDelta))
            let updatedNode = MarkdownBlockNode(
                kind: node.kind,
                inline: inline,
                quoteDepth: node.quoteDepth
            )
            state.nodes[index] = TranscriptBlock.Text(
                source: displaySource(for: updatedNode),
                syntax: syntax,
                container: container,
                node: updatedNode
            )
            state.lastMutationStart = index
            return true
        case .listItem, .thematicBreak, .table:
            return false
        }
    }

    private static let plainAppendDelimiters = CharacterSet(
        charactersIn: "\r\\`*_~[]()<>!#|&;"
    )

    /// Avoid publishing a transient literal representation of Markdown whose
    /// delimiter is not complete yet. A third line-leading backtick or tilde
    /// is already conclusive and is parsed immediately; only one/two-character
    /// fence prefixes and unmatched inline-code spans are deferred.
    private static func shouldDeferUnstableMarkdownTail(_ source: String) -> Bool {
        let paragraphStart = source.range(of: "\n\n", options: .backwards)?.upperBound
            ?? source.startIndex
        let tail = source[paragraphStart...]
        let lastLine = tail.split(separator: "\n", omittingEmptySubsequences: false).last ?? ""
        let trimmedLine = lastLine.drop(while: { $0 == " " || $0 == "\t" })
        if let delimiter = trimmedLine.first, delimiter == "`" || delimiter == "~" {
            let run = trimmedLine.prefix(while: { $0 == delimiter })
            if run.count < 3, run.count == trimmedLine.count { return true }
        }

        // Backticks inside an established fenced block are code contents, not
        // inline delimiters. Opening fences of three or more characters are
        // likewise conclusive before their language line receives a newline.
        if unclosedFenceDelimiter(in: String(tail)) != nil { return false }

        // A pipe row is a table's header only once its delimiter row has
        // landed; before that the parser would publish it as a paragraph and
        // flip it into a table one line later. Hold the paragraph while it is
        // a lone pipe line, bounded like the inline-code case below.
        if isUnfinishedPipeTable(tail), tail.utf16.count <= 512 { return true }

        var unmatchedRuns = Set<Int>()
        var index = tail.startIndex
        while index < tail.endIndex {
            if tail[index] == "\\" {
                index = tail.index(after: index)
                if index < tail.endIndex { index = tail.index(after: index) }
                continue
            }
            guard tail[index] == "`" else {
                index = tail.index(after: index)
                continue
            }
            let end = tail[index...].firstIndex(where: { $0 != "`" }) ?? tail.endIndex
            let length = tail.distance(from: index, to: end)
            if unmatchedRuns.contains(length) { unmatchedRuns.remove(length) }
            else { unmatchedRuns.insert(length) }
            index = end
        }
        guard !unmatchedRuns.isEmpty else { return false }
        // Never hide an unbounded malformed response. A later full-plan apply
        // still converges to the canonical Markdown if the stream settles first.
        return tail.utf16.count <= 512
    }

    /// A pipe paragraph at a moment the parser would misread it: its second
    /// line has not ended yet (a table if that line is a delimiter row, a
    /// paragraph otherwise, and only the newline tells), or its last line is
    /// a row that is only its opening pipe so far, which the parser publishes
    /// as a paragraph under the table until the first cell has content.
    private static func isUnfinishedPipeTable(_ paragraph: Substring) -> Bool {
        let lines = paragraph.split(separator: "\n", omittingEmptySubsequences: false)
        guard let first = lines.first, first.drop(while: { $0 == " " }).hasPrefix("|") else {
            return false
        }
        if lines.count == 1 { return true }
        if lines.count == 2 {
            // Still a table only while the second line could yet be a
            // delimiter row; the first other character makes it prose,
            // and there is nothing left to wait for.
            return lines[1].allSatisfy { "|:- \t".contains($0) }
        }
        let last = lines[lines.count - 1].drop(while: { $0 == " " })
        return last.hasPrefix("|") && last.dropFirst().allSatisfy { $0 == " " }
    }

    private static func plainBlockAppendIsSafe(
        _ delta: String,
        previousTailEndedInNewline: Bool,
        previousTailRequiresHardBreak: Bool
    ) -> Bool {
        if delta.contains("\n\n")
            || previousTailEndedInNewline && delta.hasPrefix("\n")
            || previousTailRequiresHardBreak && delta.hasPrefix("\n") {
            return false
        }
        let lines = delta.split(separator: "\n", omittingEmptySubsequences: false)
        for index in lines.indices where index > 0 || previousTailEndedInNewline {
            let line = String(lines[index])
            let trimmed = line.drop(while: { $0 == " " || $0 == "\t" })
            if line.hasPrefix("    ") || line.hasPrefix("\t")
                || trimmed.hasPrefix("# ") || trimmed.hasPrefix("> ")
                || trimmed.hasPrefix("- ") || trimmed.hasPrefix("+ ")
                || trimmed.hasPrefix("* ") || trimmed.hasPrefix("```")
                || trimmed.hasPrefix("~~~") || trimmed.hasPrefix("|")
                || isSetextUnderline(String(trimmed))
                || isListMarkerLine(String(trimmed)) {
                return false
            }
        }
        return true
    }

    /// Foundation's full Markdown parser projects soft line breaks as spaces
    /// and suppresses a trailing break until more text arrives. Mirror that
    /// visible result without reparsing the accumulated paragraph.
    private static func normalizedSoftBreaks(
        in delta: String,
        previousTailEndedInWhitespace: Bool,
        displayedTailEndedInWhitespace: Bool
    ) -> String {
        var result = delta.replacingOccurrences(of: "\n", with: " ")
        if delta.hasSuffix("\n"), result.hasSuffix(" ") { result.removeLast() }
        if previousTailEndedInWhitespace,
           !displayedTailEndedInWhitespace,
           delta.first?.isWhitespace != true,
           !result.isEmpty {
            result.insert(" ", at: result.startIndex)
        }
        return result
    }

    private static func isSetextUnderline(_ line: String) -> Bool {
        let compact = line.filter { !$0.isWhitespace }
        guard !compact.isEmpty else { return false }
        return compact.allSatisfy { $0 == "=" } || compact.allSatisfy { $0 == "-" }
    }

    private static func unclosedFenceDelimiter(in source: String) -> Character? {
        var delimiter: Character?
        for rawLine in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = rawLine.drop(while: { $0 == " " || $0 == "\t" })
            if let active = delimiter {
                if line.first == active,
                   line.prefix(while: { $0 == active }).count >= 3 {
                    delimiter = nil
                }
            } else {
                if line.hasPrefix("```") { delimiter = "`" }
                else if line.hasPrefix("~~~") { delimiter = "~" }
            }
        }
        return delimiter
    }

    private static func openFenceBodyHasStarted(in source: String) -> Bool {
        guard unclosedFenceDelimiter(in: source) != nil else { return false }
        var foundOpeningFence = false
        for line in source.split(separator: "\n", omittingEmptySubsequences: false) {
            if foundOpeningFence { return true }
            let trimmed = line.drop(while: { $0 == " " || $0 == "\t" })
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                foundOpeningFence = true
            }
        }
        return false
    }

    private static func hasUnresolvedBlockPrefix(in source: String) -> Bool {
        let line = source.split(separator: "\n", omittingEmptySubsequences: false).last ?? ""
        let trimmed = line.drop(while: { $0 == " " || $0 == "\t" })
        guard !trimmed.isEmpty else { return false }
        let compact = trimmed.filter { $0 != " " && $0 != "\t" }
        if compact.allSatisfy({ $0 == "#" }), compact.count <= 6 { return true }
        if compact.allSatisfy({ $0 == ">" }) { return true }
        if compact.allSatisfy({ $0 == "-" || $0 == "+" || $0 == "*" }) { return true }
        if compact == "`" || compact == "``" || compact == "~" || compact == "~~" {
            return true
        }
        let digits = compact.prefix(while: \.isNumber)
        guard !digits.isEmpty else { return false }
        let suffix = compact.dropFirst(digits.count)
        return suffix.isEmpty || suffix == "." || suffix == ")"
    }

    private static func commonPrefixCount(
        _ old: ArraySlice<TranscriptBlock.Text>,
        _ new: ArraySlice<TranscriptBlock.Text>
    ) -> Int {
        var count = 0
        while count < old.count, count < new.count,
              old[old.index(old.startIndex, offsetBy: count)]
                == new[new.index(new.startIndex, offsetBy: count)] {
            count += 1
        }
        return count
    }

    private static func advanceSealIfWorthwhile(
        state: inout ItemState,
        syntax: TranscriptMarkdownSyntax,
        container: TranscriptContainerStyle?
    ) {
        guard syntax == .full, state.tailSource.utf8.count > sealThresholdBytes else { return }
        let currentTailNodes = Array(state.nodes.dropFirst(state.sealedNodeCount))
        for boundary in sealCandidates(in: state.tailSource).reversed() {
            guard boundary > state.tailSource.startIndex else { continue }
            let sealedSource = String(state.tailSource[..<boundary])
            let suffixSource = String(state.tailSource[boundary...])
            let sealedNodes = projectText(sealedSource, syntax: syntax, container: container)
            guard !sealedNodes.isEmpty, sealedNodes.count < currentTailNodes.count else { continue }
            let suffixNodes = projectText(suffixSource, syntax: syntax, container: container)
            guard sealedNodes + suffixNodes == currentTailNodes else { continue }
            state.sealedNodeCount += sealedNodes.count
            state.tailSource = suffixSource
            return
        }
    }

    private static func sealCandidates(in source: String) -> [String.Index] {
        var inFence = false
        var previousLineIsSafeNeighbor = false
        var previousLineWasBlank = false
        var candidates: [String.Index] = []
        var lineStart = source.startIndex
        while lineStart < source.endIndex {
            let lineEnd = source[lineStart...].firstIndex(of: "\n")
                .map { source.index(after: $0) } ?? source.endIndex
            let line = source[lineStart..<lineEnd]
            let trimmed = line.drop(while: { $0 == " " || $0 == "\t" })
                .trimmingCharacters(in: .newlines)
            if trimmed.isEmpty {
                previousLineWasBlank = true
            } else {
                let isFenceLine = trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~")
                let startsAtColumnZero = line.first.map { $0 != " " && $0 != "\t" } ?? false
                let trimmedString = String(trimmed)
                if !inFence,
                   startsAtColumnZero,
                   (previousLineWasBlank && previousLineIsSafeNeighbor
                       || isListMarkerLine(trimmedString)) {
                    candidates.append(lineStart)
                    if candidates.count > 8 { candidates.removeFirst() }
                }
                if isFenceLine { inFence.toggle() }
                previousLineIsSafeNeighbor = startsAtColumnZero && !isListMarkerLine(trimmedString)
                previousLineWasBlank = false
            }
            lineStart = lineEnd
        }
        return candidates
    }

    private static func isListMarkerLine(_ line: String) -> Bool {
        if line.hasPrefix("- ") || line.hasPrefix("* ") || line.hasPrefix("+ ") { return true }
        let digits = line.prefix(while: \.isNumber)
        guard !digits.isEmpty else { return false }
        let rest = line.dropFirst(digits.count)
        return rest.hasPrefix(". ") || rest.hasPrefix(") ")
    }
}
