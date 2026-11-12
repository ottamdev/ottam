import AppKit
import OttamBackendClient
import Testing
@testable import OttamApp

@Suite("Conversation switch cache")
@MainActor
struct ConversationSwitchCacheTests {
    private let a = ConversationID("a")
    private let b = ConversationID("b")
    private let c = ConversationID("c")

    private func page(_ items: [ConversationTranscriptItem], sequence: UInt64 = 1) -> ConversationListTranscriptResult {
        .init(coreInstanceID: .init("core"), sequence: sequence, items: items)
    }

    private func delta(_ conversation: ConversationID, sequence: UInt64, items: [ConversationTranscriptItem]) -> ConversationTranscriptDeltaParams {
        .init(workspaceID: .init("workspace"), conversationID: conversation, coreInstanceID: .init("core"),
              sequence: sequence, items: items, bodyAppends: [])
    }

    // MARK: Cache policy

    @Test("the least recently used conversation leaves first; the shown and running ones stay")
    func capacity() {
        let cache = ConversationTranscriptCache(policy: .init(capacity: 2))
        _ = cache.open(a)
        _ = cache.open(b)
        _ = cache.open(c)
        #expect(cache.evict(shown: c, running: []) == [a])
        #expect(cache.cachedIDs == [b, c])

        let pinned = ConversationTranscriptCache(policy: .init(capacity: 1))
        _ = pinned.open(a)
        _ = pinned.open(b)
        #expect(pinned.evict(shown: b, running: [a]).isEmpty, "a running conversation is never evicted")
        #expect(pinned.cachedIDs == [a, b])
    }

    @Test("a conversation idle past the interval is evicted, counted from its last idle mark")
    func idleClock() {
        var now = Date(timeIntervalSince1970: 0)
        let cache = ConversationTranscriptCache(policy: .init(idleInterval: 60), now: { now })
        _ = cache.open(a)
        _ = cache.open(b)
        cache.markIdle(a)
        now += 50
        // Its run ends out of view: the clock starts over.
        cache.markIdle(a)
        now += 50
        #expect(cache.evict(shown: b, running: []).isEmpty)
        now += 11
        #expect(cache.evict(shown: b, running: []) == [a])
        #expect(cache.evict(shown: nil, running: []).isEmpty, "b was never left, so it is not idle")
    }

    @Test("pressure and Core restart drop sessions but keep what the reader left; deletion forgets both")
    func removal() {
        let cache = ConversationTranscriptCache()
        _ = cache.open(a)
        _ = cache.open(b)
        let state = cache.uiState(for: a)
        state.readingPlace = .bottom
        cache.removeSessions(keeping: b)
        #expect(cache.cachedIDs == [b])
        #expect(cache.uiState(for: a) === state)
        cache.forget(a)
        #expect(cache.uiState(for: a) !== state)
        cache.forgetAll()
        #expect(cache.cachedIDs.isEmpty)
    }

    @Test("eviction prunes what readers left only when it holds nothing and is uncached")
    func uiStatePruning() {
        let cache = ConversationTranscriptCache(policy: .init(capacity: 1))
        _ = cache.open(a)
        let shownEmpty = cache.uiState(for: a)
        let leftOpen = cache.uiState(for: b)
        leftOpen.setExpanded(true, activityIDs: ["t1"])
        let leftEmpty = cache.uiState(for: c)
        leftEmpty.readingPlace = .bottom
        cache.evict(shown: a, running: [])
        #expect(cache.uiState(for: a) === shownEmpty, "a cached conversation's state stays")
        #expect(cache.uiState(for: b) === leftOpen)
        #expect(cache.uiState(for: c) !== leftEmpty)
    }

    @Test("a group stays open through any member, so paging back keeps it")
    func disclosures() {
        let state = ChatConversationUIState()
        state.setExpanded(true, activityIDs: ["t2", "t3"])
        #expect(state.isExpanded(["t1", "t2", "t3"]))
        state.setExpanded(false, activityIDs: ["t1", "t2", "t3"])
        #expect(!state.isExpanded(["t3"]))
    }

