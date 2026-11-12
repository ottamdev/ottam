import AppKit
import OttamBackendClient
import OttamMarkdownEngine
import Testing
@testable import OttamApp

@MainActor
private final class HeldTranscriptBatchUpdater {
    private(set) var callCount = 0
    private var completions: [(Bool) -> Void] = []

    func call(
        updates: @escaping () -> Void,
        completion: @escaping (Bool) -> Void
    ) {
        callCount += 1
        updates()
        completions.append(completion)
    }

    func finish(_ succeeded: Bool, at index: Int = 0) {
        completions[index](succeeded)
    }
}

/// A clock a test moves by hand, so an interval can pass without sleeping.
private final class ManualClock {
    var now = Date()

    func advance(_ interval: TimeInterval) {
        now = now.addingTimeInterval(interval)
    }
}

@Suite("Native transcript renderer")
@MainActor
struct NativeTranscriptRendererTests {
    @Test("streaming context menus retain selection copy but omit whole-message copy")
    func streamingCopyMenu() throws {
        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 500, height: 80))
        view.copyMessage = {}
        view.hasTranscriptSelection = { true }
        view.canCopyMessage = { false }
        let event = try #require(NSEvent.mouseEvent(with: .rightMouseDown, location: .zero,
            modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil, eventNumber: 1,
            clickCount: 1, pressure: 0))
        #expect(view.menu(for: event)?.items.map(\.title) == ["Copy", "Reply…"])
        view.canCopyMessage = { true }
        #expect(view.menu(for: event)?.items.map(\.title) == ["Copy", "Reply…", "Copy Message"])
    }

    @Test("attempted publication keeps input cache independently of completion")
    func attemptedPublicationKeepsInputCache() {
        let coordinator = MacAgentTranscriptView.Coordinator()
        let theme = MarkdownTheme.ottam(.builtIn)
        let snapshot = ChatTranscriptSnapshot(conversationID: ConversationID("conversation"))
        coordinator.beginCanonicalPublication(snapshot: snapshot, theme: theme)

        #expect(coordinator.snapshot == snapshot)
        #expect(coordinator.theme == theme)
    }

    @Test("a quote reply stays in one bubble, its string matching its source")
    func bubbleKeepsQuoteAndReplyTogether() throws {
        let blocks = TranscriptBlockProjector().project([
            TranscriptItem(
                id: "reply",
                content: .markdown("> quoted\n> lines\n\nmy *reply*", syntax: .inline, container: .bubble)
            )
        ])
        #expect(blocks.count == 1)
        guard case .text(let text) = try #require(blocks.first).content else {
            Issue.record("expected a text block")
            return
        }
        #expect(text.node == nil)
        #expect(text.paragraphs.map(\.quoteDepth) == [1, 0])
        #expect(text.source == "quoted\nlines\nmy reply")
        let attributed = TranscriptTextMeasurement.attributedString(for: text, theme: .ottam(.builtIn))
        // Selection offsets index the source and the drawn string alike.
        #expect(attributed.string == text.source)
        #expect(attributed.attribute(MarkdownAttribute.quoteDepth, at: 0, effectiveRange: nil) as? NSNumber == 1)
        #expect(attributed.attribute(MarkdownAttribute.quoteDepth, at: attributed.length - 1, effectiveRange: nil) == nil)
    }

    @Test("a bubble that is only a quote draws its bar inside the bubble")
    func quoteOnlyBubbleUsesParagraphs() throws {
        let blocks = TranscriptBlockProjector().project([
            TranscriptItem(id: "quote", content: .markdown("> just a quote", syntax: .inline, container: .bubble))
        ])
        #expect(blocks.count == 1)
        guard case .text(let text) = try #require(blocks.first).content else {
            Issue.record("expected a text block")
            return
        }
        #expect(text.node == nil)
        #expect(text.paragraphs.map(\.quoteDepth) == [1])
        #expect(text.source == "just a quote")
    }

    @Test("native Core Text links keep link appearance and destination")
    func nativeLinkAppearance() throws {
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(
                id: "links",
                content: .markdown(
                    "- [Architecture overview](docs/architecture/README.md)",
                    syntax: .full,
                    container: nil
                )
            )
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a text block")
            return
        }
        let attributed = TranscriptTextMeasurement.attributedString(
            for: text,
            theme: .ottam(.builtIn)
        )
        let range = (attributed.string as NSString).range(of: "Architecture overview")

        #expect(range.location != NSNotFound)
        #expect(attributed.attribute(.link, at: range.location, effectiveRange: nil) != nil)
        #expect(
            attributed.attribute(.foregroundColor, at: range.location, effectiveRange: nil)
                as? NSColor == NSColor.linkColor
        )
        // Color alone marks the link; no underline.
        #expect(attributed.attribute(.underlineStyle, at: range.location, effectiveRange: nil) == nil)
    }

    @Test("relative transcript links resolve inside the conversation root")
    func relativeLinkResolution() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let relative = try #require(NSURL(string: "docs/architecture/README.md"))
        let resolved = try #require(TranscriptLinkTarget.resolve(relative, relativeTo: root))

        #expect(resolved.isFileURL)
        #expect(resolved.path == root.appendingPathComponent("docs/architecture/README.md").path)
        #expect(
            TranscriptLinkTarget.resolve("../outside.md", relativeTo: root) == nil
        )
        #expect(
            TranscriptLinkTarget.resolve("file:///etc/passwd", relativeTo: root) == nil
        )
        #expect(
            TranscriptLinkTarget.resolve("https://example.com/docs", relativeTo: root)
                == URL(string: "https://example.com/docs")
        )
    }

    @Test("a link with a line suffix resolves as a path")
    func lineSuffixLinkResolvesAsPath() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let doc = root.appendingPathComponent("docs/a.md").path
        let line = try #require(NSURL(string: "docs/a.md:12"))
        #expect(TranscriptLinkTarget.resolve(line, relativeTo: root)?.path == doc)
        #expect(TranscriptLinkTarget.resolve("a.md:12:4", relativeTo: root)?.path == root.appendingPathComponent("a.md").path)
        #expect(TranscriptLinkTarget.resolve("docs/a.md#L12", relativeTo: root)?.path == doc)
        #expect(
            TranscriptLinkTarget.resolve("https://example.com:8080/a.md", relativeTo: root)
                == URL(string: "https://example.com:8080/a.md")
        )
        #expect(FileReference.path("docs/a.md#L12-L20") == "docs/a.md")
        #expect(FileReference.path("docs/a.md") == "docs/a.md")
    }

    @Test("a file tab's links and images start from its folder and stay inside the root")
    func folderRootResolvesSiblingDoc() {
        let root = URL(fileURLWithPath: "/repo", isDirectory: true)
        let folder = URL(fileURLWithPath: "docs/specs", isDirectory: true, relativeTo: root)
        #expect(folder.path == "/repo/docs/specs")
        #expect(TranscriptLinkTarget.resolve("b.md", relativeTo: folder)?.path == "/repo/docs/specs/b.md")
        #expect(TranscriptLinkTarget.resolve("../NOW.md", relativeTo: folder)?.path == "/repo/docs/NOW.md")
        #expect(TranscriptLinkTarget.resolve("../../../etc/a.md", relativeTo: folder) == nil)
        #expect(TranscriptLinkTarget.resolve("/repo/a.md", relativeTo: folder)?.path == "/repo/a.md")
        #expect(MarkdownImageAttachment.resolve(URL(string: "img.png")!, relativeTo: folder).path == "/repo/docs/specs/img.png")
        #expect(MarkdownImageAttachment.isContained(URL(fileURLWithPath: "/repo/x.png"), in: folder))
        #expect(!MarkdownImageAttachment.isContained(URL(fileURLWithPath: "/etc/x.png"), in: folder))
    }

    @Test("a code span is looked up from a file tab's folder, then from the root")
    func inlineCodeFromFolderThenRoot() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("docs/specs"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        try "# S".write(to: root.appendingPathComponent("docs/specs/sibling.md"), atomically: true, encoding: .utf8)
        try "# N".write(to: root.appendingPathComponent("docs/NOW.md"), atomically: true, encoding: .utf8)
        let folder = URL(fileURLWithPath: "docs/specs", isDirectory: true, relativeTo: root)
        let links = InlineCodeFileLinks()
        #expect(links.file(named: "sibling.md", in: folder)?.lastPathComponent == "sibling.md")
        #expect(links.file(named: "docs/NOW.md", in: folder)?.lastPathComponent == "NOW.md")
        #expect(links.file(named: "../../../outside.md", in: folder) == nil)
    }

    @Test("a cached render picks up a named file once it exists, and table cells link too")
    func inlineCodeLinksFollowTheDisk() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("docs"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let blocks = TranscriptBlockProjector().project([
            TranscriptItem(id: "later", content: .markdown("Writing `docs/later.md` next.", syntax: .full, container: nil)),
            TranscriptItem(id: "table", content: .markdown("| File |\n|---|\n| `docs/later.md` |", syntax: .full, container: nil)),
        ])
        let clock = ManualClock()
        let cache = TranscriptTextArtifactCache(now: { clock.now })
        let theme = MarkdownTheme.ottam(.builtIn)
        func link(in block: TranscriptBlock) -> URL? {
            guard let artifact = cache.artifact(for: block, width: 600, theme: theme, resourceRoot: root) else { return nil }
            let string = artifact.attributed.string as NSString
            let range = string.range(of: "docs/later.md")
            guard range.location != NSNotFound else { return nil }
            return artifact.attributed.attribute(.link, at: range.location, effectiveRange: nil) as? URL
        }
        for block in blocks { #expect(link(in: block) == nil) }
        try "# Later".write(to: root.appendingPathComponent("docs/later.md"), atomically: true, encoding: .utf8)
        clock.advance(InlineCodeFileLinks.recheckInterval + 0.1)
        for block in blocks { #expect(link(in: block)?.lastPathComponent == "later.md") }
    }

    @Test("a code span naming a markdown file under the root becomes a link")
    func inlineCodeMarkdownPathBecomesLink() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("docs"), withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        try "# A".write(to: root.appendingPathComponent("docs/a.md"), atomically: true, encoding: .utf8)
        try "x".write(to: root.appendingPathComponent("docs/a.swift"), atomically: true, encoding: .utf8)

        let links = InlineCodeFileLinks()
        #expect(links.file(named: "docs/a.md", in: root)?.lastPathComponent == "a.md")
        #expect(links.file(named: "docs/a.md:12", in: root)?.lastPathComponent == "a.md")
        #expect(links.file(named: "docs/missing.md", in: root) == nil)
        #expect(links.file(named: "docs/a.swift", in: root) == nil)
        #expect(links.file(named: "../outside.md", in: root) == nil)
        #expect(links.file(named: "see docs/a.md", in: root) == nil)

        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(
                id: "paths",
                content: .markdown("Read `docs/a.md` and `docs/b.md`.", syntax: .full, container: nil)
            )
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a text block")
            return
        }
        let attributed = TranscriptTextMeasurement.attributedString(
            for: text,
            theme: .ottam(.builtIn),
            resourceRoot: root
        )
        let string = attributed.string as NSString
        let found = string.range(of: "docs/a.md")
        let missing = string.range(of: "docs/b.md")
        let link = attributed.attribute(.link, at: found.location, effectiveRange: nil) as? URL
        #expect(link?.lastPathComponent == "a.md")
        #expect(attributed.attribute(.link, at: missing.location, effectiveRange: nil) == nil)
    }

    @Test("native collection items preserve exact layout-owned attributes")
    @MainActor
    func exactPreferredLayoutAttributes() {
        let textAttributes = NSCollectionViewLayoutAttributes(
            forItemWith: IndexPath(item: 2, section: 0)
        )
        textAttributes.frame = NSRect(x: 22, y: 140, width: 700, height: 183)
        let textItem = TranscriptTextCollectionItem()
        #expect(textItem.preferredLayoutAttributesFitting(textAttributes) === textAttributes)
        #expect(textAttributes.frame == NSRect(x: 22, y: 140, width: 700, height: 183))

        let cardAttributes = NSCollectionViewLayoutAttributes(
            forItemWith: IndexPath(item: 3, section: 0)
        )
        cardAttributes.frame = NSRect(x: 22, y: 339, width: 700, height: 96)
        let cardItem = TranscriptCardCollectionItem()
        #expect(cardItem.preferredLayoutAttributesFitting(cardAttributes) === cardAttributes)
        #expect(cardAttributes.frame == NSRect(x: 22, y: 339, width: 700, height: 96))
    }

    @Test("the transcript scroller overlays without shifting the reading column")
    func transcriptScrollerDoesNotReserveHorizontalSpace() throws {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 900, height: 500)
        controller.view.layoutSubtreeIfNeeded()

        let scrollView = try #require(controller.view as? NSScrollView)
        let viewportWidth = scrollView.contentView.frame.width
        scrollView.autohidesScrollers = false
        scrollView.tile()

        #expect(scrollView.scrollerStyle == .overlay)
        #expect(scrollView.contentView.frame.width == viewportWidth)
    }

    @Test("an unclosed fence streams into one stable code block and settles as one surface")
    func unclosedFenceKeepsOneStableSurface() throws {
        let full = "para\n\n```rust\nfn main() {\n    println!(\"hi\");\n}"
        let fenceStart = try #require(full.range(of: "```")).upperBound
        let fenceEnd = try #require(full.range(of: "```rust")).upperBound
        for cut in 1...full.count {
            let text = String(full.prefix(cut))
            let projector = TranscriptBlockProjector()
            var items = [TranscriptItem(id: "r", content: .markdown("", syntax: .full, container: nil))]
            _ = projector.project(items)
            var codeIDs = Set<TranscriptBlockID>()
            for ch in text {
                _ = projector.append(String(ch), to: "r", in: &items)
                for block in projector.projectedUpdate(itemID: "r", relation: .turn)?.blocks ?? [] {
                    if case .text(let t) = block.content, case .codeBlock? = t.node?.kind {
                        codeIDs.insert(block.id)
                    }
                }
            }
            let settled = projector.project(items)
            let fresh = TranscriptBlockProjector().project(items)
            #expect(settled.map(\.content) == fresh.map(\.content), "cut \(cut)")

            let codeBlocks = settled.filter {
                if case .text(let t) = $0.content, case .codeBlock? = t.node?.kind { return true }
                return false
            }
            guard text.endIndex >= fenceStart else {
                #expect(codeBlocks.isEmpty, "cut \(cut)")
                continue
            }
            // Three line-leading backticks are conclusive: the block exists
            // from then on, keeps one identity, and never gains a sibling.
            #expect(codeBlocks.count == 1, "cut \(cut): one surface, never an empty sibling")
            #expect(codeIDs.count == 1, "cut \(cut): the fence's block keeps one identity while streaming")
            #expect(codeBlocks.first.map { codeIDs.contains($0.id) } == true, "cut \(cut)")
            if text.endIndex >= fenceEnd,
               case .text(let t)? = codeBlocks.first?.content,
               case .codeBlock(let hint, let code)? = t.node?.kind {
                #expect(hint == "rust")
                var expectedBody = String(text[fenceEnd...].drop(while: { $0 == "\n" }))
                if expectedBody.hasSuffix("\n") { expectedBody.removeLast() }
                #expect(code == expectedBody, "cut \(cut)")
            }
        }
    }

    @Test("projector preserves item and changing-tail identity")
    func stableProjectionIdentity() throws {
        let projector = TranscriptBlockProjector()
        let first = projector.project([
            TranscriptItem(id: "reply", content: .markdown("Hello", syntax: .full, container: nil))
        ])
        let second = projector.project([
            TranscriptItem(id: "reply", content: .markdown("Hello world", syntax: .full, container: nil))
        ])

        #expect(try #require(first.last).id == #require(second.last).id)
        #expect(second.last?.itemID == "reply")
    }

    @Test("projection store publishes blocks and indexes atomically")
    func projectionStoreIndexes() throws {
        let store = TranscriptProjectionStore()
        let initial = [
            TranscriptItem(
                id: "first",
                content: .markdown("First paragraph.", syntax: .full, container: nil)
            ),
            TranscriptItem(
                id: "second",
                content: .markdown("Second paragraph.", syntax: .full, container: nil)
            ),
        ]
        let replacement = store.replace(plan: initial, resetProjector: true)
        #expect(replacement.oldRange.isEmpty)
        #expect(store.itemIndexByID == ["first": 0, "second": 1])
        #expect(store.blockRangeByItemID["first"] == 0..<1)
        #expect(store.blockRangeByItemID["second"] == 1..<2)
        #expect(store.indexByID[store.blocks[0].id] == 0)

        let append = try #require(store.append(" More text.", to: "first"))
        #expect(append.oldRange == 0..<1)
        #expect(store.blockRangeByItemID["first"] == 0..<1)
        #expect(store.sourcePlan[0].markdownText == "First paragraph. More text.")
        #expect(store.indexByID[store.blocks[0].id] == 0)

        let removal = store.replace(plan: [initial[1]], resetProjector: false)
        #expect(removal.removedIDs.contains(replacement.replacementBlocks[0].id))
        #expect(store.itemIndexByID == ["second": 0])
        #expect(store.blockRangeByItemID["second"] == 0..<1)
    }

    @Test("text artifacts reuse exact inputs and invalidate narrowly")
    func textArtifactCacheInvalidation() throws {
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(
                id: "artifact",
                content: .markdown("Artifact text", syntax: .full, container: nil)
            )
        ]).first)
        let cache = TranscriptTextArtifactCache()
        let theme = MarkdownTheme.ottam(.builtIn)
        let first = try #require(cache.artifact(
            for: block,
            width: 700,
            theme: theme,
            resourceRoot: nil
        ))
        let identical = try #require(cache.artifact(
            for: block,
            width: 700,
            theme: theme,
            resourceRoot: nil
        ))
        #expect(first === identical)
        #expect(cache.buildCount == 1)

        let narrower = try #require(cache.artifact(
            for: block,
            width: 500,
            theme: theme,
            resourceRoot: nil
        ))
        #expect(narrower !== first)
        #expect(cache.buildCount == 2)

        var changed = block
        if case .text(var text) = changed.content {
            text.source += " changed"
            changed.content = .text(text)
        }
        let changedArtifact = try #require(cache.artifact(
            for: changed,
            width: 500,
            theme: theme,
            resourceRoot: nil
        ))
        #expect(changedArtifact !== narrower)
        #expect(cache.buildCount == 3)
    }

    @Test("text artifact cache remains bounded while traversing a long transcript")
    @MainActor
    func boundedTextArtifactCache() {
        let cache = TranscriptTextArtifactCache()
        let theme = MarkdownTheme.ottam(.builtIn)
        for index in 0..<(TranscriptTextArtifactCache.maximumCount + 40) {
            let block = TranscriptBlock(
                id: TranscriptBlockID(itemID: "item-\(index)", generation: UInt64(index)),
                itemID: "item-\(index)",
                relation: .turn,
                content: .text(.init(
                    source: "Row \(index)",
                    syntax: .full,
                    container: nil,
                    node: nil
                ))
            )
            #expect(cache.artifact(
                for: block,
                width: 700,
                theme: theme,
                resourceRoot: nil
            ) != nil)
        }

        #expect(cache.count == TranscriptTextArtifactCache.maximumCount)
    }

    @Test("selection-only presentation preserves artifact and fade overlay identity")
    func incrementalTextPresentation() throws {
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(
                id: "presentation",
                content: .markdown("Streaming presentation", syntax: .full, container: nil)
            )
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a projected text block")
            return
        }
        let artifact = TranscriptTextRenderArtifact(
            blockID: block.id,
            text: text,
            width: 700,
            theme: .ottam(.builtIn),
            resourceRoot: nil
        )
        let batch = LiveBatchAnimationCoordinator.Batch(
            id: 42,
            range: NSRange(location: 0, length: 9),
            startedAt: ProcessInfo.processInfo.systemUptime,
            duration: 10
        )
        let item = TranscriptTextCollectionItem()
        item.loadView()
        item.view.frame = NSRect(x: 0, y: 0, width: 700, height: artifact.measuredHeight)
        item.apply(TranscriptTextPresentation(
            itemID: block.itemID,
            artifact: artifact,
            fadeBatches: [batch],
            selectedRange: NSRange(location: 0, length: 0)
        ))
        let artifactIdentity = try #require(item.debugArtifactIdentity)
        let overlayIdentities = item.debugFadeOverlayIdentities
        let surfaceRefreshCount = item.debugFadeSurfaceRefreshCount
        #expect(overlayIdentities.count == 1)

        item.apply(TranscriptTextPresentation(
            itemID: block.itemID,
            artifact: artifact,
            fadeBatches: [batch],
            selectedRange: NSRange(location: 2, length: 5)
        ))
        #expect(item.debugArtifactIdentity == artifactIdentity)
        #expect(item.debugFadeOverlayIdentities == overlayIdentities)
        #expect(item.debugFadeSurfaceRefreshCount == surfaceRefreshCount)
        #expect(item.debugSelectedRange == NSRange(location: 2, length: 5))

        item.prepareForReuse()
        #expect(item.debugFadeOverlayIdentities.isEmpty)
    }

    @Test("a retained fade surface refreshes for a reflowed artifact without restarting")
    func retainedFadeSurfaceFollowsReflow() throws {
        let projector = TranscriptBlockProjector()
        let initialBlock = try #require(projector.project([
            TranscriptItem(
                id: "reflow",
                content: .markdown("A streamed phrase", syntax: .full, container: nil)
            )
        ]).first)
        let nextBlock = try #require(projector.project([
            TranscriptItem(
                id: "reflow",
                content: .markdown(
                    "A streamed phrase that grows enough to change its line layout",
                    syntax: .full,
                    container: nil
                )
            )
        ]).first)
        guard case .text(let initialText) = initialBlock.content,
              case .text(let nextText) = nextBlock.content
        else {
            Issue.record("expected projected text blocks")
            return
        }
        #expect(initialBlock.id == nextBlock.id)
        let theme = MarkdownTheme.ottam(.builtIn)
        let initialArtifact = TranscriptTextRenderArtifact(
            blockID: initialBlock.id,
            text: initialText,
            width: 240,
            theme: theme,
            resourceRoot: nil
        )
        let nextArtifact = TranscriptTextRenderArtifact(
            blockID: nextBlock.id,
            text: nextText,
            width: 240,
            theme: theme,
            resourceRoot: nil
        )
        let batch = LiveBatchAnimationCoordinator.Batch(
            id: 44,
            range: NSRange(location: 2, length: 8),
            startedAt: ProcessInfo.processInfo.systemUptime,
            duration: 10
        )
        let item = TranscriptTextCollectionItem()
        item.loadView()
        item.view.frame = NSRect(x: 0, y: 0, width: 240, height: initialArtifact.measuredHeight)
        item.apply(TranscriptTextPresentation(
            itemID: initialBlock.itemID,
            artifact: initialArtifact,
            fadeBatches: [batch],
            selectedRange: NSRange(location: 0, length: 0)
        ))
        let overlayIdentities = item.debugFadeOverlayIdentities
        let refreshCount = item.debugFadeSurfaceRefreshCount

        item.view.frame.size.height = nextArtifact.measuredHeight
        item.apply(TranscriptTextPresentation(
            itemID: nextBlock.itemID,
            artifact: nextArtifact,
            fadeBatches: [batch],
            selectedRange: NSRange(location: 0, length: 0)
        ))

        #expect(item.debugFadeOverlayIdentities == overlayIdentities)
        #expect(item.debugFadeSurfaceRefreshCount == refreshCount + 1)
    }

    @Test("inline-code background follows the streamed glyph fade")
    func inlineCodeBackgroundFadesWithGlyphs() throws {
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(
                id: "inline-code",
                content: .markdown("this is a `code` block", syntax: .full, container: nil)
            )
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a projected text block")
            return
        }
        let view = TranscriptTextItemView(
            frame: NSRect(x: 0, y: 0, width: 700, height: 80)
        )
        let theme = MarkdownTheme.ottam(.builtIn)
        let styled = TranscriptTextMeasurement.attributedString(for: text, theme: theme)
        let codeRange = (styled.string as NSString).range(of: "code")
        #expect(codeRange.location != NSNotFound)
        // The item paints the pill itself; the styler's flat run background is gone.
        #expect(styled.attribute(.backgroundColor, at: codeRange.location, effectiveRange: nil) == nil)

        view.configure(blockID: block.id, text: text, theme: theme)
        view.layoutSubtreeIfNeeded()
        #expect(view.debugInlineCodePillRects().count == 1)

        view.configure(
            blockID: block.id,
            text: text,
            theme: theme,
            fadeBatches: [LiveBatchAnimationCoordinator.Batch(
                id: 43,
                range: codeRange,
                startedAt: ProcessInfo.processInfo.systemUptime,
                duration: 10
            )]
        )

        let baseAttributes = view.debugBaseDrawingAttributes(at: codeRange.location)
        let foreground = try #require(baseAttributes[.foregroundColor] as? NSColor)
        #expect(foreground.alphaComponent == 0)
        // The base pass leaves the fading span's pill to the fade overlay.
        #expect(view.debugInlineCodePillRects().isEmpty)
    }

    @Test("inline code gets kerned padding and a pill sized to it")
    func inlineCodePillPadding() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(
                id: "inline-code",
                content: .markdown("call `loadView()` then `configure(card:)` last", syntax: .full, container: nil)
            )
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a projected text block")
            return
        }
        let styled = TranscriptTextMeasurement.attributedString(for: text, theme: theme)
        let ranges = TranscriptTextMeasurement.inlineCodeRanges(in: styled)
        #expect(ranges.count == 2)
        let padding = theme.metrics.inlineCodePadding
        for range in ranges {
            #expect(styled.attribute(.kern, at: range.upperBound - 1, effectiveRange: nil) as? CGFloat == padding)
            #expect(styled.attribute(.kern, at: range.location - 1, effectiveRange: nil) as? CGFloat == padding)
            #expect(styled.attribute(.kern, at: range.location, effectiveRange: nil) == nil)
            #expect(styled.attribute(.font, at: range.location, effectiveRange: nil) as? NSFont == theme.inlineCode)
        }
        // Copy text is untouched: padding is advance, not characters.
        #expect(styled.string == "call loadView() then configure(card:) last")

        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 700, height: 60))
        view.configure(blockID: block.id, text: text, theme: theme)
        view.layoutSubtreeIfNeeded()
        let pills = view.debugInlineCodePillRects()
        #expect(pills.count == 2)
        let glyphWidth = ("loadView()" as NSString).size(withAttributes: [.font: theme.inlineCode]).width
        // Glyphs plus one padding each side, within Core Text rounding.
        #expect(abs(pills[0].width - (glyphWidth + padding * 2)) < 1.5)
        #expect(pills[0].height > theme.inlineCode.pointSize)
        #expect(pills[0].maxX < pills[1].minX)
    }

    @Test("inline code pills are padded symmetrically after a dash, a comma and bold text")
    func inlineCodePillSymmetry() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let source = "- **app code** — `ChatWorkspaceModel` +224, `ChatWorkspaceView` +201, `ChatMessageQueue` 151"
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(id: "bullet", content: .markdown(source, syntax: .full, container: nil))
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a projected text block")
            return
        }
        let styled = TranscriptTextMeasurement.attributedString(for: text, theme: theme)
        let ranges = TranscriptTextMeasurement.inlineCodeRanges(in: styled)
        #expect(ranges.count == 3)
        let padding = theme.metrics.inlineCodePadding
        for range in ranges {
            #expect(styled.attribute(.kern, at: range.location - 1, effectiveRange: nil) as? CGFloat == padding)
            #expect(styled.attribute(.kern, at: range.upperBound - 1, effectiveRange: nil) as? CGFloat == padding)
        }

        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 900, height: 60))
        view.configure(blockID: block.id, text: text, theme: theme)
        view.layoutSubtreeIfNeeded()
        let pills = view.debugInlineCodePillRects()
        #expect(pills.count == 3)
        for (pill, range) in zip(pills, ranges) {
            let word = (styled.string as NSString).substring(with: range)
            let glyphWidth = (word as NSString).size(withAttributes: [.font: theme.inlineCode]).width
            // Each side carries exactly one padding: the pill is the glyphs plus 2×padding.
            #expect(abs(pill.width - (glyphWidth + padding * 2)) < 1.5, "\(word)")
        }
    }

    @Test("a pill opening a paragraph stops at the text edge instead of drawing outside the item")
    func inlineCodePillAtLineStart() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(id: "p", content: .markdown("`swift build` then run", syntax: .full, container: nil))
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a projected text block")
            return
        }
        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 700, height: 60))
        view.configure(blockID: block.id, text: text, theme: theme)
        view.layoutSubtreeIfNeeded()
        let pills = view.debugInlineCodePillRects()
        #expect(pills.count == 1)
        // No glyph precedes the span to carry its leading pad, so the pill is
        // clamped to the text origin: never negative, trailing pad intact.
        #expect(pills[0].minX == 0)
        let glyphWidth = ("swift build" as NSString).size(withAttributes: [.font: theme.inlineCode]).width
        #expect(abs(pills[0].width - (glyphWidth + theme.metrics.inlineCodePadding)) < 1.5)
    }

    @Test("background preparation measures quoted blocks with their lead-in")
    func backgroundPreparationQuoteLeadIn() async throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let spacing = theme.metrics.blockSpacing
        let items = [TranscriptItem(id: "q", content: .markdown("> first\n>\n> second", syntax: .full, container: nil))]
        let service = TranscriptMeasurementService()
        let result: TranscriptMeasurementService.Result = await withCheckedContinuation { continuation in
            service.prepare(.init(
                items: items.map(TranscriptProjectionItem.init),
                width: 600,
                theme: theme,
                resourceRoot: nil
            )) { continuation.resume(returning: $0) }
        }
        #expect(result.blocks.count == 2)
        let second = result.blocks[1]
        guard case .text(let text) = second.content else {
            Issue.record("expected a projected text block")
            return
        }
        #expect(second.quoteContinuation(spacing: spacing) == (spacing.paragraph, 1))
        // The prepared height is what the main thread would cache beside the
        // block's full options, so it must already include the lead-in.
        let expected = TranscriptTextMeasurement.height(
            for: text, width: 600, theme: theme,
            options: .init(quoteLeadIn: spacing.paragraph, continuedQuoteDepth: 1)
        )
        #expect(result.textHeights[second.id] == expected)
        #expect(result.textHeights[second.id]! > TranscriptTextMeasurement.height(for: text, width: 600, theme: theme))
    }

    @Test("inline code is sized relative to the text around it and follows the UI delta")
    func inlineCodeRelativeSize() {
        let base = AppTypography.Resolution.builtIn
        #expect(base.inlineCodeFont(within: .prose).pointSize == 13)
        #expect(base.inlineCodeFont(within: .body).pointSize == 12)
        #expect(MarkdownTheme.ottam(base).inlineCode.pointSize == 13)
        #expect(MarkdownTheme.ottam(base).codeChrome.pointSize == base.points(for: .label))

        let larger = AppTypography.Resolution(uiSizeDelta: 2, codeSizeDelta: -3)
        // 16 × 0.93 = 14.9 → 15: the span grows with the prose it sits in, not the code control.
        #expect(larger.inlineCodeFont(within: .prose).pointSize == 15)
        #expect(larger.inlineCodeFont(within: .prose).fontName == larger.nsFont(.code).fontName)
    }

    @Test("projector retains sealed markdown block identities")
    func sealedPrefixIdentity() throws {
        let projector = TranscriptBlockProjector()
        let first = projector.project([
            TranscriptItem(
                id: "reply",
                content: .markdown("First paragraph\n\nSecond", syntax: .full, container: nil)
            )
        ])
        let second = projector.project([
            TranscriptItem(
                id: "reply",
                content: .markdown("First paragraph\n\nSecond grows", syntax: .full, container: nil)
            )
        ])

        #expect(first.count == 2)
        #expect(second.count == 2)
        #expect(first[0].id == second[0].id)
        #expect(first[1].id == second[1].id)
    }

    @Test("exact collection layout returns only visible and prefetched items")
    func visibleRange() throws {
        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        collection.collectionViewLayout = layout
        layout.prefetchMargin = 0
        layout.verticalInset = 0
        layout.turnSpacing = 0
        layout.entries = (0..<100).map {
            .init(
                id: TranscriptBlockID(itemID: "\($0)", generation: UInt64($0)),
                height: 40,
                relation: .turn
            )
        }
        layout.prepare()

        let visible = layout.layoutAttributesForElements(in: NSRect(x: 0, y: 400, width: 800, height: 400))
        #expect(visible.count <= 12)
        let first = try #require(visible.first?.indexPath)
        #expect(first.item == 9 || first.item == 10)
        #expect(layout.collectionViewContentSize.height == 4_000)
        #expect(layout.nearestItemIndex(atY: -10) == 0)
        #expect(layout.nearestItemIndex(atY: 405) == 10)
        #expect(layout.nearestItemIndex(atY: 10_000) == 99)
    }

    @Test("stable tail growth invalidates one item and retains prefix attributes")
    func stableTailInvalidation() throws {
        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        collection.collectionViewLayout = layout
        let entries = (0..<4).map {
            TranscriptCollectionLayout.Entry(
                id: TranscriptBlockID(itemID: "\($0)", generation: UInt64($0)),
                height: 40,
                relation: .turn
            )
        }
        layout.entries = entries
        layout.prepare()
        let prefix = try #require(layout.layoutAttributesForItem(
            at: IndexPath(item: 1, section: 0)
        ))
        var changed = entries
        changed[3] = .init(id: entries[3].id, height: 80, relation: .turn)

        layout.setEntries(changed, invalidatingFrom: 3)

        #expect(layout.debugLastInvalidation?.scope == "item")
        #expect(layout.debugLastInvalidation?.dirtyStart == 3)
        #expect(layout.debugLastInvalidation?.invalidatedItems == [3])
        layout.prepare()
        let retained = try #require(layout.layoutAttributesForItem(
            at: IndexPath(item: 1, section: 0)
        ))
        #expect(prefix === retained)
        #expect(layout.collectionViewContentSize.height == 400)
        #expect(layout.layoutAttributesForItem(
            at: IndexPath(item: 3, section: 0)
        )?.frame.height == 80)
    }

    @Test("middle height changes invalidate the surviving suffix")
    func middleSuffixInvalidation() {
        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        collection.collectionViewLayout = layout
        var entries = (0..<5).map {
            TranscriptCollectionLayout.Entry(
                id: TranscriptBlockID(itemID: "\($0)", generation: UInt64($0)),
                height: 40,
                relation: .turn
            )
        }
        layout.entries = entries
        layout.prepare()
        entries[2] = .init(id: entries[2].id, height: 90, relation: .turn)

        layout.setEntries(entries, invalidatingFrom: 2)

        #expect(layout.debugLastInvalidation?.scope == "suffix")
        #expect(layout.debugLastInvalidation?.dirtyStart == 2)
        #expect(layout.debugLastInvalidation?.invalidatedItems == [2, 3, 4])
        layout.prepare()
        #expect(layout.layoutAttributesForItem(
            at: IndexPath(item: 3, section: 0)
        )?.frame.minY == 236)
    }

    @Test("global collection invalidation rebuilds every item")
    func globalCollectionInvalidation() {
        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 240))
        collection.collectionViewLayout = layout
        layout.entries = (0..<3).map {
            .init(
                id: TranscriptBlockID(itemID: "\($0)", generation: UInt64($0)),
                height: 40,
                relation: .turn
            )
        }
        layout.prepare()

        layout.invalidateLayout()

        #expect(layout.debugLastInvalidation?.scope == "full")
        #expect(layout.debugLastInvalidation?.dirtyStart == 0)
        #expect(layout.debugLastInvalidation?.invalidatedItems == [0, 1, 2])
    }

    @Test("attached markdown blocks are spaced by the roles either side")
    func roleAwareBlockSpacing() {
        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        collection.collectionViewLayout = layout
        layout.verticalInset = 0
        layout.turnSpacing = 100
        layout.attachedSpacing = 1
        let spacing = MarkdownBlockSpacing()
        layout.blockSpacing = spacing
        let roles: [MarkdownBlockRole?] = [
            .heading, .paragraph, .paragraph, .listItem, .listItem, .heading, .codeBlock, nil,
        ]
        layout.entries = roles.enumerated().map { index, role in
            .init(
                id: TranscriptBlockID(itemID: "item", generation: UInt64(index)),
                height: 20,
                relation: index == 0 ? .turn : .attachesToPrevious,
                block: role.map { MarkdownBlockSpacing.Neighbor(role: $0) }
            )
        }
        layout.prepare()

        let gaps = (1..<roles.count).map { index -> CGFloat in
            let previous = layout.layoutAttributesForItem(at: IndexPath(item: index - 1, section: 0))!.frame
            let current = layout.layoutAttributesForItem(at: IndexPath(item: index, section: 0))!.frame
            return current.minY - previous.maxY
        }
        #expect(gaps == [
            spacing.afterHeading, // heading → paragraph
            spacing.paragraph, // paragraph → paragraph
            spacing.paragraph, // paragraph → list
            spacing.listItem, // list → list
            spacing.beforeHeading, // list → heading
            spacing.afterHeading, // heading → code
            layout.attachedSpacing, // code → card (no role)
        ])
    }

    @Test("quoted neighbours leave no layout gap; the block absorbs it and its bar runs through")
    func quoteContinuation() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let spacing = theme.metrics.blockSpacing
        let blocks = TranscriptBlockProjector().project([
            TranscriptItem(
                id: "quote",
                content: .markdown("> first\n>\n> second\n>\n> > nested\n\nplain", syntax: .full, container: nil)
            )
        ])
        #expect(blocks.count == 4)
        #expect(blocks[0].predecessor == nil)
        #expect(blocks[1].predecessor?.quoteDepth == 1)
        #expect(blocks[1].quoteContinuation(spacing: spacing) == (spacing.paragraph, 1))
        // Outer bar continues, the inner one starts with its own text.
        #expect(blocks[2].quoteContinuation(spacing: spacing) == (spacing.paragraph, 1))
        #expect(blocks[3].quoteContinuation(spacing: spacing) == (0, 0))

        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        collection.collectionViewLayout = layout
        layout.verticalInset = 0
        layout.entries = blocks.map {
            .init(id: $0.id, height: 20, relation: $0.relation, block: $0.markdownNeighbor)
        }
        layout.prepare()
        let frames = (0..<4).map { layout.layoutAttributesForItem(at: IndexPath(item: $0, section: 0))!.frame }
        #expect(frames[1].minY == frames[0].maxY)
        #expect(frames[2].minY == frames[1].maxY)
        // The plain paragraph after the quote takes the ordinary gap again.
        #expect(frames[3].minY - frames[2].maxY == spacing.paragraph)

        // The absorbed gap is part of the block's measured height.
        guard case .text(let second) = blocks[1].content else { return }
        let plain = TranscriptTextMeasurement.height(for: second, width: 600, theme: theme)
        let withLeadIn = TranscriptTextMeasurement.height(
            for: second, width: 600, theme: theme,
            options: .init(quoteLeadIn: spacing.paragraph, continuedQuoteDepth: 1)
        )
        #expect(withLeadIn == plain + spacing.paragraph)
    }

    @Test("table cells carry the inline code pill padding and no flat background")
    func tableCellPills() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(id: "t", content: .markdown("| a | b |\n|---|---|\n| `x` | y `z` |", syntax: .full, container: nil))
        ]).first)
        guard case .text(let text) = block.content else { return }
        let layout = try #require(TranscriptTextMeasurement.tableLayout(for: text, theme: theme))
        let styled = layout.attributedString(theme: theme)
        let ranges = TranscriptTextMeasurement.inlineCodeRanges(in: styled)
        #expect(ranges.count == 2)
        for range in ranges {
            #expect(styled.attribute(.backgroundColor, at: range.location, effectiveRange: nil) == nil)
            #expect(styled.attribute(.kern, at: range.upperBound - 1, effectiveRange: nil) as? CGFloat == theme.metrics.inlineCodePadding)
        }
        // `z` follows a space inside its cell, which takes the leading pad.
        #expect(styled.attribute(.kern, at: ranges[1].location - 1, effectiveRange: nil) as? CGFloat == theme.metrics.inlineCodePadding)
    }

    @Test("changing block spacing re-lays every frame")
    func blockSpacingChangeInvalidates() {
        let layout = TranscriptCollectionLayout()
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        collection.collectionViewLayout = layout
        layout.verticalInset = 0
        layout.entries = [
            .init(id: TranscriptBlockID(itemID: "a", generation: 0), height: 20, relation: .turn, block: .init(role: .paragraph)),
            .init(id: TranscriptBlockID(itemID: "a", generation: 1), height: 20, relation: .attachesToPrevious, block: .init(role: .paragraph)),
        ]
        layout.prepare()
        let before = layout.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))?.frame.minY

        var spacing = MarkdownBlockSpacing()
        spacing.paragraph += 30
        layout.blockSpacing = spacing
        layout.prepare()

        #expect(layout.debugLastInvalidation?.scope == "full")
        #expect(layout.layoutAttributesForItem(at: IndexPath(item: 1, section: 0))?.frame.minY == (before ?? 0) + 30)
    }

    @Test("Core Text measurement is exact and grows with wrapping")
    func textMeasurement() {
        let theme = MarkdownTheme.ottam(.builtIn)
        let text = TranscriptBlock.Text(
            source: String(repeating: "native transcript ", count: 30),
            syntax: .full,
            container: nil,
            node: nil
        )
        let wide = TranscriptTextMeasurement.height(for: text, width: 700, theme: theme)
        let narrow = TranscriptTextMeasurement.height(for: text, width: 220, theme: theme)
        #expect(narrow > wide)
        #expect(wide > 0)
    }

    @Test("Core Text measurement reserves an image attachment footprint")
    func imageAttachmentMeasurement() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let node = try #require(
            MarkdownBlockTreeParser.nodes(from: "![missing](missing.png)", syntax: .full).first
        )
        let text = TranscriptBlock.Text(
            source: String(node.inline.characters),
            syntax: .full,
            container: nil,
            node: node
        )

        let height = TranscriptTextMeasurement.height(for: text, width: 700, theme: theme)

        #expect(height >= MarkdownImageAttachment.placeholderSize.height)
    }

    @Test("selection copy uses exact Markdown for whole items and rendered endpoint text")
    func selectionCopyComposition() throws {
        let plan = [
            TranscriptItem(
                id: "reply",
                content: .markdown("# Heading\n\n**bold**", syntax: .full, container: nil)
            )
        ]
        let blocks = TranscriptBlockProjector().project(plan)
        let indices = Dictionary(uniqueKeysWithValues: blocks.indices.map { (blocks[$0].id, $0) })
        let selection = TranscriptSelectionController()
        let first = try #require(blocks.first)
        let last = try #require(blocks.last)

        selection.begin(
            at: .init(blockID: first.id, utf16Offset: 0),
            blockIndex: 0,
            blocks: blocks
        )
        selection.extend(
            to: .init(blockID: last.id, utf16Offset: 4),
            blockIndex: blocks.count - 1,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices)
            == "# Heading\n\n**bold**")

        selection.begin(
            at: .init(blockID: first.id, utf16Offset: 1),
            blockIndex: 0,
            blocks: blocks
        )
        selection.extend(
            to: .init(blockID: last.id, utf16Offset: 2),
            blockIndex: blocks.count - 1,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices)
            == "eading\nbo")
    }

    @Test("selecting a whole code block copies its fence while a partial range stays raw")
    func selectionCopyRefencesWholeCodeBlocks() throws {
        let plan = [
            TranscriptItem(
                id: "reply",
                content: .markdown(
                    "Intro\n\n```swift\nlet x = 1\nprint(x)\n```\n\nOutro",
                    syntax: .full,
                    container: nil
                )
            )
        ]
        let blocks = TranscriptBlockProjector().project(plan)
        let indices = Dictionary(uniqueKeysWithValues: blocks.indices.map { (blocks[$0].id, $0) })
        let selection = TranscriptSelectionController()
        #expect(blocks.count == 3)
        let code = blocks[1]

        selection.begin(at: .init(blockID: code.id, utf16Offset: 0), blockIndex: 1, blocks: blocks)
        selection.extend(
            to: .init(blockID: code.id, utf16Offset: ("let x = 1\nprint(x)" as NSString).length),
            blockIndex: 1,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices)
            == "```swift\nlet x = 1\nprint(x)\n```")

        selection.begin(at: .init(blockID: blocks[0].id, utf16Offset: 2), blockIndex: 0, blocks: blocks)
        selection.extend(
            to: .init(blockID: blocks[2].id, utf16Offset: 3),
            blockIndex: 2,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices)
            == "tro\n```swift\nlet x = 1\nprint(x)\n```\nOut")

        selection.begin(at: .init(blockID: code.id, utf16Offset: 4), blockIndex: 1, blocks: blocks)
        selection.extend(
            to: .init(blockID: code.id, utf16Offset: 9),
            blockIndex: 1,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices)
            == "x = 1")

        #expect(TranscriptSelectionController.fencedSource("a ``` b", hint: nil)
            == "````\na ``` b\n````")
    }

    @Test("projected selection text uses the rendered UTF-16 index space")
    func projectedSelectionIndexSpace() {
        let theme = MarkdownTheme.ottam(.builtIn)
        let plan = [
            TranscriptItem(
                id: "reply",
                content: .markdown(
                    "- first\n\n  continuation with ![alt text](image.png)\n\n---\n\n| A | B |\n| - | - |\n| 1 | 2 |",
                    syntax: .full,
                    container: nil
                )
            )
        ]

        for block in TranscriptBlockProjector().project(plan) {
            guard case .text(let text) = block.content else { continue }
            let rendered = TranscriptTextMeasurement.attributedString(for: text, theme: theme).string
            #expect(text.source == rendered)
            #expect(text.source.utf16.count == rendered.utf16.count)
        }
    }

    @Test("selection spans text blocks in both directions and clamps at cards")
    func crossBlockSelectionAndCardBoundary() throws {
        let projector = TranscriptBlockProjector()
        let first = try #require(projector.project([
            TranscriptItem(id: "first", content: .markdown("first", syntax: .full, container: nil))
        ]).first)
        let second = try #require(projector.project([
            TranscriptItem(id: "second", content: .markdown("second", syntax: .full, container: nil))
        ]).first)
        let third = try #require(projector.project([
            TranscriptItem(id: "third", content: .markdown("third", syntax: .full, container: nil))
        ]).first)
        let card = TranscriptBlock(
            id: TranscriptBlockID(itemID: "card", generation: 99),
            itemID: "card",
            relation: .turn,
            content: .card
        )
        let blocks = [first, second, card, third]
        let indices = Dictionary(uniqueKeysWithValues: blocks.indices.map { (blocks[$0].id, $0) })
        let selection = TranscriptSelectionController()

        selection.begin(at: .init(blockID: first.id, utf16Offset: 2), blockIndex: 0, blocks: blocks)
        selection.extend(
            to: .init(blockID: second.id, utf16Offset: 3),
            blockIndex: 1,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.localRange(for: first.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 2, length: 3))
        #expect(selection.localRange(for: second.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 0, length: 3))

        selection.begin(at: .init(blockID: second.id, utf16Offset: 4), blockIndex: 1, blocks: blocks)
        selection.extend(
            to: .init(blockID: first.id, utf16Offset: 1),
            blockIndex: 0,
            blocks: blocks,
            indexByID: indices
        )
        #expect(selection.localRange(for: first.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 1, length: 4))
        #expect(selection.localRange(for: second.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 0, length: 4))

        selection.begin(at: .init(blockID: first.id, utf16Offset: 2), blockIndex: 0, blocks: blocks)
        let forwardResult = selection.extend(
            to: .init(blockID: third.id, utf16Offset: 3),
            blockIndex: 3,
            blocks: blocks,
            indexByID: indices
        )
        #expect(forwardResult == .clampedAtBoundary)
        #expect(selection.localRange(for: first.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 2, length: 3))
        #expect(selection.localRange(for: second.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 0, length: 6))
        #expect(selection.localRange(for: third.id, blocks: blocks, indexByID: indices).length == 0)

        selection.begin(at: .init(blockID: third.id, utf16Offset: 3), blockIndex: 3, blocks: blocks)
        let backwardResult = selection.extend(
            to: .init(blockID: first.id, utf16Offset: 1),
            blockIndex: 0,
            blocks: blocks,
            indexByID: indices
        )
        #expect(backwardResult == .clampedAtBoundary)
        #expect(selection.localRange(for: third.id, blocks: blocks, indexByID: indices)
            == NSRange(location: 0, length: 3))
        #expect(selection.localRange(for: second.id, blocks: blocks, indexByID: indices).length == 0)
    }

    @Test("selection survives prepend and reuse, clamps growth, and clears removed endpoints")
    func selectionReconciliation() throws {
        let projector = TranscriptBlockProjector()
        let originalPlan = [
            TranscriptItem(id: "a", content: .markdown("alpha", syntax: .full, container: nil)),
            TranscriptItem(id: "b", content: .markdown("bravo", syntax: .full, container: nil)),
        ]
        let original = projector.project(originalPlan)
        let originalIndices = Dictionary(uniqueKeysWithValues: original.indices.map { (original[$0].id, $0) })
        let selection = TranscriptSelectionController()
        selection.begin(
            at: .init(blockID: original[0].id, utf16Offset: 2),
            blockIndex: 0,
            blocks: original
        )
        selection.extend(
            to: .init(blockID: original[1].id, utf16Offset: 4),
            blockIndex: 1,
            blocks: original,
            indexByID: originalIndices
        )

        let prepended = projector.project([
            TranscriptItem(id: "older", content: .markdown("older", syntax: .full, container: nil)),
        ] + originalPlan)
        let prependedIndices = Dictionary(uniqueKeysWithValues: prepended.indices.map { (prepended[$0].id, $0) })
        selection.reconcile(blocks: prepended, indexByID: prependedIndices)
        #expect(selection.localRange(for: original[0].id, blocks: prepended, indexByID: prependedIndices)
            == NSRange(location: 2, length: 3))
        #expect(selection.localRange(for: original[1].id, blocks: prepended, indexByID: prependedIndices)
            == NSRange(location: 0, length: 4))

        let removed = projector.project([
            TranscriptItem(id: "older", content: .markdown("older", syntax: .full, container: nil)),
            originalPlan[0],
        ])
        let removedIndices = Dictionary(uniqueKeysWithValues: removed.indices.map { (removed[$0].id, $0) })
        selection.reconcile(blocks: removed, indexByID: removedIndices)
        #expect(!selection.hasSelection)
    }

    @Test("selection normalizes Unicode boundaries and clamps around an inserted card")
    func selectionUnicodeAndInsertedBoundary() throws {
        let projector = TranscriptBlockProjector()
        let initial = projector.project([
            TranscriptItem(id: "unicode", content: .markdown("A👨‍👩‍👧‍👦B", syntax: .full, container: nil)),
            TranscriptItem(id: "tail", content: .markdown("tail", syntax: .full, container: nil)),
        ])
        let initialIndices = Dictionary(uniqueKeysWithValues: initial.indices.map { (initial[$0].id, $0) })
        let selection = TranscriptSelectionController()
        selection.begin(
            at: .init(blockID: initial[0].id, utf16Offset: 2),
            blockIndex: 0,
            blocks: initial
        )
        selection.extend(
            to: .init(blockID: initial[1].id, utf16Offset: 2),
            blockIndex: 1,
            blocks: initial,
            indexByID: initialIndices
        )
        #expect(selection.localRange(for: initial[0].id, blocks: initial, indexByID: initialIndices).location == 1)

        let card = TranscriptBlock(
            id: TranscriptBlockID(itemID: "card", generation: 999),
            itemID: "card",
            relation: .turn,
            content: .card
        )
        let divided = [initial[0], card, initial[1]]
        let dividedIndices = Dictionary(uniqueKeysWithValues: divided.indices.map { (divided[$0].id, $0) })
        selection.reconcile(blocks: divided, indexByID: dividedIndices)
        let firstLength = if case .text(let text) = divided[0].content { text.source.utf16.count } else { 0 }
        #expect(selection.localRange(for: initial[0].id, blocks: divided, indexByID: dividedIndices)
            == NSRange(location: 1, length: firstLength - 1))
        #expect(selection.localRange(for: initial[1].id, blocks: divided, indexByID: dividedIndices).length == 0)
    }

    @Test("materialized text exposes its controller-provided accessibility selection")
    func selectedRangeAccessibility() {
        let transcript = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 500, height: 80))
        transcript.configure(
            blockID: TranscriptBlockID(itemID: "reply", generation: 1),
            text: TranscriptBlock.Text(
                source: "accessible text",
                syntax: .full,
                container: nil,
                node: nil
            ),
            theme: .ottam(.builtIn),
            selectedRange: NSRange(location: 2, length: 6)
        )

        #expect(transcript.accessibilitySelectedTextRange() == NSRange(location: 2, length: 6))
        #expect(transcript.accessibilitySelectedText() == "cessib")
        transcript.prepareForReuse()
        #expect(transcript.accessibilitySelectedTextRange().length == 0)
    }

    @Test("materialized Core Text links and images are semantic accessibility children")
    func semanticAccessibilityChildren() throws {
        let markdown = "Read [docs](https://example.com/docs) and ![diagram](https://example.com/diagram.png)"
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(id: "reply", content: .markdown(markdown, syntax: .full, container: nil))
        ]).first)
        guard case .text(let text) = block.content else {
            Issue.record("expected a text block")
            return
        }
        let transcript = TranscriptTextItemView(
            frame: NSRect(x: 0, y: 0, width: 500, height: 100)
        )
        var opened: URL?
        transcript.openLink = { opened = $0 }
        transcript.configure(blockID: block.id, text: text, theme: .ottam(.builtIn))

        let children = try #require(transcript.accessibilityChildren())
            .compactMap { $0 as? NSAccessibilityElement }
        let link = try #require(children.first { $0.accessibilityRole() == .link })
        let image = try #require(children.first { $0.accessibilityRole() == .image })
        #expect(link.accessibilityLabel() == "docs")
        #expect(image.accessibilityLabel() == "diagram")
        #expect(link.accessibilityFrameInParentSpace() != NSRect.null)
        #expect(image.accessibilityFrameInParentSpace() != NSRect.null)
        #expect(link.accessibilityPerformPress())
        #expect(opened == URL(string: "https://example.com/docs"))
        let accessibilityValue = try #require(transcript.accessibilityValue() as? String)
        #expect(accessibilityValue.contains("\u{FFFC}") == false)
        #expect(accessibilityValue.utf16.count == transcript.accessibilityNumberOfCharacters())
        #expect(transcript.accessibilityVisibleCharacterRange().length == accessibilityValue.utf16.count)

        transcript.prepareForReuse()
        #expect(transcript.accessibilityChildren()?.isEmpty == true)
    }

    @Test("AppKit responder-chain Copy uses the controller-owned selection")
    func responderChainCopy() throws {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 500),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.contentView = controller.view
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "reply",
                    content: .markdown("copy through AppKit", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        let blockID = try #require(controller.debugBlockIDs().first)
        #expect(controller.debugSelect(range: NSRange(location: 0, length: 4), in: blockID))
        var copied: String?
        controller.copy = { copied = $0 }

        #expect(controller.debugCopyThroughResponderChain())
        #expect(copied == "copy")
    }

    @Test("horizontal collection-item reuse clears prose sizing and scroll position")
    func horizontalItemReuseResetsState() throws {
        let item = TranscriptTextCollectionItem()
        item.view.frame = NSRect(x: 0, y: 0, width: 240, height: 80)
        let theme = MarkdownTheme.ottam(.builtIn)
        let prose = TranscriptBlock(
            id: TranscriptBlockID(itemID: "prose", generation: 1),
            itemID: "prose",
            relation: .turn,
            content: .text(.init(source: "prose", syntax: .full, container: nil, node: nil))
        )
        item.configure(
            block: prose,
            theme: theme,
            copyMessage: {}
        )
        #expect(item.debugTextAutoresizingMask.contains(.width))

        func codeBlock(id: String) throws -> TranscriptBlock {
            let source = String(repeating: "abcdefghij", count: 80)
            let node = try #require(MarkdownBlockTreeParser.nodes(from: "```\n\(source)\n```").first)
            return TranscriptBlock(
                id: TranscriptBlockID(itemID: id, generation: UInt64(id == "code-a" ? 2 : 3)),
                itemID: id,
                relation: .turn,
                content: .text(.init(source: source, syntax: .full, container: nil, node: node))
            )
        }

        item.configure(
            block: try codeBlock(id: "code-a"),
            theme: theme,
            copyMessage: {}
        )
        #expect(!item.debugTextAutoresizingMask.contains(.width))
        item.debugScrollHorizontally(to: 60)
        #expect(item.debugHorizontalOffsetX > 0)

        item.prepareForReuse()
        item.configure(
            block: try codeBlock(id: "code-b"),
            theme: theme,
            copyMessage: {}
        )
        #expect(item.debugHorizontalOffsetX == 0)
    }

    @Test("fenced code renders through the shared tokenizer with pinned line height")
    func fencedCodeIsHighlighted() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let node = try #require(
            MarkdownBlockTreeParser.nodes(from: "```swift\nlet x = 1\n// note\n```").first
        )
        let text = TranscriptBlock.Text(
            source: "let x = 1\n// note", syntax: .full, container: nil, node: node
        )
        let attributed = TranscriptTextMeasurement.attributedString(for: text, theme: theme)

        let keyword = attributed.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor
        let comment = attributed.attribute(.foregroundColor, at: 10, effectiveRange: nil) as? NSColor
        #expect(keyword == theme.syntaxColors[.keyword])
        #expect(comment == theme.syntaxColors[.comment])
        let style = attributed.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        #expect(style?.minimumLineHeight == style?.maximumLineHeight)
        #expect((style?.minimumLineHeight ?? 0) > 0)
    }

    @Test("a fenced code item shows its language, copies raw code and measures header plus body")
    func fencedCodeItemChrome() throws {
        let item = TranscriptTextCollectionItem()
        item.view.frame = NSRect(x: 0, y: 0, width: 320, height: 120)
        let theme = MarkdownTheme.ottam(.builtIn)
        let source = "let x = 1\nprint(x)"
        let node = try #require(
            MarkdownBlockTreeParser.nodes(from: "```swift\n\(source)\n```").first
        )
        let text = TranscriptBlock.Text(source: source, syntax: .full, container: nil, node: node)
        let block = TranscriptBlock(
            id: TranscriptBlockID(itemID: "code", generation: 1),
            itemID: "code",
            relation: .turn,
            content: .text(text)
        )
        var copied: String?
        item.copyCode = { copied = $0 }
        item.configure(block: block, theme: theme, copyMessage: {})

        #expect(item.debugCodeLanguageLabel == "Swift")
        item.debugPressCopyCode()
        #expect(copied == source)
        #expect(item.debugShowsCopiedState)

        let header = TranscriptCodeBlockChrome.headerHeight(theme: theme)
        let lineHeight = TranscriptCodeBlockChrome.lineHeight(theme: theme)
        let insets = theme.metrics.codeBlockVerticalInset * 2
        let measured = TranscriptTextMeasurement.height(for: text, width: 320, theme: theme)
        #expect(item.measuredHeight(forWidth: 320) == measured)
        #expect(measured >= header + insets + lineHeight * 2)
        #expect(measured <= header + insets + lineHeight * 2 + 6)

        let bare = try #require(MarkdownBlockTreeParser.nodes(from: "```\n\(source)\n```").first)
        item.prepareForReuse()
        item.configure(
            block: TranscriptBlock(
                id: TranscriptBlockID(itemID: "bare", generation: 2),
                itemID: "bare",
                relation: .turn,
                content: .text(.init(source: source, syntax: .full, container: nil, node: bare))
            ),
            theme: theme,
            copyMessage: {}
        )
        #expect(item.debugCodeLanguageLabel == nil)
        #expect(!item.debugShowsCopiedState)
    }

    @Test("a legacy horizontal scroller adds its strip only when code overflows")
    func legacyScrollerInsetAppliesOnOverflow() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        func text(_ source: String) throws -> TranscriptBlock.Text {
            let node = try #require(MarkdownBlockTreeParser.nodes(from: "```\n\(source)\n```").first)
            return .init(source: source, syntax: .full, container: nil, node: node)
        }
        let wide = try text(String(repeating: "abcdefghij", count: 40))
        let narrow = try text("short")
        let legacy = TranscriptScrollableBlockOptions(horizontalScrollerInset: 15)

        #expect(
            TranscriptTextMeasurement.height(for: wide, width: 300, theme: theme, options: legacy)
                == TranscriptTextMeasurement.height(for: wide, width: 300, theme: theme) + 15
        )
        #expect(
            TranscriptTextMeasurement.height(for: narrow, width: 300, theme: theme, options: legacy)
                == TranscriptTextMeasurement.height(for: narrow, width: 300, theme: theme)
        )
        let artifact = TranscriptTextRenderArtifact(
            blockID: TranscriptBlockID(itemID: "wide", generation: 1),
            text: wide,
            width: 300,
            theme: theme,
            resourceRoot: nil,
            options: legacy
        )
        #expect(artifact.measuredHeight
            == TranscriptTextMeasurement.height(for: wide, width: 300, theme: theme, options: legacy))
    }

    @Test("an overflowing table keeps its last row above a legacy scroller")
    func legacyScrollerKeepsTableRowsVisible() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let source = "| a | b |\n|---|---|\n| \(String(repeating: "wide ", count: 40)) | x |\n| last row | y |"
        let node = try #require(MarkdownBlockTreeParser.nodes(from: source).first)
        let text = TranscriptBlock.Text(source: source, syntax: .full, container: nil, node: node)
        let width: CGFloat = 240
        let inset = NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
        let legacy = TranscriptScrollableBlockOptions(horizontalScrollerInset: inset)
        let body = TranscriptTextMeasurement.height(for: text, width: width, theme: theme)
        let measured = TranscriptTextMeasurement.height(for: text, width: width, theme: theme, options: legacy)
        #expect(measured == body + inset)

        let item = TranscriptTextCollectionItem()
        item.view.frame = NSRect(x: 0, y: 0, width: width, height: measured)
        let artifact = TranscriptTextRenderArtifact(
            blockID: TranscriptBlockID(itemID: "table", generation: 1),
            text: text,
            width: width,
            theme: theme,
            resourceRoot: nil,
            options: legacy
        )
        let block = TranscriptBlock(
            id: artifact.blockID, itemID: "table", relation: .turn, content: .text(text)
        )
        item.configure(block: block, artifact: artifact, theme: theme, copyMessage: {})
        item.debugSetScrollerStyle(.legacy)
        let header = TranscriptTableChrome.headerHeight(theme: theme)
        #expect(item.debugScrollClipHeight == body - header)
        #expect(item.debugScrollDocumentHeight == body - header, "the document must not run behind the bar")

        // Overlay scrollers take nothing; the plain measurement is the body.
        let overlayItem = TranscriptTextCollectionItem()
        overlayItem.view.frame = NSRect(x: 0, y: 0, width: width, height: body)
        overlayItem.configure(
            block: block,
            artifact: TranscriptTextRenderArtifact(
                blockID: artifact.blockID,
                text: text,
                width: width,
                theme: theme,
                resourceRoot: nil
            ),
            theme: theme,
            copyMessage: {}
        )
        overlayItem.debugSetScrollerStyle(.overlay)
        #expect(overlayItem.debugScrollClipHeight == body - header)
        #expect(overlayItem.debugScrollDocumentHeight == body - header)
    }

    @Test("a table a fraction short of its row stack still lays out its last row")
    func shortTableFrameKeepsLastRow() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let text = try tableText("| a | b |\n|---|---|\n| `x` | 1 |\n| `y` | 2 |\n| `z` | 3 |")
        let width: CGFloat = 400
        let measured = TranscriptTextMeasurement.height(for: text, width: width, theme: theme)
        let item = TranscriptTextCollectionItem()
        // Core Text drops a line that does not wholly fit its path; a clip
        // height a fraction under the measurement must not cost the last row.
        item.view.frame = NSRect(x: 0, y: 0, width: width, height: measured - 0.5)
        let artifact = TranscriptTextRenderArtifact(
            blockID: TranscriptBlockID(itemID: "table", generation: 1),
            text: text, width: width, theme: theme, resourceRoot: nil
        )
        let block = TranscriptBlock(id: artifact.blockID, itemID: "table", relation: .turn, content: .text(text))
        item.configure(block: block, artifact: artifact, theme: theme, copyMessage: {})
        item.debugSetScrollerStyle(.overlay)
        let rowHeight = TranscriptTableChrome.rowHeight(theme: theme)
        // One pill per body row, each inside its own row: the slack below the
        // grid must not shift the rows.
        let rows = item.debugInlineCodePillRects().map { Int($0.midY / rowHeight) }.sorted()
        #expect(rows == [1, 2, 3])
    }

    private func tableText(_ source: String) throws -> TranscriptBlock.Text {
        let block = try #require(TranscriptBlockProjector().project([
            TranscriptItem(id: "t", content: .markdown(source, syntax: .full, container: nil))
        ]).first)
        guard case .text(let text) = block.content else { throw TestFailure("expected text") }
        return text
    }

    private struct TestFailure: Error { let message: String; init(_ message: String) { self.message = message } }

    @Test("table cells land on measured stops in each column's alignment")
    func tableColumnsAlignOnMeasuredStops() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let text = try tableText("""
        | Name | Count | Note |
        |:-----|:-----:|-----:|
        | alpha | 1 | first |
        | a much longer name | 12345 | x |
        """)
        let layout = try #require(TranscriptTextMeasurement.tableLayout(for: text, theme: theme))
        let padding = TranscriptTableChrome.cellPadding
        #expect(layout.columns.count == 3)
        #expect(layout.columns[0].x == 0)
        #expect(layout.columns[1].x == layout.columns[0].width)
        #expect(layout.columns[2].x == layout.columns[0].width + layout.columns[1].width)
        #expect(layout.width == layout.columns[2].maxX)
        #expect(layout.columns.map(\.alignment) == [.left, .center, .right])

        // Real glyph positions on the second body row, not just the stops.
        let attributed = TranscriptTextMeasurement.attributedString(for: text, theme: theme)
        #expect(attributed.string == text.source, "layout characters equal the selection substrate")
        let lines = (attributed.string as NSString).components(separatedBy: "\n")
        let rowStart = lines[0].utf16.count + 1 + lines[1].utf16.count + 1
        let row = attributed.attributedSubstring(from: NSRange(location: rowStart, length: lines[2].utf16.count))
        let line = CTLineCreateWithAttributedString(row)
        let cells = lines[2].split(separator: "\t").map(String.init)
        // "\t" + "a much longer name" + "\t" + "12345" + "\t" + "x"
        let secondCellStart = 1 + cells[0].utf16.count + 1
        let thirdCellEnd = row.length
        #expect(CTLineGetOffsetForStringIndex(line, 1, nil) == padding, "first column is a left stop")
        let centerStart = CTLineGetOffsetForStringIndex(line, secondCellStart, nil)
        let centerEnd = CTLineGetOffsetForStringIndex(line, secondCellStart + cells[1].utf16.count, nil)
        #expect(abs((centerStart + centerEnd) / 2 - (layout.columns[1].x + layout.columns[1].width / 2)) < 1,
                "centered cell straddles its column's middle")
        #expect(abs(CTLineGetOffsetForStringIndex(line, thirdCellEnd, nil) - (layout.columns[2].maxX - padding)) < 1,
                "right-aligned cell ends at its column's trailing padding")

        // Overflow is decided by the grid, not by the last glyph.
        #expect(TranscriptTextMeasurement.naturalWidth(for: text, theme: theme) == layout.width)

        // A narrower grid stretches in proportion to fill the item; a wider
        // item never shrinks it.
        let stretched = try #require(TranscriptTextMeasurement.tableLayout(for: text, theme: theme, width: 900))
        #expect(stretched.width == 900)
        #expect(stretched.naturalWidth == layout.width)
        #expect(stretched.columns.allSatisfy { $0.width == floor($0.width) })
        #expect(stretched.columns[0].width > layout.columns[0].width)
        let narrow = try #require(TranscriptTextMeasurement.tableLayout(for: text, theme: theme, width: 100))
        #expect(narrow.width == layout.width)
    }

    @Test("table rows keep one pinned height, inline code or not")
    func tableRowHeightIsPinned() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let plain = try tableText("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |")
        let styled = try tableText("| a | **b** |\n|---|---|\n| `code` | [l](https://x.y) |\n| 3 | 4 |")
        let expected = TranscriptTableChrome.headerHeight(theme: theme)
            + 3 * TranscriptTableChrome.rowHeight(theme: theme)
        #expect(TranscriptTextMeasurement.height(for: plain, width: 400, theme: theme) == expected)
        #expect(TranscriptTextMeasurement.height(for: styled, width: 400, theme: theme) == expected)
        // The header row is the emboldened body face.
        let attributed = TranscriptTextMeasurement.attributedString(for: styled, theme: theme)
        let headerFont = attributed.attribute(.font, at: 1, effectiveRange: nil) as? NSFont
        #expect(headerFont?.fontDescriptor.symbolicTraits.contains(.bold) == true)

        // Rows land one row height apart, and the pin comes from paragraph
        // spacing, not an inflated line box: the line keeps its own ascent
        // and descent, so an inline-code background stays a tight pill.
        let rowHeight = TranscriptTableChrome.rowHeight(theme: theme)
        let framesetter = CTFramesetterCreateWithAttributedString(attributed)
        let path = CGPath(rect: CGRect(x: 0, y: 0, width: 400, height: 1000), transform: nil)
        let frame = CTFramesetterCreateFrame(framesetter, CFRange(location: 0, length: 0), path, nil)
        let lines = CTFrameGetLines(frame) as! [CTLine]
        var origins = [CGPoint](repeating: .zero, count: lines.count)
        CTFrameGetLineOrigins(frame, CFRange(location: 0, length: 0), &origins)
        #expect(lines.count == 3)
        // The item insets the frame by the shared row inset; from there each
        // row's baseline is its row's top plus its own ascent.
        let inset = TranscriptTableLayout.rowInset(in: attributed)
        #expect(inset >= 4)
        #expect(TranscriptTextMeasurement.geometry(for: styled, width: 400, theme: theme, attributed: attributed)
            .verticalInset == inset)
        for (index, line) in lines.enumerated() {
            var ascent: CGFloat = 0
            var descent: CGFloat = 0
            CTLineGetTypographicBounds(line, &ascent, &descent, nil)
            #expect(ascent + descent < rowHeight - 4, "line box is the text's own, not the row's")
            // Baselines are exactly one row apart: the pitch is whole points
            // by construction, whatever the fonts' fractional metrics.
            if index > 0 {
                #expect(abs((origins[index - 1].y - origins[index].y) - rowHeight) < 0.01, "row \(index) pitch")
            }
            // Every row's line, inset included, fits above its rule.
            #expect(inset + ascent + descent <= rowHeight - inset + 0.5, "row \(index) fits")
        }
        // The first baseline sits inside its row, ascent below the row top.
        let firstBaselineFromTop = 1000 - origins[0].y
        #expect(firstBaselineFromTop > 0 && firstBaselineFromTop < rowHeight - inset)
    }

    @Test("a whole table copies as a pipe table while a partial range stays cells")
    func selectionCopyReconstructsWholeTables() throws {
        let plan = [
            TranscriptItem(
                id: "reply",
                content: .markdown(
                    "Intro\n\n| Name | N |\n|:-----|--:|\n| `a` | 1 |\n| **b** | 22 |\n\nOutro",
                    syntax: .full,
                    container: nil
                )
            )
        ]
        let blocks = TranscriptBlockProjector().project(plan)
        let indices = Dictionary(uniqueKeysWithValues: blocks.indices.map { (blocks[$0].id, $0) })
        let selection = TranscriptSelectionController()
        #expect(blocks.count == 3)
        let table = blocks[1]
        guard case .text(let text) = table.content else { return }

        selection.begin(at: .init(blockID: table.id, utf16Offset: 0), blockIndex: 1, blocks: blocks)
        selection.extend(
            to: .init(blockID: table.id, utf16Offset: text.source.utf16.count),
            blockIndex: 1, blocks: blocks, indexByID: indices
        )
        let copied = try #require(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices))
        #expect(copied == "| Name  |   N |\n| ----- | --: |\n| `a`   |   1 |\n| **b** |  22 |")
        // The copy parses back to the same grid.
        #expect(MarkdownBlockTreeParser.nodes(from: copied).map(\.kind) == [text.node?.kind])

        selection.begin(at: .init(blockID: table.id, utf16Offset: 1), blockIndex: 1, blocks: blocks)
        selection.extend(
            to: .init(blockID: table.id, utf16Offset: 7),
            blockIndex: 1, blocks: blocks, indexByID: indices
        )
        #expect(selection.copiedText(blocks: blocks, sourcePlan: plan, indexByID: indices) == "Name\tN")

        #expect(TranscriptTableSource.pipeTable(
            header: [AttributedString("a|b")], rows: [[AttributedString("")]], alignments: [.center]
        ) == "| a\\|b |\n| :--: |\n|      |")
    }

    @Test("a copied cell parses back to the same cell: escapes, code fences, styles across runs")
    func tableCellSourceRoundTrips() throws {
        let cells = [
            "\\*not emphasis\\* and \\`not code\\` \\[x\\]",
            "`` `a` `` and ``a`b``",
            "**bold *and italic* bold**",
            "[**bold** link](https://x.y/p) ~~gone~~",
            "*em **strong em** em* `code` tail",
        ]
        let source = "| c |\n|---|\n" + cells.map { "| \($0) |" }.joined(separator: "\n")
        let text = try tableText(source)
        guard case .table(let header, let rows, let alignments)? = text.node?.kind else {
            throw TestFailure("expected table")
        }
        let copied = TranscriptTableSource.pipeTable(header: header, rows: rows, alignments: alignments)
        let again = try tableText(copied)
        guard case .table(let header2, let rows2, _)? = again.node?.kind else { throw TestFailure("expected table") }
        #expect(header2 == header)
        #expect(rows2 == rows)
        #expect(rows2.count == cells.count)
    }

    @Test("table links wear the prose link appearance and images in cells are their alt text")
    func tableLinksAndImages() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let text = try tableText("| a | b |\n|---|---|\n| [docs](https://x.y) | ![pic](https://x.y/p.png) |")
        let attributed = TranscriptTextMeasurement.attributedString(for: text, theme: theme)
        let string = attributed.string
        let linkRange = (string as NSString).range(of: "docs")
        let attributes = attributed.attributes(at: linkRange.location, effectiveRange: nil)
        #expect(attributes[.link] != nil)
        #expect((attributes[.foregroundColor] as? NSColor) == NSColor.linkColor)
        #expect(attributes[.underlineStyle] == nil)
        // No attachment: the cell shows "pic", the display source agrees, and
        // the row height is the pinned one.
        #expect(string.contains("pic"))
        #expect(!string.contains("\u{FFFC}"))
        #expect(text.source == attributed.string)
        var hasAttachment = false
        attributed.enumerateAttribute(.attachment, in: NSRange(location: 0, length: attributed.length)) { value, _, _ in
            if value != nil { hasAttachment = true }
        }
        #expect(!hasAttachment)
        #expect(TranscriptTextMeasurement.height(for: text, width: 400, theme: theme)
            == 2 * TranscriptTableChrome.rowHeight(theme: theme))
        // The copy keeps the image syntax.
        guard case .table(let header, let rows, let alignments)? = text.node?.kind else { throw TestFailure("expected table") }
        #expect(TranscriptTableSource.pipeTable(header: header, rows: rows, alignments: alignments)
            .contains("![pic](https://x.y/p.png)"))
    }

    @Test("a table item wears the bare tile: no header band, no Copy, no fold")
    func tableItemWearsBareTile() throws {
        let theme = MarkdownTheme.ottam(.builtIn)
        let text = try tableText("| a | b |\n|---|---|\n| 1 | 2 |")
        let item = TranscriptTextCollectionItem()
        item.view.frame = NSRect(x: 0, y: 0, width: 320, height: 200)
        var copied: String?
        item.copyCode = { copied = $0 }
        item.configure(
            block: TranscriptBlock(
                id: TranscriptBlockID(itemID: "t", generation: 1),
                itemID: "t", relation: .turn, content: .text(text)
            ),
            theme: theme,
            copyMessage: {}
        )
        #expect(item.debugCodeLanguageLabel == nil)
        #expect(item.debugCodeExpandTitle == nil)
        #expect(TranscriptTableChrome.headerHeight(theme: theme) == 0)
        item.debugPressCopyCode()
        #expect(copied == nil)
        #expect(!item.debugShowsCopiedState)
    }

    @Test("a streamed pipe table is one surface from its delimiter row on, never a paragraph first")
    func streamedTableKeepsOneStableSurface() throws {
        let full = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |"
        let delimiterEnd = try #require(full.range(of: "|---|---|\n")).upperBound
        for cut in 1...full.count {
            let text = String(full.prefix(cut))
            let projector = TranscriptBlockProjector()
            var items = [TranscriptItem(id: "r", content: .markdown("", syntax: .full, container: nil))]
            _ = projector.project(items)
            var tableIDs = Set<TranscriptBlockID>()
            var pipeParagraphs = 0
            for ch in text {
                _ = projector.append(String(ch), to: "r", in: &items)
                for block in projector.projectedUpdate(itemID: "r", relation: .turn)?.blocks ?? [] {
                    guard case .text(let t) = block.content else { continue }
                    switch t.node?.kind {
                    case .table?: tableIDs.insert(block.id)
                    case .paragraph? where t.source.hasPrefix("|"): pipeParagraphs += 1
                    default: break
                    }
                }
            }
            let settled = projector.project(items)
            let fresh = TranscriptBlockProjector().project(items)
            #expect(settled.map(\.content) == fresh.map(\.content), "cut \(cut)")
            #expect(pipeParagraphs == 0, "cut \(cut): the header row never shows as prose")
            if text.endIndex >= delimiterEnd {
                #expect(tableIDs.count == 1, "cut \(cut): one table identity while streaming")
            }
        }
    }

    @Test("pipe prose is published as soon as its second line cannot be a delimiter row")
    func pipeProseStopsDeferringOnceDelimiterIsImpossible() throws {
        let projector = TranscriptBlockProjector()
        var items = [TranscriptItem(id: "r", content: .markdown("", syntax: .full, container: nil))]
        _ = projector.project(items)
        func stream(_ delta: String) -> TranscriptBlock.Text? {
            _ = projector.append(delta, to: "r", in: &items)
            for block in projector.projectedUpdate(itemID: "r", relation: .turn)?.blocks ?? [] {
                if case .text(let text) = block.content, text.source.hasPrefix("| note") { return text }
            }
            return nil
        }
        // After a settled intro, a lone pipe line and a second line that
        // could still be `|---|` stay held back.
        _ = stream("intro\n\n")
        #expect(stream("| note") == nil)
        #expect(stream("\n") == nil)
        #expect(stream("|-") == nil)
        // The first character that cannot be part of a delimiter row settles
        // it as prose, published at once.
        let text = try #require(stream("c"))
        guard case .paragraph? = text.node?.kind else { throw TestFailure("expected paragraph") }
        #expect(text.source.hasSuffix("|-c"))
    }

    @Test("the code tile forwards vertical wheel gestures and keeps horizontal ones")
    func horizontalScrollViewForwardsVerticalGestures() throws {
        final class Recorder: NSView {
            var forwarded = 0
            override func scrollWheel(with event: NSEvent) { forwarded += 1 }
        }
        let recorder = Recorder(frame: NSRect(x: 0, y: 0, width: 200, height: 100))
        let scrollView = TranscriptHorizontalScrollView(frame: recorder.bounds)
        scrollView.hasHorizontalScroller = true
        scrollView.hasVerticalScroller = false
        recorder.addSubview(scrollView)
        let document = NSView(frame: NSRect(x: 0, y: 0, width: 600, height: 100))
        scrollView.documentView = document

        func wheel(x: Int32, y: Int32) throws -> NSEvent {
            guard let cgEvent = CGEvent(
                scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2, wheel1: y, wheel2: x, wheel3: 0
            ) else { throw TestFailure("could not create scroll wheel CGEvent") }
            guard let event = NSEvent(cgEvent: cgEvent) else {
                throw TestFailure("could not create scroll wheel NSEvent")
            }
            return event
        }

        scrollView.scrollWheel(with: try wheel(x: 0, y: 10))
        #expect(recorder.forwarded == 1)
        scrollView.scrollWheel(with: try wheel(x: 3, y: 10))
        #expect(recorder.forwarded == 2)
        scrollView.scrollWheel(with: try wheel(x: 10, y: 2))
        #expect(recorder.forwarded == 2)

        document.frame.size.width = 200
        scrollView.scrollWheel(with: try wheel(x: 10, y: 0))
        #expect(recorder.forwarded == 3)
    }

    @Test("an overflowing code item shows a trailing cue until scrolled to its end")
    func overflowCueFollowsScroll() throws {
        let item = TranscriptTextCollectionItem()
        item.view.frame = NSRect(x: 0, y: 0, width: 240, height: 120)
        let theme = MarkdownTheme.ottam(.builtIn)
        let source = String(repeating: "abcdefghij", count: 80)
        let node = try #require(MarkdownBlockTreeParser.nodes(from: "```\n\(source)\n```").first)
        item.configure(
            block: TranscriptBlock(
                id: TranscriptBlockID(itemID: "wide", generation: 1),
                itemID: "wide",
                relation: .turn,
                content: .text(.init(source: source, syntax: .full, container: nil, node: node))
            ),
            theme: theme,
            copyMessage: {}
        )
        item.debugSetScrollerStyle(.overlay)
        #expect(item.debugShowsOverflowCue)
        item.debugScrollHorizontally(to: 100_000)
        #expect(!item.debugShowsOverflowCue)

        item.debugSetScrollerStyle(.legacy)
        item.debugScrollHorizontally(to: 0)
        #expect(!item.debugShowsOverflowCue, "a legacy scroller is its own overflow cue")
    }

    @Test("a long code item shows the fold control and reports its toggle")
    func fencedCodeItemFoldControl() throws {
        let item = TranscriptTextCollectionItem()
        item.view.frame = NSRect(x: 0, y: 0, width: 320, height: 400)
        let theme = MarkdownTheme.ottam(.builtIn)
        let source = (1...81).map { "line \($0)" }.joined(separator: "\n")
        let node = try #require(MarkdownBlockTreeParser.nodes(from: "```\n\(source)\n```").first)
        let id = TranscriptBlockID(itemID: "long", generation: 1)
        let block = TranscriptBlock(
            id: id,
            itemID: "long",
            relation: .turn,
            content: .text(.init(source: source, syntax: .full, container: nil, node: node))
        )
        var toggled: TranscriptBlockID?
        item.toggleExpansion = { toggled = $0 }
        item.configure(block: block, theme: theme, copyMessage: {})
        #expect(item.debugCodeExpandTitle == "Show all 81 lines")
        item.debugPressExpandCode()
        #expect(toggled == id)

        let expanded = TranscriptTextRenderArtifact(
            blockID: id,
            text: .init(source: source, syntax: .full, container: nil, node: node),
            width: 320,
            theme: theme,
            resourceRoot: nil,
            options: .init(isExpanded: true)
        )
        item.configure(block: block, artifact: expanded, theme: theme, copyMessage: {})
        #expect(item.debugCodeExpandTitle == "Show less")

        let short = (1...80).map { "line \($0)" }.joined(separator: "\n")
        let shortNode = try #require(MarkdownBlockTreeParser.nodes(from: "```\n\(short)\n```").first)
        item.prepareForReuse()
        item.configure(
            block: TranscriptBlock(
                id: TranscriptBlockID(itemID: "short", generation: 2),
                itemID: "short",
                relation: .turn,
                content: .text(.init(source: short, syntax: .full, container: nil, node: shortNode))
            ),
            theme: theme,
            copyMessage: {}
        )
        #expect(item.debugCodeExpandTitle == nil)
    }

    @Test("a long code block folds at the line limit and unfolds through the height transaction")
    func codeBlockFoldToggle() throws {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let theme = MarkdownTheme.ottam(.builtIn)
        let lines = (1...100).map { "line \($0)" }.joined(separator: "\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "reply",
                    content: .markdown("```swift\n\(lines)\n```", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: theme
        )
        let id = try #require(controller.debugBlockIDs().first)
        let header = TranscriptCodeBlockChrome.headerHeight(theme: theme)
        let footer = TranscriptCodeBlockChrome.footerHeight(theme: theme)
        let lineHeight = TranscriptCodeBlockChrome.lineHeight(theme: theme)
        let insets = theme.metrics.codeBlockVerticalInset * 2

        let collapsed = try #require(controller.debugBlockHeights().first)
        #expect(!controller.debugIsCodeBlockExpanded(id))
        // Exactly the leading 80 lines: at least 80 pinned lines, fewer than
        // 81 lines even with the styler's 2pt inter-line spacing.
        #expect(collapsed >= header + footer + insets + lineHeight * 80)
        #expect(collapsed < header + footer + insets + (lineHeight + 2) * 81)

        controller.toggleCodeBlockExpansion(id)
        #expect(controller.debugIsCodeBlockExpanded(id))
        let expanded = try #require(controller.debugBlockHeights().first)
        #expect(expanded >= header + footer + insets + lineHeight * 100)
        #expect(controller.debugFrame(for: id)?.height == expanded)

        controller.toggleCodeBlockExpansion(id)
        #expect(!controller.debugIsCodeBlockExpanded(id))
        #expect(controller.debugBlockHeights().first == collapsed)
    }

    @Test("an open fence past the fold streams at a constant height")
    func streamingFencedCodeFoldsLive() throws {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let opening = "```swift\n" + (1...85).map { "line \($0)" }.joined(separator: "\n")
        controller.apply(
            plan: [
                TranscriptItem(id: "live", content: .markdown(opening, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let id = try #require(controller.debugBlockIDs().last)
        let folded = try #require(controller.debugBlockHeights().last)

        #expect(controller.appendProjectedText("\nline 86\nline 87", to: "live"))
        #expect(controller.debugBlockIDs().last == id)
        #expect(controller.debugBlockHeights().last == folded)
        #expect(controller.debugCompletedMarkdownText(itemID: "live")?.hasSuffix("line 87") == true)
    }

    @Test("short user bubbles hug their text while long bubbles keep the width cap")
    func bubbleWidthFitsContent() {
        let theme = MarkdownTheme.ottam(.builtIn)
        func geometry(_ source: String) -> TranscriptTextMeasurement.Geometry {
            let text = TranscriptBlock.Text(
                source: source,
                syntax: .inline,
                container: .bubble,
                node: nil
            )
            let attributed = TranscriptTextMeasurement.attributedString(for: text, theme: theme)
            return TranscriptTextMeasurement.geometry(
                for: text,
                width: 700,
                theme: theme,
                attributed: attributed
            )
        }

        let short = geometry("Hello")
        let long = geometry(String(repeating: "a comfortably wrapping sentence ", count: 30))
        let shortBubbleWidth = short.textWidth + short.horizontalInset * 2
        let longBubbleWidth = long.textWidth + long.horizontalInset * 2

        // Right-to-left lines lay out flush right; their offset is not width.
        let rtl = geometry("שלום")

        #expect(shortBubbleWidth < 100)
        #expect(rtl.textWidth + rtl.horizontalInset * 2 < 100)
        #expect(longBubbleWidth > 500)
        #expect(longBubbleWidth <= theme.metrics.bubbleMaxWidth)
    }

    @Test("a native text item resolves and loads an image inside the conversation root")
    func nativeTextItemLoadsRelativeImage() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-native-image-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let image = NSImage(size: NSSize(width: 8, height: 4))
        image.lockFocus()
        NSColor.systemBlue.setFill()
        NSRect(x: 0, y: 0, width: 8, height: 4).fill()
        image.unlockFocus()
        let tiff = try #require(image.tiffRepresentation)
        let png = try #require(
            NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:])
        )
        try png.write(to: root.appendingPathComponent("image.png"))

        let node = try #require(
            MarkdownBlockTreeParser.nodes(from: "![tiny](image.png)", syntax: .full).first
        )
        let text = TranscriptBlock.Text(
            source: String(node.inline.characters),
            syntax: .full,
            container: nil,
            node: node
        )
        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 700, height: 200))
        var layoutChanges = 0
        view.onAttachmentLayoutChanged = { layoutChanges += 1 }
        view.configure(
            blockID: TranscriptBlockID(itemID: "image", generation: 1),
            text: text,
            theme: .ottam(.builtIn),
            resourceRoot: root
        )

        for _ in 0..<100 where view.debugLoadedImageCount == 0 {
            try? await Task.sleep(for: .milliseconds(10))
        }
        #expect(view.debugLoadedImageCount == 1)
        #expect(layoutChanges == 1)
    }

    @Test("a failed native image keeps its compact Core Text footprint")
    func failedNativeImageKeepsCompactCoreTextFootprint() async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-native-missing-image-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let node = try #require(
            MarkdownBlockTreeParser.nodes(from: "![missing](gone.png)", syntax: .full).first
        )
        let text = TranscriptBlock.Text(
            source: String(node.inline.characters),
            syntax: .full,
            container: nil,
            node: node
        )
        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 700, height: 200))
        var layoutChanges = 0
        view.onAttachmentLayoutChanged = { layoutChanges += 1 }
        view.configure(
            blockID: TranscriptBlockID(itemID: "missing-image", generation: 1),
            text: text,
            theme: .ottam(.builtIn),
            resourceRoot: root
        )
        let provisionalHeight = try #require(view.measuredHeight(forWidth: 700))

        for _ in 0..<100 where layoutChanges == 0 {
            try? await Task.sleep(for: .milliseconds(10))
        }
        let failedHeight = try #require(view.measuredHeight(forWidth: 700))

        #expect(layoutChanges == 1)
        #expect(abs(failedHeight - provisionalHeight) < 0.5)
    }

    @Test("a loaded image refreshes artifact geometry when rematerialized after recycling")
    func rematerializedLoadedImageRefreshesGeometry() throws {
        let node = try #require(
            MarkdownBlockTreeParser.nodes(from: "![loaded](image.png)", syntax: .full).first
        )
        let blockID = TranscriptBlockID(itemID: "image", generation: 1)
        let artifact = TranscriptTextRenderArtifact(
            blockID: blockID,
            text: .init(source: "\u{FFFC}", syntax: .full, container: nil, node: node),
            width: 700,
            theme: .ottam(.builtIn),
            resourceRoot: nil
        )
        let placeholderHeight = artifact.measuredHeight
        var attachment: MarkdownImageAttachment?
        artifact.attributed.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: artifact.attributed.length)
        ) { value, _, _ in
            attachment = value as? MarkdownImageAttachment
        }
        let loaded = try #require(attachment)
        loaded.finishLoading(image: NSImage(size: NSSize(width: 1, height: 1)), size: CGSize(width: 800, height: 400))
        let view = TranscriptTextItemView(frame: NSRect(x: 0, y: 0, width: 700, height: 400))
        var layoutChanges = 0
        view.onAttachmentLayoutChanged = { layoutChanges += 1 }

        view.configure(artifact: artifact)

        #expect(layoutChanges == 1)
        #expect(artifact.measuredHeight > placeholderHeight)
    }

    @Test("stream append mutates only the granted source item")
    func appendSource() throws {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "old", content: .markdown("old", syntax: .full, container: nil)),
            TranscriptItem(id: "live", content: .markdown("base", syntax: .full, container: nil)),
        ]
        #expect(projector.append(" suffix", to: "live", in: &plan))
        #expect(plan[0].markdownText == "old")
        #expect(plan[1].markdownText == "base suffix")
    }

    @Test("streaming seals settled blocks and bounds the reparsed tail")
    func boundedStreamingTail() throws {
        let projector = TranscriptBlockProjector()
        let source = (0..<100).map { "Paragraph \($0) with settled content." }
            .joined(separator: "\n\n") + "\n\nOpen tail"
        var plan = [
            TranscriptItem(id: "live", content: .markdown(source, syntax: .full, container: nil))
        ]
        let before = projector.project(plan)

        #expect(projector.append(" grows", to: "live", in: &plan))
        let after = projector.project(plan)

        #expect(before.count == after.count)
        #expect(Array(before.dropLast().map(\.id)) == Array(after.dropLast().map(\.id)))
        #expect(try #require(projector.debugUnsealedTailBytes(itemID: "live")) < 2_048)
    }

    @Test("ordinary streaming text bypasses Markdown reparsing")
    func plainStreamingBypassesParser() throws {
        let source = String(repeating: "A settled ordinary sentence with no markup. ", count: 3_000)
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "live", content: .markdown(source, syntax: .full, container: nil))
        ]
        _ = projector.project(plan)

        #expect(projector.append("More ordinary words arrive.", to: "live", in: &plan))
        #expect(projector.debugLastAppendParsedBytes(itemID: "live") == 0)

        let incremental = projector.project(plan)
        let rebuilt = TranscriptBlockProjector().project(plan)
        #expect(renderedSources(incremental) == renderedSources(rebuilt))
    }

    @Test("ordinary soft-line streaming bypasses Markdown reparsing")
    func softLineStreamingBypassesParser() {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(
                id: "live",
                content: .markdown("one\ntwo", syntax: .full, container: nil)
            )
        ]
        _ = projector.project(plan)

        #expect(projector.append("\nthree", to: "live", in: &plan))
        #expect(projector.debugLastAppendParsedBytes(itemID: "live") == 0)

        let incremental = projector.project(plan)
        let rebuilt = TranscriptBlockProjector().project(plan)
        #expect(renderedSources(incremental) == renderedSources(rebuilt))
    }

    @Test("plain streaming across a trailing-space tick matches a fresh parse")
    func trailingSpaceStreamingConverges() {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "live", content: .markdown("word", syntax: .full, container: nil))
        ]
        _ = projector.project(plan)

        #expect(projector.append(" ", to: "live", in: &plan))
        #expect(projector.append("next", to: "live", in: &plan))

        let incremental = projector.project(plan)
        let rebuilt = TranscriptBlockProjector().project(plan)
        #expect(renderedSources(incremental) == renderedSources(rebuilt))
        #expect(renderedSources(incremental) == ["word next"])
    }

    @Test("setext and hard-break boundaries leave the plain streaming fast path")
    func structuralLineBoundariesReparse() {
        for (initial, delta) in [("Title", "\n---"), ("Title", "\n==="), ("line  ", "\nnext")] {
            let projector = TranscriptBlockProjector()
            var plan = [
                TranscriptItem(id: "live", content: .markdown(initial, syntax: .full, container: nil))
            ]
            _ = projector.project(plan)

            #expect(projector.append(delta, to: "live", in: &plan))
            #expect((projector.debugLastAppendParsedBytes(itemID: "live") ?? 0) > 0)

            let incremental = projector.project(plan)
            let rebuilt = TranscriptBlockProjector().project(plan)
            #expect(incremental.map(\.content) == rebuilt.map(\.content))
        }
    }

    @Test("an open fenced block streams without reparsing its accumulated body")
    func openFenceStreamingBypassesParser() {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(
                id: "live",
                content: .markdown("```swift\nlet first = 1\n", syntax: .full, container: nil)
            )
        ]
        _ = projector.project(plan)

        #expect(projector.append("let second = 2\n", to: "live", in: &plan))
        #expect(projector.debugLastAppendParsedBytes(itemID: "live") == 0)

        let incremental = projector.project(plan)
        let rebuilt = TranscriptBlockProjector().project(plan)
        #expect(renderedSources(incremental) == renderedSources(rebuilt))
    }

    @Test("a streamed fence language stays metadata instead of entering the code body")
    func streamedFenceLanguageDoesNotBecomeCode() throws {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "live", content: .markdown("", syntax: .full, container: nil))
        ]
        _ = projector.project(plan)

        for chunk in ["```", "swift", "\n", "let value = 1"] {
            #expect(projector.append(chunk, to: "live", in: &plan))
        }

        let incremental = projector.project(plan)
        let rebuilt = TranscriptBlockProjector().project(plan)
        #expect(incremental.map(\.content) == rebuilt.map(\.content))
        let block = try #require(incremental.last)
        guard case .text(let text) = block.content,
              case .codeBlock(let language, let code) = text.node?.kind
        else {
            Issue.record("expected a fenced code block")
            return
        }
        #expect(language == "swift")
        #expect(code == "let value = 1")
    }

    @Test("incremental Markdown is invariant at every character split")
    func chunkingIsInvisible() {
        let fixtures = [
            "# Heading",
            "## Heading",
            "###### Heading",
            "> quoted",
            ">> nested quote",
            "- item",
            "* item",
            "+ item",
            "1. item",
            "1) item",
            "12. item",
            "Intro **bold** and *italic* with `code`.\n\n- first\n- second",
            "Before ![alt](image.png) after.\n\n> quoted\n\n---",
            "```swift\nlet value = 1\n```\n\n| A | B |\n| - | - |\n| 1 | 2 |",
        ]

        for source in fixtures {
            let expected = TranscriptBlockProjector().project([
                TranscriptItem(id: "live", content: .markdown(source, syntax: .full, container: nil))
            ]).map(\.content)
            let boundaries = Array(source.indices) + [source.endIndex]
            for boundary in boundaries {
                let projector = TranscriptBlockProjector()
                var plan = [
                    TranscriptItem(id: "live", content: .markdown("", syntax: .full, container: nil))
                ]
                _ = projector.project(plan)
                let prefix = String(source[..<boundary])
                let suffix = String(source[boundary...])
                if !prefix.isEmpty { #expect(projector.append(prefix, to: "live", in: &plan)) }
                if !suffix.isEmpty { #expect(projector.append(suffix, to: "live", in: &plan)) }
                #expect(projector.project(plan).map(\.content) == expected)
            }

            let projector = TranscriptBlockProjector()
            var plan = [
                TranscriptItem(id: "live", content: .markdown("", syntax: .full, container: nil))
            ]
            _ = projector.project(plan)
            for character in source {
                #expect(projector.append(String(character), to: "live", in: &plan))
            }
            #expect(projector.project(plan).map(\.content) == expected)
        }
    }

    @Test("streaming defers only ambiguous fence prefixes")
    func streamingFencePrefixIsStable() throws {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(
                id: "live",
                content: .markdown("A typical run looks like:\n", syntax: .full, container: nil)
            )
        ]
        let stable = projector.project(plan)

        #expect(projector.append("`", to: "live", in: &plan))
        #expect(projector.projectedUpdate(itemID: "live", relation: .turn)?.blocks.isEmpty == true)

        #expect(projector.append("`", to: "live", in: &plan))
        #expect(projector.projectedUpdate(itemID: "live", relation: .turn)?.blocks.isEmpty == true)
        #expect(renderedSources(stable) == ["A typical run looks like:"])

        #expect(projector.append("`text", to: "live", in: &plan))
        let projected = try #require(
            projector.projectedUpdate(itemID: "live", relation: .turn)
        ).blocks
        let code = try #require(projected.last)
        guard case .text(let text) = code.content,
              case .codeBlock(let language, _) = text.node?.kind
        else {
            Issue.record("expected the third backtick to establish a code block")
            return
        }
        #expect(language == "text")
    }

    @Test("streaming inline code waits for its closing delimiter")
    func streamingInlineCodeIsStable() throws {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "live", content: .markdown("Use ", syntax: .full, container: nil))
        ]
        let stable = projector.project(plan)

        #expect(projector.append("`", to: "live", in: &plan))
        #expect(projector.append("value", to: "live", in: &plan))
        #expect(projector.projectedUpdate(itemID: "live", relation: .turn)?.blocks.isEmpty == true)
        #expect(renderedSources(stable) == ["Use"])

        #expect(projector.append("` now", to: "live", in: &plan))
        let projected = try #require(
            projector.projectedUpdate(itemID: "live", relation: .turn)
        ).blocks
        let block = try #require(projected.first)
        guard case .text(let text) = block.content, let node = text.node else {
            Issue.record("expected projected inline Markdown")
            return
        }
        #expect(String(node.inline.characters) == "Use value now")
        #expect(node.inline.runs.contains {
            $0.inlinePresentationIntent?.contains(.code) == true
        })
    }

    @Test("settling releases an unmatched inline delimiter as canonical text")
    func settlingReleasesDeferredInlineDelimiter() {
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "live", content: .markdown("Use ", syntax: .full, container: nil))
        ]
        _ = projector.project(plan)
        #expect(projector.append("`unfinished", to: "live", in: &plan))
        #expect(projector.projectedUpdate(itemID: "live", relation: .turn)?.blocks.isEmpty == true)

        let settled = projector.project(plan)
        #expect(renderedSources(settled) == ["Use `unfinished"])
    }

    @Test("a long streamed ordered list keeps a bounded Markdown tail")
    func orderedListStreamingTailIsBounded() throws {
        let source = (1...240).map { "\($0). Settled ordered item \($0)." }
            .joined(separator: "\n")
        let projector = TranscriptBlockProjector()
        var plan = [
            TranscriptItem(id: "live", content: .markdown(source, syntax: .full, container: nil))
        ]
        _ = projector.project(plan)

        #expect(projector.append("\n241. Newly streamed ordered item.", to: "live", in: &plan))
        #expect(try #require(projector.debugUnsealedTailBytes(itemID: "live")) < 2_048)

        let incremental = projector.project(plan)
        let rebuilt = TranscriptBlockProjector().project(plan)
        #expect(renderedSources(incremental) == renderedSources(rebuilt))
    }

    @Test("live glyph animation stays bounded and honors Reduce Motion")
    func liveAnimationBounds() {
        let coordinator = LiveBatchAnimationCoordinator()
        for index in 0..<8 {
            coordinator.append(
                range: NSRange(location: index * 100, length: 100),
                reduceMotion: false,
                now: 10
            )
        }
        #expect(coordinator.active(at: 10).count == 4)
        #expect(coordinator.active(at: 10).reduce(0, { $0 + $1.range.length }) <= 512)
        coordinator.append(range: NSRange(location: 900, length: 20), reduceMotion: true, now: 10)
        #expect(coordinator.active(at: 10).count == 4)
        #expect(coordinator.active(at: 11).isEmpty)
    }

    @Test("settling and viewport growth keep bottom items materialized")
    func settlingKeepsVisibleItems() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<200).map { "## Section \($0)\n\nParagraph \($0) with enough text to wrap across the reading column." }
            .joined(separator: "\n\n")
        let plan = [
            TranscriptItem(id: "live", content: .markdown(source, syntax: .full, container: nil))
        ]
        controller.apply(
            plan: plan,
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()
        try? await Task.sleep(for: .milliseconds(50))
        let initial = controller.debugLayoutState()
        #expect(initial.visibleAttributeCount > 0)
        #expect(initial.materializedItemCount > 0, "\(initial)")

        controller.view.frame.size.height = 560
        controller.view.layoutSubtreeIfNeeded()
        try? await Task.sleep(for: .milliseconds(50))
        let settled = controller.debugLayoutState()
        #expect(settled.blockCount == 400)
        #expect(settled.visibleAttributeCount > 0)
        #expect(settled.materializedItemCount > 0, "\(settled)")
        #expect(abs(settled.offsetY - settled.maximumOffsetY) <= 2)

        let shortenedSource = (0..<196).map {
            "## Section \($0)\n\nParagraph \($0) with enough text to wrap across the reading column."
        }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown(shortenedSource, syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        try? await Task.sleep(for: .milliseconds(100))
        let structurallySettled = controller.debugLayoutState()
        #expect(structurallySettled.blockCount == 392)
        #expect(structurallySettled.visibleAttributeCount > 0)
        #expect(structurallySettled.materializedItemCount > 0, "\(structurallySettled)")
        #expect(structurallySettled.maximumOffsetY < settled.maximumOffsetY)
        #expect(abs(structurallySettled.offsetY - structurallySettled.maximumOffsetY) <= 2)
    }

    @Test("a short first exchange clears automatic titlebar and composer insets")
    func shortExchangeUsesInsetScrollRange() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.debugSetContentInsets(NSEdgeInsets(top: 60, left: 0, bottom: 100, right: 0))
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "first",
                    content: .markdown("Hello!", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()
        try? await Task.sleep(for: .milliseconds(50))

        let state = controller.debugLayoutState()
        #expect(state.maximumOffsetY == -60)
        #expect(state.offsetY == state.maximumOffsetY)
        #expect(state.materializedItemCount > 0, "\(state)")
    }

    @Test("dynamic titlebar and composer insets preserve native scroll semantics")
    func dynamicInsetsPreserveFollowingAndDetachedOffsets() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<100).map {
            "Paragraph \($0) with enough text to wrap across the reading column."
        }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(id: "reply", content: .markdown(source, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn),
            resourceRoot: nil
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()

        controller.debugSetContentInsets(NSEdgeInsets(top: 52, left: 0, bottom: 186, right: 0))
        let following = controller.debugLayoutState()
        #expect(following.isFollowingBottom)
        #expect(abs(following.offsetY - following.maximumOffsetY) <= 2, "\(following)")

        controller.scrollBack(viewports: 1)
        let detached = controller.debugLayoutState()
        #expect(!detached.isFollowingBottom)
        controller.debugSetContentInsets(NSEdgeInsets(top: 52, left: 0, bottom: 127, right: 0))
        let shrunk = controller.debugLayoutState()
        #expect(!shrunk.isFollowingBottom)
        #expect(abs(shrunk.offsetY - detached.offsetY) <= 1, "\(shrunk)")
    }

    @Test("automatic inset motion preserves a held completion footer's bottom target")
    func automaticInsetMotionKeepsCompletionFooterVisible() throws {
        final class FooterCard: TranscriptCard {
            weak var hostedView: NSView?

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { 28 }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let updater = HeldTranscriptBatchUpdater()
        var nextCompletion = 0
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        func drainUpdates() {
            while controller.debugTransactionIsBusy {
                guard nextCompletion < updater.callCount else {
                    Issue.record("busy renderer has no held native completion")
                    return
                }
                updater.finish(true, at: nextCompletion)
                nextCompletion += 1
            }
        }
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<100).map {
            "Paragraph \($0) with enough text to wrap across the reading column."
        }.joined(separator: "\n\n")
        let reply = TranscriptItem(
            id: "reply",
            content: .markdown(source, syntax: .full, container: nil)
        )
        controller.apply(
            plan: [reply],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        drainUpdates()

        controller.debugSetContentInsets(
            NSEdgeInsets(top: 52, left: 0, bottom: 230, right: 0)
        )
        drainUpdates()
        controller.scrollToBottom()

        let footer = TranscriptItem(
            id: "footer",
            content: .card(.turnTrailer(.persisted(
                runID: nil, body: source, createdAt: "now", outcome: .complete
            )))
        )
        controller.apply(
            plan: [reply, footer],
            cards: ["footer": FooterCard()],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        #expect(controller.debugTransactionIsBusy)

        // This is the completion ordering from the recording: the native bottom
        // accessory shrinks while footer insertion is still held. AppKit reports
        // the resulting clip adjustment through live-scroll notifications even
        // though the reader did not touch the transcript.
        controller.debugSetContentInsets(
            NSEdgeInsets(top: 52, left: 0, bottom: 186, right: 0)
        )
        controller.debugSimulateAutomaticLiveScroll(
            toOffsetY: controller.debugLayoutState().offsetY - 44
        )

        drainUpdates()

        let state = controller.debugLayoutState()
        let footerID = try #require(controller.debugBlockIDs().last)
        let footerFrame = try #require(controller.debugFrame(for: footerID))
        let unobscuredBottom = state.offsetY + controller.view.bounds.height - 186
        #expect(state.isFollowingBottom)
        #expect(abs(state.offsetY - state.maximumOffsetY) <= 2, "\(state)")
        #expect(footerFrame.maxY <= unobscuredBottom + 1, "\(footerFrame) \(state)")
    }

    @Test("deterministic scroll probe requests older history at the top")
    func scrollProbeRequestsOlderHistory() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<100).map {
            "Paragraph \($0) with enough text to wrap across the reading column."
        }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(id: "reply", content: .markdown(source, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()
        var requests = 0
        controller.onNearTop = { requests += 1 }

        controller.scrollBack(viewports: 100)
        controller.scrollBack(viewports: 100)

        #expect(requests == 1)
    }

    @Test("an unbracketed native wheel scroll detaches immediately")
    func unbracketedLiveScrollDetaches() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<100).map {
            "Paragraph \($0) with enough text to wrap across the reading column."
        }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(id: "reply", content: .markdown(source, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn),
            resourceRoot: nil
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()
        let bottom = controller.debugLayoutState()

        // AppKit documents that legacy wheel events may emit didLiveScroll
        // without a willStart/didEnd pair. `debugScroll` models that path when
        // it is called without `debugBeginLiveScroll`.
        controller.debugScroll(toOffsetY: bottom.maximumOffsetY - 240)
        let detached = controller.debugLayoutState()
        #expect(!detached.userIsLiveScrolling)
        #expect(!detached.isFollowingBottom)
    }

    @Test("streaming does not fight an active native scroll gesture")
    func streamingDefersScrollCorrectionsDuringLiveScroll() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<120).map {
            "Paragraph \($0) with enough text to wrap across the reading column."
        }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown(source, syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()
        try? await Task.sleep(for: .milliseconds(50))

        let bottom = controller.debugLayoutState()
        controller.debugBeginLiveScroll()
        controller.debugScroll(toOffsetY: bottom.maximumOffsetY - 600)
        controller.debugEndLiveScroll()
        let detached = controller.debugLayoutState()
        #expect(!detached.isFollowingBottom)

        let beforeDetachedAppend = controller.debugSynchronousMaterializationCount
        #expect(controller.appendProjectedText(
            " first streaming extension" + String(repeating: " word", count: 80),
            to: "live"
        ))
        #expect(
            controller.debugSynchronousMaterializationCount - beforeDetachedAppend == 1
        )
        controller.debugBeginLiveScroll()
        controller.debugScroll(toOffsetY: detached.offsetY - 180)
        let gestureOffset = controller.debugLayoutState().offsetY
        let beforeDeferredAppend = controller.debugSynchronousMaterializationCount
        #expect(controller.appendProjectedText(
            " second streaming extension" + String(repeating: " word", count: 80),
            to: "live"
        ))
        #expect(controller.debugSynchronousMaterializationCount == beforeDeferredAppend)
        try? await Task.sleep(for: .milliseconds(50))

        let duringGesture = controller.debugLayoutState()
        #expect(duringGesture.userIsLiveScrolling)
        #expect(abs(duringGesture.offsetY - gestureOffset) <= 1, "\(duringGesture)")
        controller.debugEndLiveScroll()

        let beforeReattach = controller.debugLayoutState()
        #expect(!beforeReattach.isFollowingBottom)
        controller.debugBeginLiveScroll()
        controller.debugScroll(toOffsetY: beforeReattach.maximumOffsetY)
        #expect(controller.appendProjectedText(
            " final streaming extension" + String(repeating: " word", count: 80),
            to: "live"
        ))
        controller.debugEndLiveScroll()
        try? await Task.sleep(for: .milliseconds(50))

        let reattached = controller.debugLayoutState()
        #expect(reattached.isFollowingBottom)
        #expect(abs(reattached.offsetY - reattached.maximumOffsetY) <= 2, "\(reattached)")
    }

    @Test("a following stream commit corrects the bottom synchronously")
    func followingStreamCommitIsAtomic() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<80).map {
            "\($0 + 1). Item \($0) with enough text to occupy one transcript line."
        }.joined(separator: "\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown(source, syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.scrollToBottom()
        let before = controller.debugLayoutState()
        let materializationsBeforeAppend = controller.debugSynchronousMaterializationCount

        #expect(controller.appendProjectedText(
            "\n81. A newly streamed list item that grows the document by another line.",
            to: "live"
        ))

        let after = controller.debugLayoutState()
        #expect(after.maximumOffsetY > before.maximumOffsetY)
        #expect(after.isFollowingBottom)
        #expect(abs(after.offsetY - after.maximumOffsetY) <= 2, "\(after)")
        #expect(
            controller.debugSynchronousMaterializationCount - materializationsBeforeAppend == 1
        )
        #expect(controller.debugDirectCollectionUpdateCount == 0)
        #expect(controller.debugBatchUpdateCount == 1)
        #expect(controller.debugNonanimatedStreamingUpdateCount == 1)
    }



    @Test("resize and inset inputs coalesce behind one held structural update")
    func busyLayoutInputsCoalesce() {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let drainsBefore = controller.debugPendingWorkDrainCount
        let materializationsBefore = controller.debugSynchronousMaterializationCount

        #expect(controller.appendProjectedText("\n\nSecond paragraph", to: "live"))
        controller.debugSetContentInsets(
            NSEdgeInsets(top: 30, left: 0, bottom: 50, right: 0)
        )
        controller.debugSetContentInsets(
            NSEdgeInsets(top: 42, left: 0, bottom: 70, right: 0)
        )
        controller.debugSetViewportSize(NSSize(width: 720, height: 420))

        #expect(controller.debugTransactionIsBusy)
        #expect(controller.debugPendingWorkDrainCount == drainsBefore)
        #expect(controller.debugSynchronousMaterializationCount == materializationsBefore)

        updater.finish(true)

        #expect(!controller.debugTransactionIsBusy)
        #expect(controller.debugPendingWorkDrainCount == drainsBefore + 1)
        #expect(controller.debugSynchronousMaterializationCount == materializationsBefore + 1)
        let state = controller.debugLayoutState()
        #expect(state.readingWidth == state.lastMeasuredWidth)
        #expect(abs(state.offsetY - state.maximumOffsetY) <= 2, "\(state)")
    }

    @Test("card and image geometry reentry coalesces behind a held structural update")
    func busyGeometryCoalesces() throws {
        final class Card: TranscriptCard {
            weak var hostedView: NSView?
            var height: CGFloat = 40

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { height }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let card = Card()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                ),
                TranscriptItem(
                    id: "footer",
                    content: .card(.turnTrailer(.persisted(
                        runID: nil, body: "Body", createdAt: "now", outcome: .complete
                    )))
                ),
            ],
            cards: ["footer": card],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let liveID = try #require(controller.debugBlockIDs().first)
        let drainsBefore = controller.debugPendingWorkDrainCount

        #expect(controller.appendProjectedText("\n\nSecond paragraph", to: "live"))
        card.height = 140
        controller.cardPayloadDidChange(card)
        controller.debugNotifyImageGeometryDidChange(liveID)

        #expect(controller.debugTransactionIsBusy)
        #expect(controller.debugBlockHeights().last == 40)
        #expect(controller.debugPendingImageGeometryCount == 1)

        updater.finish(true)

        #expect(!controller.debugTransactionIsBusy)
        #expect(controller.debugPendingWorkDrainCount == drainsBefore + 1)
        #expect(controller.debugBlockHeights().last == 140)
        #expect(controller.debugPendingImageGeometryCount == 0)
    }

    @Test("card expansion and collapse preserve a following bottom viewport")
    func cardGeometryPreservesFollowingBottom() {
        final class Card: TranscriptCard {
            weak var hostedView: NSView?
            var height: CGFloat = 40

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { height }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 160)
        controller.view.layoutSubtreeIfNeeded()
        let card = Card()
        let source = (0..<20).map { "Paragraph \($0)" }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "body",
                    content: .markdown(source, syntax: .full, container: nil)
                ),
                TranscriptItem(
                    id: "footer",
                    content: .card(.turnTrailer(.persisted(
                        runID: nil, body: "Body", createdAt: "now", outcome: .complete
                    )))
                ),
            ],
            cards: ["footer": card],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.scrollToBottom()

        card.height = 140
        controller.cardPayloadDidChange(card)
        let expanded = controller.debugLayoutState()
        #expect(expanded.isFollowingBottom)
        #expect(abs(expanded.offsetY - expanded.maximumOffsetY) <= 2, "\(expanded)")

        card.height = 40
        controller.cardPayloadDidChange(card)
        let collapsed = controller.debugLayoutState()
        #expect(collapsed.isFollowingBottom)
        #expect(abs(collapsed.offsetY - collapsed.maximumOffsetY) <= 2, "\(collapsed)")
    }

    @Test("turn trailer phase handoff keeps bottom geometry unchanged")
    func turnTrailerPhaseHandoffKeepsBottomGeometry() throws {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 180)
        controller.view.layoutSubtreeIfNeeded()

        let theme = MarkdownTheme.ottam(.builtIn)
        let store = ChatTranscriptCardStore(factory: LiveChatTranscriptCardFactory(
            theme: theme,
            environment: HostedCardEnvironment(typography: .builtIn),
            actions: ChatTranscriptActions()
        ))
        let runID = ConversationRunID("run-1")
        let trailerID = "run/run-1/trailer"
        let body = TranscriptItem(
            id: "run-1#0",
            content: .markdown(
                (0..<30).map { "Paragraph \($0) keeps the transcript taller than its viewport." }
                    .joined(separator: "\n\n"),
                syntax: .full,
                container: nil
            )
        )

        func apply(_ presentation: ChatTurnTrailerPresentation) {
            let trailer = TranscriptItem(
                id: trailerID,
                content: .card(.turnTrailer(presentation))
            )
            let items = [body, trailer]
            let cards = store.reconcile(items).cards
            controller.apply(
                plan: items,
                cards: cards,
                conversationID: ConversationID("conversation"),
                theme: theme
            )
        }

        apply(.running(
            runID: runID,
            status: .running,
            startedAt: "2026-08-31T18:00:00Z",
            activityCount: 2
        ))
        controller.scrollToBottom()
        let running = controller.debugLayoutState()
        let blockIDs = controller.debugBlockIDs()

        apply(.finished(runID: runID, body: "stand-in", outcome: .complete))
        let finished = controller.debugLayoutState()

        apply(.persisted(
            runID: runID,
            body: "canonical",
            createdAt: "2026-08-21T10:00:00Z",
            outcome: .endedEarly
        ))
        let persisted = controller.debugLayoutState()

        #expect(controller.debugBlockIDs() == blockIDs)
        #expect(abs(finished.maximumOffsetY - running.maximumOffsetY) <= 0.5)
        #expect(abs(persisted.maximumOffsetY - running.maximumOffsetY) <= 0.5)
        #expect(abs(finished.offsetY - running.offsetY) <= 0.5)
        #expect(abs(persisted.offsetY - running.offsetY) <= 0.5)
        #expect(persisted.isFollowingBottom)
    }

    @Test("card geometry changes preserve a detached reader anchor")
    func cardGeometryPreservesDetachedAnchor() throws {
        final class Card: TranscriptCard {
            weak var hostedView: NSView?
            var height: CGFloat = 40

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { height }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 240)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 240),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.contentView = controller.view
        controller.view.layoutSubtreeIfNeeded()
        let card = Card()
        let source = (0..<80).map {
            "Paragraph \($0) with enough text to keep the viewport detached."
        }.joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "activity",
                    content: .card(.turnTrailer(.persisted(
                        runID: nil, body: "Body", createdAt: "now", outcome: .complete
                    )))
                ),
                TranscriptItem(
                    id: "body",
                    content: .markdown(source, syntax: .full, container: nil)
                ),
            ],
            cards: ["activity": card],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.scrollToBottom()
        let following = controller.debugLayoutState()
        controller.debugBeginLiveScroll()
        controller.debugScroll(toOffsetY: following.maximumOffsetY * 0.55)
        controller.debugEndLiveScroll()
        let before = try #require(controller.debugViewportAnchor())
        #expect(!controller.debugLayoutState().isFollowingBottom)

        card.height = 140
        controller.cardPayloadDidChange(card)

        let after = try #require(controller.debugViewportAnchor())
        #expect(after.id == before.id)
        #expect(
            abs(after.relativeY - before.relativeY) <= 1,
            "before=\(before.relativeY) after=\(after.relativeY)"
        )
        #expect(!controller.debugLayoutState().isFollowingBottom)
    }

    @Test("live scrolling that begins after submission downgrades held viewport work")
    func liveScrollStartInvalidatesHeldViewportCommit() {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let materializationsBefore = controller.debugSynchronousMaterializationCount

        #expect(controller.appendProjectedText("\n\nSecond paragraph", to: "live"))
        #expect(controller.debugTransactionIsBusy)

        controller.debugBeginLiveScroll()
        updater.finish(true)

        #expect(!controller.debugTransactionIsBusy)
        #expect(controller.debugSynchronousMaterializationCount == materializationsBefore)

        controller.debugEndLiveScroll()

        #expect(controller.debugSynchronousMaterializationCount == materializationsBefore + 1)
    }

    @Test("selection autoscroll waits for held completion and uses completed selection")
    func selectionAutoscrollWaitsForHeldCompletion() throws {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let blockID = try #require(controller.debugCompletedBlockIDs().first)
        controller.debugBeginSelectionDrag(
            in: blockID,
            offset: 0,
            initialRange: NSRange(location: 0, length: "First paragraph".utf16.count)
        )
        let materializationsBefore = controller.debugSynchronousMaterializationCount
        let autoscrollsBefore = controller.debugNativeSelectionAutoscrollCount
        #expect(controller.appendProjectedText("\n\nSecond paragraph", to: "live"))
        let event = try #require(NSEvent.mouseEvent(
            with: .leftMouseDragged,
            location: NSPoint(x: 100, y: -20),
            modifierFlags: [],
            timestamp: 0,
            windowNumber: 0,
            context: nil,
            eventNumber: 1,
            clickCount: 1,
            pressure: 1
        ))

        controller.debugUpdateSelectionDrag(with: event)

        #expect(controller.debugSynchronousMaterializationCount == materializationsBefore)
        #expect(controller.debugNativeSelectionAutoscrollCount == autoscrollsBefore)
        #expect(controller.debugSelectedCopyText() == "First paragraph")

        updater.finish(true)

        #expect(controller.debugSynchronousMaterializationCount == materializationsBefore + 1)
        controller.debugEndSelectionDrag(with: event)
    }

    @Test("upward selection autoscroll detaches before the next live publication")
    func upwardSelectionAutoscrollDetachesFollowing() throws {
        let controller = MacAgentTranscriptViewController(
            selectionAutoscroller: { collectionView, _ in
                guard let clipView = collectionView.enclosingScrollView?.contentView else {
                    return false
                }
                let proposed = NSRect(
                    x: clipView.bounds.minX,
                    y: clipView.bounds.minY - 80,
                    width: clipView.bounds.width,
                    height: clipView.bounds.height
                )
                clipView.scroll(to: clipView.constrainBoundsRect(proposed).origin)
                return true
            }
        )
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 420)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 420),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.contentView = controller.view
        let longText = (0..<120).map { "Paragraph \($0) fills the native transcript." }
            .joined(separator: "\n\n")
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown(longText, syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        let initial = controller.debugLayoutState()
        #expect(initial.isFollowingBottom)
        #expect(abs(initial.offsetY - initial.maximumOffsetY) <= 2)
        let lastBlock = try #require(controller.debugCompletedBlockIDs().last)
        controller.debugBeginSelectionDrag(in: lastBlock, offset: 0)
        let event = try #require(NSEvent.mouseEvent(
            with: .leftMouseDragged,
            location: NSPoint(x: 100, y: -40),
            modifierFlags: [],
            timestamp: 0,
            windowNumber: window.windowNumber,
            context: nil,
            eventNumber: 1,
            clickCount: 1,
            pressure: 1
        ))

        controller.debugUpdateSelectionDrag(with: event)

        let detached = controller.debugLayoutState()
        #expect(detached.offsetY < initial.offsetY)
        #expect(!detached.isFollowingBottom)
        #expect(controller.appendProjectedText("\n\nA later paragraph", to: "live"))
        let afterAppend = controller.debugLayoutState()
        #expect(!afterAppend.isFollowingBottom)
        #expect(afterAppend.offsetY < afterAppend.maximumOffsetY)
        controller.debugEndSelectionDrag(with: event)
    }

    @Test("prepend restores its completed-generation anchor after native completion")
    func prependRestoresCompletedAnchor() throws {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 420)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 420),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.contentView = controller.view
        controller.view.layoutSubtreeIfNeeded()
        let conversationID = ConversationID("conversation")
        let theme = MarkdownTheme.ottam(.builtIn)
        let original = (0..<30).map { index in
            TranscriptItem(
                id: "item-\(index)",
                content: .markdown(
                    "Paragraph \(index) has enough text to occupy a stable transcript row.",
                    syntax: .full,
                    container: nil
                )
            )
        }
        controller.apply(
            plan: original,
            cards: [:],
            conversationID: conversationID,
            theme: theme
        )
        let state = controller.debugLayoutState()
        controller.debugBeginLiveScroll()
        controller.debugScroll(toOffsetY: state.maximumOffsetY * 0.55)
        controller.debugEndLiveScroll()
        let detachedState = controller.debugLayoutState()
        #expect(!detachedState.isFollowingBottom)
        let anchorBefore = try #require(controller.debugViewportAnchor())
        var canonicalResults: [Bool] = []
        let prepended = (0..<200).map { index in
            TranscriptItem(
                id: "prepended-\(index)",
                content: .markdown(
                    "Earlier paragraph \(index) adds history above the completed viewport.",
                    syntax: .full,
                    container: nil
                )
            )
        } + original

        controller.applyPreparingLargeSettledTranscript(
            plan: prepended,
            cards: [:],
            conversationID: conversationID,
            theme: theme,
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { canonicalResults.append($0) }
        )

        #expect(controller.debugTransactionIsBusy)
        #expect(canonicalResults.isEmpty)

        updater.finish(true)

        let frameAfter = try #require(controller.debugFrame(for: anchorBefore.id))
        let stateAfter = controller.debugLayoutState()
        #expect(canonicalResults == [true])
        #expect(abs((frameAfter.minY - stateAfter.offsetY) - anchorBefore.relativeY) <= 1)
    }

    @Test("the latest canonical presentation supersedes queued canonical work")
    func latestCanonicalSupersedesQueuedPresentation() {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let conversationID = ConversationID("conversation")
        let theme = MarkdownTheme.ottam(.builtIn)
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: conversationID,
            theme: theme
        )
        #expect(controller.appendProjectedText("\n\nSecond paragraph", to: "live"))
        var firstResults: [Bool] = []
        var latestResults: [Bool] = []

        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("Superseded canonical", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: conversationID,
            theme: theme,
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { firstResults.append($0) }
        )
        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("Latest canonical", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: conversationID,
            theme: theme,
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { latestResults.append($0) }
        )

        #expect(firstResults == [false])
        #expect(latestResults.isEmpty)
        #expect(updater.callCount == 1)

        updater.finish(true, at: 0)

        #expect(updater.callCount == 2)
        #expect(latestResults.isEmpty)

        updater.finish(true, at: 1)

        #expect(latestResults == [true])
        #expect(controller.debugCompletedMarkdownText(itemID: "live") == "Latest canonical")
    }



    @Test("same-size identity replacement uses one native item-reload transaction")
    func sameSizeIdentityReplacementReloadsItems() {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let conversationID = ConversationID("conversation")
        let theme = MarkdownTheme.ottam(.builtIn)
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live-run#0",
                    content: .markdown("One paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: conversationID,
            theme: theme
        )
        let completedIDs = controller.debugCompletedBlockIDs()

        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(
                    id: "persisted-message",
                    content: .markdown("One paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: conversationID,
            theme: theme,
            permitsBackgroundPreparation: false
        )

        #expect(updater.callCount == 1)
        #expect(controller.debugTransactionIsBusy)
        #expect(controller.debugItemReloadCount == 1)
        #expect(controller.debugCompletedBlockIDs() == completedIDs)

        updater.finish(true)

        #expect(!controller.debugTransactionIsBusy)
        #expect(controller.debugCompletedBlockIDs() != completedIDs)
    }



    @Test("canonical presentation queues behind a held structural update")
    func canonicalPresentationQueuesBehindNativeCompletion() {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        #expect(controller.appendProjectedText("\n\nSecond paragraph", to: "live"))
        var canonicalResults: [Bool] = []

        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("Canonical replacement", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn),
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { canonicalResults.append($0) }
        )
        controller.debugSetContentInsets(
            NSEdgeInsets(top: 18, left: 0, bottom: 24, right: 0)
        )

        #expect(updater.callCount == 1)
        #expect(canonicalResults.isEmpty)
        #expect(controller.debugCompletedMarkdownText(itemID: "live") == "First paragraph")

        updater.finish(true, at: 0)

        #expect(updater.callCount == 2)
        #expect(controller.debugTransactionIsBusy)
        #expect(canonicalResults.isEmpty)

        updater.finish(true, at: 1)

        #expect(!controller.debugTransactionIsBusy)
        #expect(canonicalResults == [true])
        #expect(controller.debugCompletedMarkdownText(itemID: "live") == "Canonical replacement")
        #expect(updater.callCount == 2)
    }

    @Test("false and late native completions fail once without promotion")
    func failedNativeCompletionDoesNotPromote() throws {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let completedIDs = controller.debugCompletedBlockIDs()
        var completions: [Bool] = []

        controller.applyPreparingLargeSettledTranscript(
            plan: [.init(id: "live", content: .markdown("First paragraph\n\nSecond paragraph", syntax: .full, container: nil))],
            cards: [:], conversationID: ConversationID("conversation"), theme: .ottam(.builtIn),
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { completions.append($0) }
        )

        updater.finish(false)
        updater.finish(true)

        #expect(!controller.debugTransactionIsBusy)
        #expect(controller.debugCompletedBlockIDs() == completedIDs)
        #expect(completions == [false])

        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown(
                        "First paragraph\n\nSecond paragraph",
                        syntax: .full,
                        container: nil
                    )
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        #expect(controller.debugCompletedBlockIDs().count == 2)
    }

    @Test("teardown fails a held canonical publication and ignores its late native completion")
    func teardownSettlesHeldCanonicalOnce() {
        let updater = HeldTranscriptBatchUpdater()
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("First paragraph", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        var completions: [Bool] = []
        var canonicalResults: [Bool] = []
        controller.applyPreparingLargeSettledTranscript(
            plan: [.init(id: "live", content: .markdown("First paragraph\n\nSecond paragraph", syntax: .full, container: nil))],
            cards: [:], conversationID: ConversationID("conversation"), theme: .ottam(.builtIn),
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { completions.append($0) }
        )
        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("Canonical replacement", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn),
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { canonicalResults.append($0) }
        )

        controller.tearDown()
        updater.finish(true)

        #expect(completions == [false])
        #expect(canonicalResults == [false])
    }



    @Test("normalized append-only snapshots reuse incremental native projection")
    func canonicalAppendUsesIncrementalProjection() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        func publish(_ body: String) {
            controller.apply(plan: [.init(id: "reply", content: .markdown(body, syntax: .full, container: nil))],
                             cards: [:], conversationID: ConversationID("conversation"), theme: .ottam(.builtIn))
        }
        publish("First paragraph.\n\nSecond")
        let fullApplications = controller.debugApplyCount
        publish("First paragraph.\n\nSecond paragraph.")
        #expect(controller.debugApplyCount == fullApplications)
        #expect(controller.debugCompletedMarkdownText(itemID: "reply") == "First paragraph.\n\nSecond paragraph.")
        publish("Replacement")
        #expect(controller.debugApplyCount == fullApplications + 1)
        #expect(controller.debugCompletedMarkdownText(itemID: "reply") == "Replacement")
    }

    @Test("an identical presentation update performs no renderer work")
    func identicalApplyIsNoOp() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let plan = [
            TranscriptItem(id: "reply", content: .markdown("Hello", syntax: .full, container: nil))
        ]
        let theme = MarkdownTheme.ottam(.builtIn)
        controller.apply(
            plan: plan,
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: theme
        )
        let count = controller.debugApplyCount

        controller.apply(
            plan: plan,
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: theme
        )
        #expect(controller.debugApplyCount == count)
    }

    @Test("a theme change reconfigures materialized text without replacing the plan")
    func themeChangeRefreshesVisibleText() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let plan = [
            TranscriptItem(id: "reply", content: .markdown("Hello", syntax: .full, container: nil))
        ]
        controller.apply(
            plan: plan,
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()

        let largerTypography = AppTypography.Resolution(
            uiSizeDelta: 2,
            codeSizeDelta: 2
        )
        let largerTheme = MarkdownTheme.ottam(largerTypography)
        controller.apply(
            plan: plan,
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: largerTheme
        )
        controller.view.layoutSubtreeIfNeeded()

        #expect(controller.debugVisibleThemeSignatures() == [largerTheme.signature])
    }

    @Test("an effective-appearance change rebuilds materialized text artifacts")
    func appearanceChangeRefreshesVisibleText() {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "reply",
                    content: .markdown("Hello", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        let before = controller.debugVisibleArtifactIdentities()

        controller.debugNotifyEffectiveAppearanceChanged()

        let after = controller.debugVisibleArtifactIdentities()
        #expect(!before.isEmpty)
        #expect(!after.isEmpty)
        #expect(before.isDisjoint(with: after))
    }

    @Test("a changed card spec invalidates native height under stable block identity")
    func changedCardSpecInvalidatesHeight() throws {
        final class Card: TranscriptCard {
            weak var hostedView: NSView?
            var height: CGFloat = 40

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { height }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 100)
        controller.view.layoutSubtreeIfNeeded()
        let card = Card()
        let first = TranscriptItem(
            id: "footer",
            content: .card(.turnTrailer(.persisted(
                runID: nil, body: "Body", createdAt: "now", outcome: .complete
            )))
        )
        controller.apply(
            plan: [first],
            cards: ["footer": card],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        let blockID = try #require(controller.debugBlockIDs().first)
        #expect(controller.debugBlockHeights() == [40])

        card.height = 140
        let second = TranscriptItem(
            id: "footer",
            content: .card(.turnTrailer(.persisted(
                runID: nil, body: "Body", createdAt: "now", outcome: .endedEarly
            )))
        )
        controller.apply(
            plan: [second],
            cards: ["footer": card],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )

        #expect(controller.debugBlockIDs() == [blockID])
        #expect(controller.debugBlockHeights() == [140])
    }

    @Test("an inserted Thought card preserves the trailing turn-trailer bottom target")
    func thoughtInsertionPreservesBottomTarget() {
        final class Card: TranscriptCard {
            weak var hostedView: NSView?

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { 28 }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let runID = ConversationRunID("run-live")
        let initialStatus = TranscriptItem(
            id: "run/run-live/trailer",
            content: .card(.turnTrailer(.running(
                runID: runID,
                status: .running,
                startedAt: nil,
                activityCount: 0
            )))
        )
        controller.apply(
            plan: [initialStatus],
            cards: [initialStatus.id: Card()],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.scrollToBottom()

        let thought = TranscriptItem(
            id: "run-live#0",
            content: .card(.activityGroup([
                ChatLiveActivity(
                    id: "run-live#0/0",
                    label: "Thought",
                    detail: nil,
                    kind: "think",
                    toolCallID: nil
                ),
            ], isActive: true))
        )
        let updatedStatus = TranscriptItem(
            id: initialStatus.id,
            content: .card(.turnTrailer(.running(
                runID: runID,
                status: .running,
                startedAt: nil,
                activityCount: 1
            )))
        )
        controller.apply(
            plan: [thought, updatedStatus],
            cards: [thought.id: Card(), updatedStatus.id: Card()],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )

        let state = controller.debugLayoutState()
        #expect(state.isFollowingBottom)
        #expect(abs(state.offsetY - state.maximumOffsetY) <= 2, "\(state)")
    }

    @Test("streamed block insertion keeps the trailing turn trailer stationary during native completion")
    func structuralStreamingKeepsRunStatusStationaryWhileHeld() throws {
        final class Card: TranscriptCard {
            weak var hostedView: NSView?

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { 28 }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }

        let updater = HeldTranscriptBatchUpdater()
        var nextCompletion = 0
        let controller = MacAgentTranscriptViewController(batchUpdater: updater.call)
        func drainUpdates() {
            while controller.debugTransactionIsBusy {
                updater.finish(true, at: nextCompletion)
                nextCompletion += 1
            }
        }
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<60).map {
            "Paragraph \($0) has enough text to keep the transcript taller than its viewport."
        }.joined(separator: "\n\n")
        let status = TranscriptItem(
            id: "run/run-live/trailer",
            content: .card(.turnTrailer(.running(
                runID: ConversationRunID("run-live"),
                status: .running,
                startedAt: nil,
                activityCount: 3
            )))
        )
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown(source, syntax: .full, container: nil)
                ),
                status,
            ],
            cards: [status.id: Card()],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        drainUpdates()
        controller.scrollToBottom()

        let statusID = try #require(controller.debugBlockIDs().last)
        let frameBefore = try #require(controller.debugFrame(for: statusID))
        let stateBefore = controller.debugLayoutState()
        let relativeYBefore = frameBefore.minY - stateBefore.offsetY

        #expect(controller.appendProjectedText("\n\n**Swift:** strongly typed protocol requests", to: "live"))
        #expect(controller.debugTransactionIsBusy)

        let frameWhileHeld = try #require(controller.debugFrame(for: statusID))
        let stateWhileHeld = controller.debugLayoutState()
        #expect(
            abs((frameWhileHeld.minY - stateWhileHeld.offsetY) - relativeYBefore) <= 1,
            "before=\(stateBefore) held=\(stateWhileHeld)"
        )

        drainUpdates()

        let frameAfter = try #require(controller.debugFrame(for: statusID))
        let stateAfter = controller.debugLayoutState()
        #expect(abs((frameAfter.minY - stateAfter.offsetY) - relativeYBefore) <= 1)
        #expect(abs(stateAfter.offsetY - stateAfter.maximumOffsetY) <= 2, "\(stateAfter)")
    }

    @Test("a large settled conversation publishes exact background measurements at bottom")
    func largeSettledConversationPreparesOffMain() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<1_200).map {
            "Paragraph \($0) has enough ordinary text to create an exact native layout row."
        }.joined(separator: "\n\n")

        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(id: "large", content: .markdown(source, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("large-conversation"),
            theme: .ottam(.builtIn),
            permitsBackgroundPreparation: true
        )

        for _ in 0..<100 where controller.debugLayoutState().blockCount == 0 {
            try? await Task.sleep(for: .milliseconds(10))
        }
        controller.view.layoutSubtreeIfNeeded()
        let state = controller.debugLayoutState()
        #expect(state.blockCount == 1_200)
        #expect(state.isFollowingBottom)
        #expect(abs(state.offsetY - state.maximumOffsetY) <= 2, "\(state)")
    }

    @Test("large background preparation waits for a resolved viewport width")
    func largePreparationWaitsForWidth() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        let source = (0..<1_200).map {
            "Paragraph \($0) has enough ordinary text to create an exact native layout row."
        }.joined(separator: "\n\n")

        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(id: "large", content: .markdown(source, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("large-conversation"),
            theme: .ottam(.builtIn),
            permitsBackgroundPreparation: true
        )
        #expect(controller.debugLayoutState().blockCount == 0)

        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        for _ in 0..<100 where controller.debugLayoutState().blockCount == 0 {
            try? await Task.sleep(for: .milliseconds(10))
        }

        let state = controller.debugLayoutState()
        #expect(state.blockCount == 1_200)
        #expect(state.lastMeasuredWidth == state.readingWidth)
        #expect(abs(state.offsetY - state.maximumOffsetY) <= 2, "\(state)")
    }

    @Test("resizing during preparation cannot install stale-width heights")
    func resizeDuringLargePreparation() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 520, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let source = (0..<1_800).map {
            "Paragraph \($0) has enough ordinary text to keep background measurement occupied."
        }.joined(separator: "\n\n")

        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(id: "large", content: .markdown(source, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("large-conversation"),
            theme: .ottam(.builtIn),
            permitsBackgroundPreparation: true
        )
        controller.view.frame = NSRect(x: 0, y: 0, width: 900, height: 500)
        controller.view.layoutSubtreeIfNeeded()

        for _ in 0..<150 where controller.debugLayoutState().blockCount == 0 {
            try? await Task.sleep(for: .milliseconds(10))
        }
        let state = controller.debugLayoutState()
        #expect(state.blockCount == 1_800)
        #expect(state.readingWidth == 760)
        #expect(state.lastMeasuredWidth == state.readingWidth)
        #expect(abs(state.offsetY - state.maximumOffsetY) <= 2, "\(state)")
    }

    @Test("a stale background preparation cannot replace a newer conversation")
    func staleBackgroundPreparationIsDiscarded() async {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        let oldSource = (0..<1_500).map {
            "Old paragraph \($0) has enough content to keep the measurement worker occupied."
        }.joined(separator: "\n\n")
        controller.applyPreparingLargeSettledTranscript(
            plan: [
                TranscriptItem(id: "old", content: .markdown(oldSource, syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("old-conversation"),
            theme: .ottam(.builtIn),
            permitsBackgroundPreparation: true
        )

        controller.apply(
            plan: [
                TranscriptItem(id: "new", content: .markdown("New conversation", syntax: .full, container: nil))
            ],
            cards: [:],
            conversationID: ConversationID("new-conversation"),
            theme: .ottam(.builtIn)
        )
        try? await Task.sleep(for: .milliseconds(300))

        #expect(controller.debugBlockIDs().map(\.itemID) == ["new"])
    }

    @Test("a live append preserves selection in the stable text block")
    func liveAppendPreservesSelection() throws {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(
                    id: "live",
                    content: .markdown("Select this text", syntax: .full, container: nil)
                )
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        let blockID = try #require(controller.debugBlockIDs().last)
        #expect(controller.debugSelect(range: NSRange(location: 0, length: 6), in: blockID))

        #expect(controller.appendProjectedText(" and keep it", to: "live"))
        #expect(controller.debugSelectedRange(in: blockID) == NSRange(location: 0, length: 6))
    }

    @Test("the quote reply follows its selection through scrolling and pins at the edge")
    func quoteReplyFollowsSelectionAndPins() throws {
        let controller = MacAgentTranscriptViewController()
        // SwiftUI configures quote reply before the view has a size.
        controller.quoteReply = TranscriptQuoteReplyConfiguration(agentName: "Agent") { _, _ in }
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: (0..<40).map { index in
                TranscriptItem(
                    id: "item-\(index)",
                    content: .markdown("Paragraph \(index) of a transcript tall enough to scroll.", syntax: .full, container: nil)
                )
            },
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        controller.debugScroll(toOffsetY: 0)
        controller.view.layoutSubtreeIfNeeded()
        let blockID = try #require(controller.debugBlockIDs().first)
        #expect(controller.debugSelect(range: NSRange(location: 0, length: 9), in: blockID))

        let presenter = try #require(controller.debugQuoteReplyPresenter)
        #expect(presenter.isShown)
        #expect(presenter.pin == .none)
        // The first placement grows the capsule in; the next lands it.
        controller.debugScroll(toOffsetY: 0)
        controller.view.layoutSubtreeIfNeeded()
        let inlineFrame = presenter.surfaceFrame

        controller.debugScroll(toOffsetY: 400)
        controller.view.layoutSubtreeIfNeeded()
        #expect(presenter.isShown)
        #expect(presenter.pin == .top)
        #expect(presenter.surfaceFrame.minY < inlineFrame.minY)

        controller.debugScroll(toOffsetY: 0)
        controller.view.layoutSubtreeIfNeeded()
        #expect(presenter.pin == .none)
        #expect(presenter.surfaceFrame == inlineFrame)

        controller.apply(
            plan: [TranscriptItem(id: "other", content: .markdown("Other", syntax: .full, container: nil))],
            cards: [:],
            conversationID: ConversationID("other-conversation"),
            theme: .ottam(.builtIn)
        )
        #expect(!presenter.isShown)
    }

    @Test("a press on empty space selects from the nearest text")
    func backgroundPressSelectsNearestText() throws {
        let controller = MacAgentTranscriptViewController()
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 500),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        defer { window.close() }
        // In a window, so a press converts through window coordinates.
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        window.contentView?.addSubview(controller.view)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(id: "first", content: .markdown("alpha beta", syntax: .full, container: nil)),
                TranscriptItem(id: "second", content: .markdown("gamma delta", syntax: .full, container: nil)),
            ],
            cards: [:],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        let ids = controller.debugBlockIDs()
        let first = try #require(controller.debugFrame(for: ids[0]))
        let second = try #require(controller.debugFrame(for: ids[1]))
        #expect(second.minY > first.maxY)
        #expect(controller.debugVisibleArtifactIdentities().count == 2)

        // Just under the first paragraph, left of its first word.
        controller.debugPress(at: NSPoint(x: first.minX - 4, y: first.maxY + 1), clickCount: 2)
        #expect(controller.debugSelectedCopyText() == "alpha")
        // Right of the second paragraph's text, in the gutter.
        controller.debugPress(at: NSPoint(x: second.maxX + 40, y: second.midY), clickCount: 2)
        #expect(controller.debugSelectedCopyText() == "delta")
    }

    /// A paragraph with a card under it, as an agent message above its status
    /// row, in a window so presses convert through window coordinates.
    private func makeParagraphAboveCard() throws -> (
        controller: MacAgentTranscriptViewController,
        window: NSWindow,
        text: NSRect,
        card: NSRect
    ) {
        final class StatusCard: TranscriptCard {
            weak var hostedView: NSView?

            func measuredHeight(forWidth width: CGFloat) -> CGFloat { 28 }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }
        let controller = MacAgentTranscriptViewController()
        controller.quoteReply = TranscriptQuoteReplyConfiguration(agentName: "Agent") { _, _ in }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 500),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        window.contentView?.addSubview(controller.view)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(
            plan: [
                TranscriptItem(id: "reply", content: .markdown("alpha beta gamma", syntax: .full, container: nil)),
                TranscriptItem(id: "status", content: .card(.turnTrailer(.persisted(
                    runID: nil, body: "alpha beta gamma", createdAt: "now", outcome: .complete
                )))),
            ],
            cards: ["status": StatusCard()],
            conversationID: ConversationID("conversation"),
            theme: .ottam(.builtIn)
        )
        controller.view.layoutSubtreeIfNeeded()
        let ids = controller.debugBlockIDs()
        let text = try #require(controller.debugFrame(for: ids[0]))
        let card = try #require(controller.debugFrame(for: ids[1]))
        return (controller, window, text, card)
    }

    @Test("a click on empty space below a card clears the selection and its reply")
    func backgroundClickClearsSelection() throws {
        let (controller, window, text, card) = try makeParagraphAboveCard()
        defer { window.close() }
        controller.debugPress(at: NSPoint(x: text.minX + 4, y: text.midY), clickCount: 2)
        #expect(controller.debugSelectedCopyText() == "alpha")
        #expect(controller.debugQuoteReplyPresenter?.isShown == true)

        // Below the card, under the middle of the line: the press anchors in
        // the nearest text, and a release there must not extend the selection
        // to the card's boundary.
        controller.debugPress(at: NSPoint(x: text.minX + 40, y: card.maxY + 40), clickCount: 1)
        #expect(controller.debugSelectedCopyText() == nil)
        #expect(controller.debugQuoteReplyPresenter?.isShown == false)
    }

    @Test("a click that jitters under the drag threshold stays a click")
    func backgroundClickJitterStaysClick() throws {
        let (controller, window, text, card) = try makeParagraphAboveCard()
        defer { window.close() }
        controller.debugPress(at: NSPoint(x: text.minX + 4, y: text.midY), clickCount: 2)
        #expect(controller.debugQuoteReplyPresenter?.isShown == true)

        let press = NSPoint(x: text.minX + 40, y: card.maxY + 40)
        controller.debugPress(at: press, clickCount: 1, drags: [NSPoint(x: press.x + 1, y: press.y + 1)])
        #expect(controller.debugSelectedCopyText() == nil)
        #expect(controller.debugQuoteReplyPresenter?.isShown == false)
    }

    @Test("a drag still extends to where it is released")
    func dragExtendsToRelease() throws {
        let (controller, window, text, card) = try makeParagraphAboveCard()
        defer { window.close() }
        // Drags partway into the line, then releases below the card: the
        // release, not the last drag, sets the extent, which clamps at the
        // card to the paragraph's end. The press sits before the middle of the
        // first letter in any system font size, so it anchors at offset 0.
        controller.debugPress(
            at: NSPoint(x: text.minX + 1, y: text.midY),
            clickCount: 1,
            drags: [NSPoint(x: text.minX + 40, y: text.midY)],
            release: NSPoint(x: text.minX + 40, y: card.maxY + 40)
        )
        #expect(controller.debugSelectedCopyText() == "alpha beta gamma")
        #expect(controller.debugQuoteReplyPresenter?.isShown == true)
    }

    @Test("the quote reply takes clicks on its surface and passes the rest through")
    func quoteReplyPassesClicksOutsideItsSurface() {
        let presenter = QuoteReplyPresenter()
        // SwiftUI hit-tests only laid-out content, which needs a window.
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 800, height: 600),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        defer { window.close() }
        presenter.layer.frame = NSRect(x: 0, y: 0, width: 800, height: 600)
        window.contentView?.addSubview(presenter.layer)
        presenter.show(quote: "quoted")
        presenter.place(
            anchor: NSRect(x: 40, y: 100, width: 300, height: 18),
            band: NSRect(x: 0, y: 0, width: 800, height: 600)
        )
        window.contentView?.layoutSubtreeIfNeeded()
        let surface = presenter.surfaceFrame
        #expect(surface.width > 0 && surface.height > 0)
        // hitTest takes the superview's coordinates; the surface is in the layer's.
        func hit(_ x: CGFloat, _ y: CGFloat) -> NSView? {
            presenter.layer.hitTest(presenter.layer.convert(NSPoint(x: x, y: y), to: presenter.layer.superview))
        }
        #expect(hit(surface.midX, surface.midY) != nil)
        // Inside the hosting view's room to grow, outside the surface.
        #expect(hit(surface.minX + 10, surface.maxY + 60) == nil)

        presenter.hide()
        #expect(hit(surface.midX, surface.midY) == nil)
    }

    private func renderedSources(_ blocks: [TranscriptBlock]) -> [String] {
        blocks.compactMap { block in
            guard case .text(let text) = block.content else { return nil }
            return text.source
        }
    }
}
