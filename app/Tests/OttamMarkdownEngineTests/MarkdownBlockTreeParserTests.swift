import Foundation
import Testing
@testable import OttamMarkdownEngine

@Suite("MarkdownBlockTreeParser")
struct MarkdownBlockTreeParserTests {
    private func kinds(_ text: String, syntax: MarkdownSyntax = .full) -> [MarkdownBlockNode.Kind] {
        MarkdownBlockTreeParser.nodes(from: text, syntax: syntax).map(\.kind)
    }

    /// A plain table cell: inline content with no intents.
    private func cell(_ text: String) -> AttributedString { AttributedString(text) }

    @Test("heading levels 1-6 surface with their text")
    func headingLevels() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "# One\n\n## Two\n\n#### Four")
        #expect(nodes.map(\.kind) == [.heading(level: 1), .heading(level: 2), .heading(level: 4)])
        #expect(String(nodes[0].inline.characters) == "One")
        #expect(String(nodes[2].inline.characters) == "Four")
    }

    @Test("paragraphs split and carry inline content")
    func paragraphs() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "First para.\n\nSecond para.")
        #expect(nodes.map(\.kind) == [.paragraph, .paragraph])
        #expect(String(nodes[1].inline.characters) == "Second para.")
    }

    @Test("nested unordered list depth and markers")
    func nestedUnorderedList() {
        let markdown = """
        - top
            - nested
        - top two
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.kind) == [
            .listItem(depth: 1, marker: .bullet, isContinuation: false),
            .listItem(depth: 2, marker: .bullet, isContinuation: false),
            .listItem(depth: 1, marker: .bullet, isContinuation: false)
        ])
    }

    @Test("ordered list carries ordinals")
    func orderedList() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "1. first\n2. second")
        #expect(nodes.map(\.kind) == [
            .listItem(depth: 1, marker: .ordinal(1), isContinuation: false),
            .listItem(depth: 1, marker: .ordinal(2), isContinuation: false)
        ])
    }

    @Test("continuation paragraph inside a list item is flagged")
    func listItemContinuation() {
        let markdown = """
        - item text

            continuation paragraph

        - next item
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        let continuations = nodes.compactMap { node -> Bool? in
            guard case .listItem(_, _, let isContinuation) = node.kind else { return nil }
            return isContinuation
        }
        #expect(continuations == [false, true, false])
    }

    @Test("fenced code block keeps language hint and trims trailing newline")
    func codeBlock() {
        let markdown = """
        ```swift
        let x = 1
        print(x)
        ```
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.kind) == [.codeBlock(languageHint: "swift", code: "let x = 1\nprint(x)")])
    }

    @Test("unhinted fence normalizes to nil hint")
    func codeBlockNoHint() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "```\nplain\n```")
        #expect(nodes.map(\.kind) == [.codeBlock(languageHint: nil, code: "plain")])
    }

    @Test("blockquote and thematic break")
    func quoteAndBreak() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "> quoted\n\n---\n\nafter")
        #expect(nodes.map(\.kind) == [.paragraph, .thematicBreak, .paragraph])
        #expect(nodes.map(\.quoteDepth) == [1, 0, 0])
        #expect(String(nodes[0].inline.characters) == "quoted")
    }

    @Test("inline text keeps its quotes and its line breaks")
    func inlineQuotes() {
        let text = "> first *line*\n> second\n\none\ntwo\n> > nested\n\n> again"
        let nodes = MarkdownBlockTreeParser.nodes(from: text, syntax: .inline)
        #expect(nodes.map { String($0.inline.characters) } == ["first line\nsecond", "one\ntwo", "nested", "again"])
        #expect(nodes.map(\.quoteDepth) == [1, 0, 2, 1])
        #expect(nodes[0].inline.runs.contains { $0.inlinePresentationIntent?.contains(.emphasized) == true })
    }

    @Test("inline text without a quote is one paragraph")
    func inlineWithoutQuotes() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "a > b\n\n    > code-indented", syntax: .inline)
        #expect(nodes.map(\.quoteDepth) == [0])
    }

    @Test("a quote contains its blocks instead of flattening them")
    func quoteContainment() {
        let markdown = """
        > ## quoted heading
        >
        > - quoted item
        >
        > ```swift
        > let x = 1
        > ```
        >
        > | a |
        > | - |
        > | 1 |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.kind) == [
            .heading(level: 2),
            .listItem(depth: 1, marker: .bullet, isContinuation: false),
            .codeBlock(languageHint: "swift", code: "let x = 1"),
            .table(header: [cell("a")], rows: [[cell("1")]], alignments: [.left])
        ])
        #expect(nodes.allSatisfy { $0.quoteDepth == 1 })
    }

    @Test("task items lift their checkbox out of the text")
    func taskItems() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "- [ ] open\n- [x] done\n- [X] also done\n- [ ]\n- [y] not a task\n- [ ]no space")
        #expect(nodes.map(\.kind) == [
            .listItem(depth: 1, marker: .task(checked: false), isContinuation: false),
            .listItem(depth: 1, marker: .task(checked: true), isContinuation: false),
            .listItem(depth: 1, marker: .task(checked: true), isContinuation: false),
            .listItem(depth: 1, marker: .task(checked: false), isContinuation: false),
            .listItem(depth: 1, marker: .bullet, isContinuation: false),
            .listItem(depth: 1, marker: .bullet, isContinuation: false),
        ])
        #expect(String(nodes[0].inline.characters) == "open")
        #expect(String(nodes[1].inline.characters) == "done")
        #expect(String(nodes[3].inline.characters) == "")
        #expect(String(nodes[4].inline.characters) == "[y] not a task")
        #expect(String(nodes[5].inline.characters) == "[ ]no space")
    }

    @Test("a continuation paragraph starting with brackets is not a task")
    func taskContinuationUntouched() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "- [ ] item\n\n    [x] literal in a continuation")
        // A continuation draws no marker, so its marker is the plain default.
        #expect(nodes.map(\.kind) == [
            .listItem(depth: 1, marker: .task(checked: false), isContinuation: false),
            .listItem(depth: 1, marker: .bullet, isContinuation: true),
        ])
        #expect(String(nodes[1].inline.characters) == "[x] literal in a continuation")
    }

    @Test("nested quotes carry their own depth")
    func nestedQuoteDepth() {
        let markdown = """
        > outer
        >
        > > inner
        > >
        > > > deepest

        after
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.quoteDepth) == [1, 2, 3, 0])
        #expect(nodes.allSatisfy { $0.kind == .paragraph })
    }

    @Test("table assembles a grid with its header row separated out")
    func tableAssembly() {
        let markdown = """
        | a | b |
        | - | - |
        | 1 | 2 |
        | 3 | 4 |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.kind) == [
            .table(
                header: [cell("a"), cell("b")],
                rows: [[cell("1"), cell("2")], [cell("3"), cell("4")]],
                alignments: [.left, .left]
            )
        ])
    }

    @Test("the delimiter row sets each column's alignment")
    func tableAlignments() {
        let markdown = """
        | a | b | c | d |
        |:--|:-:|--:|---|
        | 1 | 2 | 3 | 4 |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        guard case .table(_, _, let alignments)? = nodes.first?.kind else {
            Issue.record("expected a table")
            return
        }
        #expect(alignments == [.left, .center, .right, .left])
    }

    @Test("cells keep emphasis, code and links as inline runs")
    func tableCellsKeepInlineIntents() {
        let markdown = """
        | Name | Count |
        |------|-------|
        | `code` | **12** |
        | [link](https://example.com) | plain |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        guard case .table(_, let rows, _)? = nodes.first?.kind else {
            Issue.record("expected a table")
            return
        }
        #expect(rows[0][0].runs.first?.inlinePresentationIntent?.contains(.code) == true)
        #expect(rows[0][1].runs.first?.inlinePresentationIntent?.contains(.stronglyEmphasized) == true)
        #expect(rows[1][0].runs.first?.link == URL(string: "https://example.com"))
        #expect(String(rows[1][1].characters) == "plain")
        #expect(rows.flatMap { $0 }.allSatisfy { cell in cell.runs.allSatisfy { $0.presentationIntent == nil } })
    }

    @Test("a short row pads to the column count and a long row drops the surplus")
    func raggedTableRows() {
        let markdown = """
        | a | b | c |
        |---|---|---|
        | 1 |
        | 1 | 2 | 3 | 4 |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        guard case .table(_, let rows, _)? = nodes.first?.kind else {
            Issue.record("expected a table")
            return
        }
        #expect(rows.map { $0.map { String($0.characters) } } == [["1", "", ""], ["1", "2", "3"]])
    }

    @Test("consecutive tables stay separate blocks")
    func adjacentTables() {
        let markdown = """
        | a |
        | - |
        | 1 |

        prose between

        | b |
        | - |
        | 2 |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.kind) == [
            .table(header: [cell("a")], rows: [[cell("1")]], alignments: [.left]),
            .paragraph,
            .table(header: [cell("b")], rows: [[cell("2")]], alignments: [.left])
        ])
    }

    @Test("a table with only a header row parses as a header and no rows")
    func headerOnlyTable() {
        let markdown = """
        | a | b |
        | - | - |
        """
        let nodes = MarkdownBlockTreeParser.nodes(from: markdown)
        #expect(nodes.map(\.kind) == [
            .table(header: [cell("a"), cell("b")], rows: [], alignments: [.left, .left])
        ])
    }

    @Test("inline code and emphasis intents survive the walk")
    func inlineIntentsPreserved() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "mix `code` and **bold**")
        #expect(nodes.count == 1)
        let inline = nodes[0].inline
        let codeRuns = inline.runs.filter { $0.inlinePresentationIntent?.contains(.code) == true }
        let boldRuns = inline.runs.filter { $0.inlinePresentationIntent?.contains(.stronglyEmphasized) == true }
        #expect(!codeRuns.isEmpty)
        #expect(!boldRuns.isEmpty)
        // Block-level intent must be stripped — the styler owns block presentation.
        #expect(inline.runs.allSatisfy { $0.presentationIntent == nil })
    }

    @Test("links survive the walk")
    func linksPreserved() {
        let nodes = MarkdownBlockTreeParser.nodes(
            from: "see [web](https://example.com/docs) and [local](docs/architecture/README.md)"
        )
        let links = nodes[0].inline.runs.compactMap(\.link)
        #expect(links == [
            URL(string: "https://example.com/docs"),
            URL(string: "docs/architecture/README.md"),
        ])
    }

    @Test("image URLs survive the walk")
    func imagesPreserved() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "shot: ![alt text](https://example.com/pic.png)")
        let images = nodes.flatMap { $0.inline.runs.compactMap(\.imageURL) }
        #expect(images == [URL(string: "https://example.com/pic.png")])
    }

    @Test("empty input yields no nodes; whitespace-only falls back")
    func emptyAndFallback() {
        #expect(MarkdownBlockTreeParser.nodes(from: "").isEmpty)
        let nodes = MarkdownBlockTreeParser.nodes(from: "   ")
        #expect(nodes.count == 1)
        #expect(nodes[0].kind == .paragraph)
    }

    @Test("inline syntax preserves newlines instead of reflowing")
    func inlineSyntaxPreservesNewlines() {
        let nodes = MarkdownBlockTreeParser.nodes(from: "line one\nline two", syntax: .inline)
        let joined = nodes.map { String($0.inline.characters) }.joined()
        #expect(joined.contains("\n"))
    }
}