    @Test("a kept rendering returns once; an evicted conversation keeps only its place")
    func keeper() {
        let cache = ConversationTranscriptCache()
        _ = cache.open(a)
        let session = TranscriptViewSession()
        cache.keep(session, place: .block(itemID: "x", ordinal: 1, relativeY: 4), for: a)
        #expect(cache.hasViewSession(for: a))
        #expect(cache.takeViewSession(for: a) === session)
        #expect(!cache.hasViewSession(for: a))
        cache.keep(TranscriptViewSession(), place: nil, for: a)
        #expect(cache.readingPlace(for: a) == .block(itemID: "x", ordinal: 1, relativeY: 4), "nil keeps the last place")
        cache.keep(TranscriptViewSession(), place: .bottom, for: c)
        #expect(!cache.hasViewSession(for: c))
        #expect(cache.readingPlace(for: c) == .bottom)
    }

    @Test("an artifact cache trims to its most recently used entries")
    func artifactTrim() {
        let source = (0..<6).map { "Paragraph \($0)." }.joined(separator: "\n\n")
        let blocks = TranscriptBlockProjector().project([
            TranscriptItem(id: "body", content: .markdown(source, syntax: .full, container: nil))
        ])
        let artifacts = TranscriptTextArtifactCache()
        for block in blocks {
            _ = artifacts.artifact(for: block, width: 600, theme: .ottam(.builtIn), resourceRoot: nil)
        }
        #expect(artifacts.count == 6)
        artifacts.trim(toMostRecent: 2)
        #expect(artifacts.count == 2)
        let builds = artifacts.buildCount
        _ = artifacts.artifact(for: blocks[5], width: 600, theme: .ottam(.builtIn), resourceRoot: nil)
        #expect(artifacts.buildCount == builds, "the most recent artifact is kept")
    }

    // MARK: Model

