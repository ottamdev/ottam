import AppKit
import Testing
@testable import OttamMarkdownEngine

@Suite("MarkdownStyler")
struct MarkdownStylerTests {
    private static func makeTheme() -> MarkdownTheme {
        MarkdownTheme(
            body: NSFont.systemFont(ofSize: 13),
            code: NSFont.monospacedSystemFont(ofSize: 12, weight: .regular),
            heading1: NSFont.boldSystemFont(ofSize: 20),
            heading2: NSFont.boldSystemFont(ofSize: 17),
            heading3: NSFont.boldSystemFont(ofSize: 13),
            heading4: NSFont.boldSystemFont(ofSize: 12),
            textColor: .labelColor,
            secondaryTextColor: .secondaryLabelColor,
            inlineCodeBackground: NSColor.labelColor.withAlphaComponent(0.07),
            codeBackground: .textBackgroundColor,
            codeBorder: .separatorColor,
            quoteBar: .tertiaryLabelColor,
            ruleColor: .separatorColor,
            syntaxColors: [
                .keyword: .systemOrange,
                .string: .systemGreen,
                .comment: .systemGray
            ],
            signature: "test-theme"
        )
    }

    private let styler = MarkdownStyler(theme: Self.makeTheme())

    private func blockRanges(_ styled: NSAttributedString) -> [(role: MarkdownBlockRole, id: Int, range: NSRange)] {
        var result: [(MarkdownBlockRole, Int, NSRange)] = []
        styled.enumerateAttribute(
            MarkdownAttribute.blockID,
            in: NSRange(location: 0, length: styled.length)
        ) { value, range, _ in
            guard let id = (value as? NSNumber)?.intValue else { return }
            let role = MarkdownBlockRole(
                attributeValue: styled.attribute(MarkdownAttribute.blockRole, at: range.location, effectiveRange: nil)
            )
            guard let role else { return }
            result.append((role, id, range))
        }
        return result
    }

    @Test("blocks carry contiguous role and ID ranges covering the whole string")
    func semanticRangesCoverEverything() {
        let styled = styler.attributedString(from: "# Title\n\npara one\n\npara two")
        let blocks = blockRanges(styled)
        #expect(blocks.map(\.role) == [.heading, .paragraph, .paragraph])
        #expect(blocks.map(\.id) == [0, 1, 2])
        // Contiguous: each block starts where the previous ended, first at 0, last at end.
        #expect(blocks.first?.range.location == 0)
        for (previous, next) in zip(blocks, blocks.dropFirst()) {
            #expect(previous.range.location + previous.range.length == next.range.location)
        }
        #expect(blocks.last.map { $0.range.location + $0.range.length } == styled.length)
    }

    @Test("blockIDs stay stable when a document grows by appending")
    func blockIDStability() {
        let prefix = "# Title\n\nfirst paragraph"
        let grown = prefix + "\n\nsecond paragraph"
        let before = blockRanges(styler.attributedString(from: prefix))
        let after = blockRanges(styler.attributedString(from: grown))
        #expect(after.count == before.count + 1)
        for (b, a) in zip(before, after) {
            #expect(b.role == a.role)
            #expect(b.id == a.id)
        }
    }

    @Test("list items get hanging indents that deepen with nesting")
    func listHangingIndents() {
        let styled = styler.attributedString(from: "- top\n    - nested deeper item")
        let blocks = blockRanges(styled)
        #expect(blocks.map(\.role) == [.listItem, .listItem])

        func paragraphStyle(at location: Int) -> NSParagraphStyle? {
            styled.attribute(.paragraphStyle, at: location, effectiveRange: nil) as? NSParagraphStyle
        }
        let top = paragraphStyle(at: blocks[0].range.location)
        let nested = paragraphStyle(at: blocks[1].range.location)
        #expect(top != nil && nested != nil)
        // Hanging: wrapped lines indent past the marker on the first line.
        #expect(top!.headIndent > top!.firstLineHeadIndent)
        #expect(nested!.headIndent > nested!.firstLineHeadIndent)
        // Nesting pushes both indents right.
        #expect(nested!.firstLineHeadIndent > top!.firstLineHeadIndent)
        // Depth is recorded for decorations/serialization.
        let depth = styled.attribute(MarkdownAttribute.listDepth, at: blocks[1].range.location, effectiveRange: nil) as? NSNumber
        #expect(depth?.intValue == 2)
    }

