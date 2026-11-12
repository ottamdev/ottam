import AppKit
import OttamBackendClient
import Testing
@testable import OttamApp

@Suite("Transcript history recovery")
@MainActor
struct TranscriptHistoryRecoveryTests {
    private func drain() async throws {
        try await Task.sleep(for: .milliseconds(50))
    }

    private func controller(long: Bool = false) async throws -> MacAgentTranscriptViewController {
        let controller = MacAgentTranscriptViewController()
        controller.loadViewIfNeeded()
        controller.view.frame = NSRect(x: 0, y: 0, width: 800, height: 500)
        controller.view.layoutSubtreeIfNeeded()
        controller.apply(plan: [TranscriptItem(id: "reply", content: .markdown(
            long ? String(repeating: "A retained paragraph in the conversation.\n\n", count: 100) : "A short collapsed page.",
            syntax: .full, container: nil))], cards: [:],
            conversationID: ConversationID("conversation"), theme: .ottam(.builtIn))
        controller.view.layoutSubtreeIfNeeded()
        try await drain()
        return controller
    }

    @Test("a collapsed page keeps loading until its cursor is exhausted")
    func consecutiveCollapsedPages() async throws {
        let controller = try await controller()
        var requests = 0
        controller.onNearTop = { requests += 1 }
        controller.updateHistoryPagination(cursor: "page-1", isLoading: false)
        try await drain()
        #expect(requests == 1)
        controller.updateHistoryPagination(cursor: "page-1", isLoading: true)
        controller.scrollBack(viewports: 10)
        try await drain()
        #expect(requests == 1)
        // A failed fetch with the same cursor must not spin automatically.
        controller.updateHistoryPagination(cursor: "page-1", isLoading: false)
        try await drain()
        #expect(requests == 1)
        controller.updateHistoryPagination(cursor: "page-2", isLoading: false)
        try await drain()
        #expect(requests == 2)
        controller.updateHistoryPagination(cursor: nil, isLoading: false)
        controller.scrollBack(viewports: 10)
        try await drain()
        #expect(requests == 2)
    }

    @Test("a reader at the top can fetch consecutive pages without scrolling down")
    func consecutivePagesAtTop() async throws {
        let controller = try await controller(long: true)
        var requests = 0
        controller.onNearTop = { requests += 1 }
        controller.updateHistoryPagination(cursor: "page-1", isLoading: false)
        try await drain()
        #expect(requests == 0, "Following the tail must not load older history")
        controller.scrollBack(viewports: 100)
        #expect(requests == 1)
        controller.updateHistoryPagination(cursor: "page-2", isLoading: false)
        try await drain()
        #expect(requests == 2)
        controller.scrollBack(viewports: 100)
        #expect(requests == 2, "The same cursor must not request twice")
    }

    @Test("geometry commits use the final column width even when row heights are unchanged")
    func commitRepairsStaleDocumentWidth() throws {
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 800, height: 500))
        let collection = NSCollectionView(frame: NSRect(x: 0, y: 0, width: 220, height: 500))
        let layout = TranscriptCollectionLayout()
        collection.collectionViewLayout = layout
        scroll.documentView = collection
        layout.entries = [.init(id: .init(itemID: "card", generation: 0), height: 40, relation: .turn)]
        let coordinator = TranscriptLayoutCoordinator(scrollView: scroll, collectionView: collection, layout: layout)
        // Simulate a document retained at the temporary width from attachment.
        collection.setFrameSize(NSSize(width: 220, height: 500))
        layout.prepare()
        coordinator.commit(transactionID: 1, target: .deferred)
        let frame = try #require(layout.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))).frame
        #expect(abs(frame.width - min(ChatLayout.contentWidth, scroll.contentView.bounds.width - 44)) < 1)
        #expect(abs(collection.bounds.width - scroll.contentView.bounds.width) < 1)
    }

    @Test("older history prefetches two viewports before the top without repeating the cursor")
    func prefetchBeforeReachingTop() async throws {
        let controller = try await controller(long: true)
        var requests = 0
        controller.onNearTop = { requests += 1 }
        controller.updateHistoryPagination(cursor: "page-1", isLoading: false)
        try await drain()
        let top: CGFloat = 0
        controller.debugScroll(toOffsetY: top + 1_500)
        #expect(requests == 0)
        controller.debugScroll(toOffsetY: top + 750)
        #expect(requests == 1, "History should load before the old 120-point boundary")
        controller.updateHistoryPagination(cursor: "page-1", isLoading: true)
        controller.debugScroll(toOffsetY: top + 700)
        #expect(requests == 1)
        controller.updateHistoryPagination(cursor: "page-1", isLoading: false)
        try await drain()
        controller.debugScroll(toOffsetY: top + 650)
        #expect(requests == 1, "A failed request must not loop inside the prefetch zone")
        controller.updateHistoryPagination(cursor: "page-2", isLoading: false)
        try await drain()
        #expect(requests == 2)
    }

    @Test("reattachment reconciles stale row widths without a window resize")
    func attachmentRepairsStaleRows() async throws {
        let controller = try await controller()
        func findScroll(_ view: NSView) -> NSScrollView? {
            if let scroll = view as? NSScrollView { return scroll }
            return view.subviews.lazy.compactMap { findScroll($0) }.first
        }
        let scroll = try #require(findScroll(controller.view))
        let collection = try #require(scroll.documentView as? NSCollectionView)
        let layout = try #require(collection.collectionViewLayout as? TranscriptCollectionLayout)
        let expected = min(ChatLayout.contentWidth, scroll.contentView.bounds.width - 44)
        collection.setFrameSize(NSSize(width: 220, height: collection.frame.height))
        layout.prepare()
        #expect(layout.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))?.frame.width != expected)
        controller.viewDidAppear()
        controller.view.layoutSubtreeIfNeeded()
        try await drain()
        #expect(layout.layoutAttributesForItem(at: IndexPath(item: 0, section: 0))?.frame.width == expected)
        #expect(collection.visibleItems().allSatisfy { abs($0.view.frame.width - expected) < 1 })
    }
}