    @Test("a background store keeps merging; returning shows it and its detail without a load")
    func cachedReturn() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<4 { root.deleteLastPathComponent() }
        let data = try Data(contentsOf: root.appendingPathComponent("backend/crates/ottam-protocol/fixtures/conversation-get-response.json"))
        let detail = try JSONDecoder().decode(ProtocolResponse<ConversationGetResult>.self, from: data).result!.conversation
        let backend = DiffReviewModel(client: OttamCoreProcessClient(executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")))
        let model = ChatWorkspaceModel(transcriptCache: ConversationTranscriptCache(policy: .init()))
        let first = detail.conversation.conversationID

        model.select(first, backend: backend)
        model.applyLoadedDetail(detail, transcript: .success(page([TranscriptFixture.item("one")])))
        let store = model.transcriptStore
        model.select(b, backend: backend)
        #expect(model.transcriptStore !== store)
        #expect(model.isLoadingDetail, "an uncached conversation still loads")

        _ = model.mergeTranscriptDelta(delta(first, sequence: 2, items: [TranscriptFixture.item("two", position: 2)]),
                                       workspaceID: .init("workspace"), backend: backend)
        #expect(store.items["two"] != nil)
        #expect(model.transcriptCache.session(for: first)?.needsResync == false)
        _ = model.mergeTranscriptDelta(delta(first, sequence: 9, items: [TranscriptFixture.item("three", position: 3)]),
                                       workspaceID: .init("workspace"), backend: backend)
        #expect(model.transcriptCache.session(for: first)?.needsResync == true, "a gap waits for the return")

        model.select(first, backend: backend)
        #expect(model.transcriptStore === store)
        #expect(model.selectedDetail == detail)
        #expect(!model.isLoadingDetail)
        #expect(model.transcriptCache.session(for: first)?.needsResync == true, "kept until a resync finishes")
        model.select(b, backend: backend)
        #expect(model.transcriptCache.session(for: first)?.needsResync == true, "leaving early does not lose it")
    }

    // MARK: Renderer

    private let longSource = (0..<80).map {
        "Paragraph \($0) with enough text to keep the viewport detached."
    }.joined(separator: "\n\n")

    private func hostedController(_ cache: ConversationTranscriptCache) -> (MacAgentTranscriptViewController, NSWindow) {
        let controller = MacAgentTranscriptViewController()
        controller.viewSessions = cache
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 240)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 800, height: 240),
                              styleMask: [.titled], backing: .buffered, defer: false)
        // Swift owns the window; closing must not release it a second time.
        window.isReleasedWhenClosed = false
        window.contentView = controller.view
        controller.view.layoutSubtreeIfNeeded()
        return (controller, window)
    }

    private func waitUntil(_ condition: () -> Bool) async throws {
        for _ in 0..<300 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(condition())
    }

    private func show(_ controller: MacAgentTranscriptViewController, _ conversation: ConversationID, _ plan: [TranscriptItem]) {
        controller.apply(plan: plan, cards: [:], conversationID: conversation, theme: .ottam(.builtIn))
        controller.view.layoutSubtreeIfNeeded()
    }

    private func markdown(_ id: String, _ source: String) -> TranscriptItem {
        TranscriptItem(id: id, content: .markdown(source, syntax: .full, container: nil))
    }

    private func detachMidway(_ controller: MacAgentTranscriptViewController) throws -> TranscriptViewportAnchor {
        controller.scrollToBottom()
        let following = controller.debugLayoutState()
        controller.debugBeginLiveScroll()
        controller.debugScroll(toOffsetY: following.maximumOffsetY * 0.5)
        controller.debugEndLiveScroll()
        #expect(!controller.debugLayoutState().isFollowingBottom)
        return try #require(controller.debugViewportAnchor())
    }

    @Test("a cached return keeps its blocks, heights and place")
    func resumedReturn() throws {
        let cache = ConversationTranscriptCache()
        _ = cache.open(a)
        _ = cache.open(b)
        let (controller, window) = hostedController(cache)
        defer { window.close() }
        show(controller, a, [markdown("body", longSource)])
        let before = try detachMidway(controller)
        let builds = controller.debugTextArtifactBuildCount
        #expect(builds > 0)

        show(controller, b, [markdown("other", "Another conversation.")])
        #expect(controller.debugLayoutState().isFollowingBottom, "a first visit opens at the bottom")
        show(controller, a, [markdown("body", longSource)])

        let after = try #require(controller.debugViewportAnchor())
        #expect(after.id == before.id)
        #expect(abs(after.relativeY - before.relativeY) <= 1, "before=\(before.relativeY) after=\(after.relativeY)")
        #expect(!controller.debugLayoutState().isFollowingBottom)
        #expect(controller.debugTextArtifactBuildCount - builds < 3, "kept artifacts and heights skip remeasurement")
    }

    @Test("a cached return measures its rebuilt cards, not the heights it kept")
    func resumedReturnRemeasuresCards() throws {
        // Cards are not part of the kept rendering: a return builds fresh
        // ones, so an expanded card left behind comes back collapsed.
        final class Card: TranscriptCard {
            weak var hostedView: NSView?
            let height: CGFloat
            init(height: CGFloat) { self.height = height }
            func measuredHeight(forWidth width: CGFloat) -> CGFloat { height }
            func makeHostedView() -> NSView { NSView() }
            func adoptPayload(from fresh: any TranscriptCard) -> Bool { false }
        }
        let cache = ConversationTranscriptCache()
        _ = cache.open(a)
        _ = cache.open(b)
        let (controller, window) = hostedController(cache)
        defer { window.close() }
        let plan = [
            markdown("body", longSource),
            TranscriptItem(id: "task", content: .card(.turnTrailer(.persisted(
                runID: nil, body: "Body", createdAt: "now", outcome: .complete
            )))),
        ]
        controller.apply(plan: plan, cards: ["task": Card(height: 140)], conversationID: a, theme: .ottam(.builtIn))
        controller.view.layoutSubtreeIfNeeded()
        #expect(controller.debugBlockHeights().last == 140)

        show(controller, b, [markdown("other", "Another conversation.")])
        #expect(cache.hasViewSession(for: a), "the return resumes the kept rendering")
        controller.apply(plan: plan, cards: ["task": Card(height: 40)], conversationID: a, theme: .ottam(.builtIn))
        controller.view.layoutSubtreeIfNeeded()

        #expect(!cache.hasViewSession(for: a))
        #expect(controller.debugBlockHeights().last == 40)
    }

    @Test("an evicted return rebuilds, then finds the place by content")
    func evictedReturn() throws {
        let cache = ConversationTranscriptCache()
        _ = cache.open(a)
        _ = cache.open(b)
        let (controller, window) = hostedController(cache)
        defer { window.close() }
        show(controller, a, [markdown("body", longSource)])
        let before = try detachMidway(controller)
        let index = controller.debugCompletedBlockIDs().firstIndex(of: before.id)
        show(controller, b, [markdown("other", "Another conversation.")])
        cache.removeSessions(keeping: b)

        show(controller, a, [markdown("body", longSource)])
        let after = try #require(controller.debugViewportAnchor())
        #expect(controller.debugCompletedBlockIDs().firstIndex(of: after.id) == index)
        #expect(abs(after.relativeY - before.relativeY) <= 1)
        #expect(!controller.debugLayoutState().isFollowingBottom)
    }

    @Test("a place above the loaded rows pages back, then restores")
    func pageBackToPlace() async throws {
        let cache = ConversationTranscriptCache()
        cache.uiState(for: a).readingPlace = .block(itemID: "older", ordinal: 0, relativeY: 0)
        let (controller, window) = hostedController(cache)
        defer { window.close() }
        // Parallel main-actor suites can hold the main queue past the default.
        controller.placeWait = .seconds(60)
        var requests = 0
        controller.onNearTop = { requests += 1 }
        controller.updateHistoryPagination(cursor: "page-1", isLoading: false)
        show(controller, a, [markdown("body", longSource)])
        try await waitUntil { requests == 1 }
        #expect(controller.debugLayoutState().isFollowingBottom, "the bottom holds while paging")

        controller.updateHistoryPagination(cursor: nil, isLoading: false)
        show(controller, a, [markdown("older", "The place the reader left."), markdown("body", longSource)])
        try await waitUntil {
            controller.view.layoutSubtreeIfNeeded()
            return controller.debugViewportAnchor()?.id.itemID == "older"
        }
        let anchor = try #require(controller.debugViewportAnchor())
        #expect(anchor.id.itemID == "older")
        #expect(!controller.debugLayoutState().isFollowingBottom)
    }

    // MARK: Cards

    @Test("an activity group opens from the remembered disclosures and records its toggles")
    func groupExpansion() {
        let state = ChatConversationUIState()
        var factory = LiveChatTranscriptCardFactory(
            theme: .ottam(.builtIn), environment: HostedCardEnvironment(typography: .builtIn), actions: ChatTranscriptActions()
        )
        factory.disclosures = { state }
        let t1 = TranscriptFixture.activity("t1"), t2 = TranscriptFixture.activity("t2"), t0 = TranscriptFixture.activity("t0")
        let card = factory.makeCard(.activityGroup([t1, t2], isActive: false), id: "t1/group") as? HostedActivityGroupCard
        #expect(card?.payload.isExpanded == false)
        card?.actions.onToggleExpanded()
        #expect(state.isExpanded(["t2"]))
        // Paged back: the group now starts at an older member.
        let rebuilt = factory.makeCard(.activityGroup([t0, t1, t2], isActive: false), id: "t0/group") as? HostedActivityGroupCard
        #expect(rebuilt?.payload.isExpanded == true)
        rebuilt?.actions.onToggleExpanded()
        #expect(!state.isExpanded(["t0", "t1", "t2"]))
    }
}