    @Test("code blocks remain selectable attributed text with syntax metadata")
    func codeBlockStyling() {
        let theme = Self.makeTheme()
        let styled = styler.attributedString(from: "```swift\nlet x = 1\nlet y = 2\n```")
        let blocks = blockRanges(styled)
        #expect(blocks.map(\.role) == [.codeBlock])
        #expect(styled.string == "let x = 1\nlet y = 2")
        let location = blocks[0].range.location
        #expect(styled.attribute(.attachment, at: location, effectiveRange: nil) == nil)
        #expect((styled.attribute(.font, at: location, effectiveRange: nil) as? NSFont)?.isFixedPitch == true)
        let language = styled.attribute(MarkdownAttribute.codeLanguage, at: location, effectiveRange: nil) as? String
        #expect(language == "swift")
        let style = styled.attribute(.paragraphStyle, at: location, effectiveRange: nil) as? NSParagraphStyle
        let lineHeight = ceil(theme.code.ascender - theme.code.descender + theme.code.leading)
        #expect(style?.lineBreakMode == .byClipping)
        #expect(style?.lineSpacing == 2)
        #expect(style?.minimumLineHeight == lineHeight)
        #expect(style?.maximumLineHeight == lineHeight)
        #expect(style?.paragraphSpacing == theme.metrics.paragraphSpacing)
    }

    @Test("code block separators preserve distinct semantic ranges")
    func codeBlockSeparator() {
        let styled = styler.attributedString(from: "```\ncode\n```\n\nafter")
        #expect(styled.string == "code\nafter")
        #expect(blockRanges(styled).map(\.role) == [.codeBlock, .paragraph])
    }

    @Test("tables project as selectable text rather than transcript attachments")
    func tableStyling() {
        let styled = styler.attributedString(from: "| A | B |\n|---|---|\n| 1 | 2 |")
        #expect(blockRanges(styled).map(\.role) == [.table])
        #expect(styled.string.contains("A │ B"))
        #expect(styled.attribute(.attachment, at: 0, effectiveRange: nil) == nil)
    }

    @Test("inline code run gets code font, background, and inline role")
    func inlineCode() {
        let styled = styler.attributedString(from: "before `mid` after")
        var found = false
        styled.enumerateAttribute(
            MarkdownAttribute.inlineRole,
            in: NSRange(location: 0, length: styled.length)
        ) { value, range, _ in
            guard let number = value as? NSNumber,
                  number.intValue == MarkdownInlineRole.code.rawValue else { return }
            found = true
            #expect((styled.string as NSString).substring(with: range) == "mid")
            let font = styled.attribute(.font, at: range.location, effectiveRange: nil) as? NSFont
            #expect(font?.isFixedPitch == true)
            #expect(styled.attribute(.backgroundColor, at: range.location, effectiveRange: nil) != nil)
        }
        #expect(found)
    }

    @Test("bold and italic map to font traits")
    func emphasisTraits() {
        let styled = styler.attributedString(from: "**bold** and *italic*")
        let text = styled.string as NSString
        let boldFont = styled.attribute(.font, at: text.range(of: "bold").location, effectiveRange: nil) as? NSFont
        let italicFont = styled.attribute(.font, at: text.range(of: "italic").location, effectiveRange: nil) as? NSFont
        #expect(boldFont.map { NSFontManager.shared.traits(of: $0).contains(.boldFontMask) } == true)
        #expect(italicFont.map { NSFontManager.shared.traits(of: $0).contains(.italicFontMask) } == true)
    }

    @Test("links carry the .link attribute")
    func links() {
        let styled = styler.attributedString(from: "see [docs](https://example.com)")
        var links: [URL] = []
        styled.enumerateAttribute(.link, in: NSRange(location: 0, length: styled.length)) { value, _, _ in
            if let url = value as? URL { links.append(url) }
        }
        #expect(links == [URL(string: "https://example.com")])
    }

    @Test("image runs carry the image inline role")
    func imageRuns() {
        let styled = styler.attributedString(from: "![alt](https://example.com/i.png)")
        var roles: [Int] = []
        styled.enumerateAttribute(
            MarkdownAttribute.inlineRole,
            in: NSRange(location: 0, length: styled.length)
        ) { value, _, _ in
            if let number = value as? NSNumber { roles.append(number.intValue) }
        }
        #expect(roles.contains(MarkdownInlineRole.image.rawValue))
    }

    @Test("heading fonts scale by level")
    func headingFonts() {
        let styled = styler.attributedString(from: "# Big\n\n### Small")
        let text = styled.string as NSString
        let h1 = styled.attribute(.font, at: text.range(of: "Big").location, effectiveRange: nil) as? NSFont
        let h3 = styled.attribute(.font, at: text.range(of: "Small").location, effectiveRange: nil) as? NSFont
        #expect(h1?.pointSize == 20)
        #expect(h3?.pointSize == 13)
    }

    @Test("consecutive list items are tighter than paragraphs")
    func listRhythm() {
        let styled = styler.attributedString(from: "- one\n- two")
        let style = styled.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        let theme = Self.makeTheme()
        #expect(style?.paragraphSpacing == theme.metrics.listParagraphSpacing)
    }

    @Test("a quoted paragraph keeps its role and gains a depth and an inset")
    func blockquote() {
        let theme = Self.makeTheme()
        let styled = styler.attributedString(from: "> quoted words")
        let blocks = blockRanges(styled)
        #expect(blocks.map(\.role) == [.paragraph])
        #expect(
            (styled.attribute(MarkdownAttribute.quoteDepth, at: 0, effectiveRange: nil)
                as? NSNumber)?.intValue == 1
        )
        let style = styled.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        #expect(style?.headIndent == theme.metrics.quoteInset)
        #expect(
            styled.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor
                == theme.secondaryTextColor
        )
    }

    @Test("quoting composes with the contained block's own indent")
    func quotedListKeepsItsHangingIndent() {
        let theme = Self.makeTheme()
        let plain = styler.attributedString(from: "- item")
        let quoted = styler.attributedString(from: "> - item")

        func headIndent(_ styled: NSAttributedString) -> CGFloat? {
            (styled.attribute(.paragraphStyle, at: 0, effectiveRange: nil)
                as? NSParagraphStyle)?.headIndent
        }
        #expect(blockRanges(quoted).map(\.role) == [.listItem])
        // The quote inset stacks on the marker hanging indent rather than
        // replacing it — that is what the old `.blockquote` kind could not do.
        #expect(headIndent(quoted) == (headIndent(plain) ?? 0) + theme.metrics.quoteInset)
    }

    @Test("nested quotes stack one inset per level")
    func nestedQuoteInsets() {
        let theme = Self.makeTheme()
        let styled = styler.attributedString(from: "> > deep")
        #expect(
            (styled.attribute(MarkdownAttribute.quoteDepth, at: 0, effectiveRange: nil)
                as? NSNumber)?.intValue == 2
        )
        let style = styled.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        #expect(style?.headIndent == theme.metrics.quoteInset * 2)
    }

    @Test("block spacing opens a section break above a heading and stays tight below it")
    func blockSpacingHeadings() {
        let spacing = MarkdownBlockSpacing()
        #expect(spacing.gap(from: .paragraph, to: .heading) == spacing.beforeHeading)
        #expect(spacing.gap(from: .listItem, to: .heading) == spacing.beforeHeading)
        #expect(spacing.gap(from: .codeBlock, to: .heading) == spacing.beforeHeading)
        // Stacked headings and the text under a heading share the tight gap.
        #expect(spacing.gap(from: .heading, to: .heading) == spacing.afterHeading)
        #expect(spacing.gap(from: .heading, to: .paragraph) == spacing.afterHeading)
        #expect(spacing.gap(from: .heading, to: .listItem) == spacing.afterHeading)
        #expect(spacing.gap(from: .heading, to: .codeBlock) == spacing.afterHeading)
    }

    @Test("block spacing keeps lists tighter than prose and re-inflates after them")
    func blockSpacingListsAndTiles() {
        let spacing = MarkdownBlockSpacing()
        #expect(spacing.gap(from: .paragraph, to: .paragraph) == spacing.paragraph)
        #expect(spacing.gap(from: .listItem, to: .listItem) == spacing.listItem)
        #expect(spacing.listItem < spacing.paragraph)
        #expect(spacing.gap(from: .paragraph, to: .listItem) == spacing.paragraph)
        #expect(spacing.gap(from: .listItem, to: .paragraph) == spacing.tile)
        for tile in [MarkdownBlockRole.codeBlock, .table, .thematicBreak] {
            #expect(spacing.gap(from: .paragraph, to: tile) == spacing.tile)
            #expect(spacing.gap(from: tile, to: .paragraph) == spacing.tile)
            #expect(spacing.gap(from: .listItem, to: tile) == spacing.tile)
        }
    }

    @Test("a top-level marker change opens a new list; nesting and quoting do not")
    func neighborGaps() {
        let spacing = MarkdownBlockSpacing()
        let bullet = MarkdownBlockSpacing.Neighbor(role: .listItem, listDepth: 1)
        let ordinal = MarkdownBlockSpacing.Neighbor(role: .listItem, listDepth: 1, listIsOrdered: true)
        let nestedOrdinal = MarkdownBlockSpacing.Neighbor(role: .listItem, listDepth: 2, listIsOrdered: true)
        #expect(spacing.gap(from: bullet, to: ordinal) == spacing.tile)
        #expect(spacing.gap(from: ordinal, to: bullet) == spacing.tile)
        #expect(spacing.gap(from: bullet, to: bullet) == spacing.listItem)
        #expect(spacing.gap(from: bullet, to: nestedOrdinal) == spacing.listItem)
        #expect(spacing.gap(from: nestedOrdinal, to: bullet) == spacing.listItem)

        let nodes = MarkdownBlockTreeParser.nodes(from: "- a\n- [ ] b\n\n1. c")
        let neighbors = nodes.map(MarkdownBlockSpacing.Neighbor.init)
        #expect(neighbors[0].listIsOrdered == false && neighbors[1].listIsOrdered == false)
        #expect(spacing.gap(from: neighbors[0], to: neighbors[1]) == spacing.listItem)
        #expect(spacing.gap(from: neighbors[1], to: neighbors[2]) == spacing.tile)
    }

    @Test("quoted blocks hand their gap to the block as a lead-in")
    func quoteLeadIn() {
        let spacing = MarkdownBlockSpacing()
        let quoted = MarkdownBlockSpacing.Neighbor(role: .paragraph, quoteDepth: 1)
        let plain = MarkdownBlockSpacing.Neighbor(role: .paragraph)
        #expect(spacing.quoteLeadIn(from: quoted, to: quoted) == spacing.paragraph)
        #expect(spacing.quoteLeadIn(from: quoted, to: .init(role: .listItem, quoteDepth: 2, listDepth: 1)) == spacing.paragraph)
        #expect(spacing.quoteLeadIn(from: plain, to: quoted) == 0)
        #expect(spacing.quoteLeadIn(from: quoted, to: plain) == 0)
        // A quoted fence draws no bar, so the bar breaks there and the gap stays.
        let quotedFence = MarkdownBlockSpacing.Neighbor(role: .codeBlock, quoteDepth: 1)
        #expect(spacing.quoteLeadIn(from: quoted, to: quotedFence) == 0)
        #expect(spacing.quoteLeadIn(from: quotedFence, to: quoted) == 0)
    }

    @Test("list markers change glyph by depth and share one column for ordinals")
    func listMarkers() {
        #expect(MarkdownStyler.markerText(for: .bullet, depth: 1) == "• ")
        #expect(MarkdownStyler.markerText(for: .bullet, depth: 2) == "◦ ")
        #expect(MarkdownStyler.markerText(for: .bullet, depth: 3) == "▪ ")
        #expect(MarkdownStyler.markerText(for: .task(checked: false), depth: 1) == "☐ ")
        #expect(MarkdownStyler.markerText(for: .task(checked: true), depth: 1) == "☑ ")

        let styled = styler.attributedString(from: "1. one\n2. two\n3. three\n4. four\n5. five\n6. six\n7. seven\n8. eight\n9. nine\n10. ten")
        let blocks = blockRanges(styled)
        func headIndent(_ index: Int) -> CGFloat? {
            (styled.attribute(.paragraphStyle, at: blocks[index].range.location, effectiveRange: nil) as? NSParagraphStyle)?.headIndent
        }
        #expect(headIndent(0) == headIndent(9))
        // Markers are padded out to a two-digit column on their trailing
        // space, so the one-digit marker carries the larger pad. (A two-digit
        // marker can still carry a little: the column is measured on "00.",
        // and "1" is narrower than "0" in a proportional face.)
        let oneDigitPad = styled.attribute(.kern, at: blocks[0].range.location + 2, effectiveRange: nil) as? CGFloat ?? 0
        let twoDigitPad = styled.attribute(.kern, at: blocks[9].range.location + 3, effectiveRange: nil) as? CGFloat ?? 0
        #expect(oneDigitPad > 0)
        #expect(twoDigitPad < oneDigitPad)
    }

    @Test("a finished task dims its text; an open one does not")
    func taskDimming() {
        let styled = styler.attributedString(from: "- [x] done\n- [ ] open")
        let text = styled.string as NSString
        #expect(text.hasPrefix("☑ done\n☐ open"))
        let done = styled.attribute(.foregroundColor, at: text.range(of: "done").location, effectiveRange: nil) as? NSColor
        let open = styled.attribute(.foregroundColor, at: text.range(of: "open").location, effectiveRange: nil) as? NSColor
        #expect(done == .secondaryLabelColor)
        #expect(open == .labelColor)
    }

    @Test("inline code scales with the run it sits in")
    func inlineCodeRelativeToRun() {
        let theme = Self.makeTheme()
        let styled = styler.attributedString(from: "# Fix `loadView()`\n\nCall `loadView()` here")
        let text = styled.string as NSString
        let inHeading = styled.attribute(.font, at: text.range(of: "loadView()").location, effectiveRange: nil) as? NSFont
        let inProse = styled.attribute(.font, at: text.range(of: "loadView()", options: .backwards).location, effectiveRange: nil) as? NSFont
        // 20 × 12/13 = 18.5 → 18; the code face is kept.
        #expect(inHeading?.pointSize == 18)
        #expect(inHeading?.fontName == theme.code.fontName)
        #expect(inProse?.pointSize == theme.code.pointSize)
    }

    @Test("every block node exposes the role the styler stamps")
    func nodeRoleMatchesStampedRole() {
        let nodes = MarkdownBlockTreeParser.nodes(
            from: "# H\n\npara\n\n- item\n\n```\ncode\n```\n\n---\n\n| a |\n|---|\n| b |",
            syntax: .full
        )
        let styled = styler.attributedString(from: nodes)
        #expect(blockRanges(styled).map(\.role) == nodes.map(\.role))
        #expect(nodes.map(\.role) == [.heading, .paragraph, .listItem, .codeBlock, .thematicBreak, .table])
    }
}
