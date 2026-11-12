import AppKit
import OttamInstrumentation
import OttamBackendClient
import Testing
@testable import OttamApp

@Suite("Streaming commit coordinator")
@MainActor
struct StreamingCommitCoordinatorTests {
    @Test("display tick payload reports callback spacing and missed configured slots")
    func displayTickPayload() {
        let first = TranscriptSignpost.DisplayTickPayload.calculate(
            configuredHz: 30,
            physicalHz: 120,
            timestamp: 10,
            previousTimestamp: nil,
            pendingBytes: 64,
            pendingDeltas: 2
        )
        let delayed = TranscriptSignpost.DisplayTickPayload.calculate(
            configuredHz: 30,
            physicalHz: 120,
            timestamp: 10.1,
            previousTimestamp: 10,
            pendingBytes: 192,
            pendingDeltas: 6
        )

        #expect(first.elapsedSlots == 1)
        #expect(first.missedSlots == 0)
        #expect(abs(delayed.callbackSpacingMilliseconds - 100) < 0.001)
        #expect(delayed.elapsedSlots == 3)
        #expect(delayed.missedSlots == 2)
        #expect(delayed.pendingBytes == 192)
        #expect(delayed.pendingDeltas == 6)
    }

    @Test("collection telemetry records frame, scroll, and visible-set outcomes")
    func collectionOutcomePayloads() {
        let unchangedFrame = TranscriptSignpost.CollectionFramePayload.calculate(
            currentWidth: 800,
            currentHeight: 1_200,
            requestedWidth: 800,
            requestedHeight: 1_200,
            actualWidth: 800,
            actualHeight: 1_200
        )
        let grownFrame = TranscriptSignpost.CollectionFramePayload.calculate(
            currentWidth: 800,
            currentHeight: 1_200,
            requestedWidth: 800,
            requestedHeight: 1_240,
            actualWidth: 800,
            actualHeight: 1_240
        )
        #expect(!unchangedFrame.changed)
        #expect(grownFrame.changed)
        #expect(grownFrame.actualHeightDelta == 40)

        let stationaryScroll = TranscriptSignpost.CollectionScrollPayload.calculate(
            priorY: 500,
            proposedY: 500,
            constrainedY: 500,
            actualY: 500
        )
        let movedScroll = TranscriptSignpost.CollectionScrollPayload.calculate(
            priorY: 500,
            proposedY: 544,
            constrainedY: 540,
            actualY: 540
        )
        #expect(!stationaryScroll.moved)
        #expect(movedScroll.moved)
        #expect(movedScroll.constrainedDeltaY == 40)

        let stableVisible = TranscriptSignpost.CollectionMaterializationPayload.calculate(
            neededLayoutBefore: false,
            visibleBefore: [IndexPath(item: 3, section: 0)],
            visibleAfter: [IndexPath(item: 3, section: 0)]
        )
        let changedVisible = TranscriptSignpost.CollectionMaterializationPayload.calculate(
            neededLayoutBefore: true,
            visibleBefore: [IndexPath(item: 3, section: 0)],
            visibleAfter: [
                IndexPath(item: 3, section: 0),
                IndexPath(item: 4, section: 0),
            ]
        )
        #expect(!stableVisible.visibleSetChanged)
        #expect(changedVisible.visibleSetChanged)
        #expect(changedVisible.appearedItems == 1)
        #expect(changedVisible.disappearedItems == 0)

        let query = TranscriptSignpost.CollectionAttributesPayload.calculate(
            requestedRect: CGRect(x: 0, y: 100, width: 800, height: 400),
            expandedRect: CGRect(x: 0, y: -500, width: 800, height: 1_600),
            itemFrames: [
                CGRect(x: 0, y: -200, width: 800, height: 100),
                CGRect(x: 0, y: 120, width: 800, height: 100),
                CGRect(x: 0, y: 700, width: 800, height: 100),
            ]
        )
        #expect(query.requestedHeight == 400)
        #expect(query.expandedHeight == 1_600)
        #expect(query.intersectingItems == 1)
        #expect(query.returnedItems == 3)
    }

    private func windowed(_ body: (NSWindow, NSView) throws -> Void) rethrows {
        let view = NSView(frame: NSRect(x: 0, y: 0, width: 10, height: 10))
        let window = NSWindow(
            contentRect: view.frame,
            styleMask: .borderless,
            backing: .buffered,
            defer: false
        )
        window.contentView = view
        window.setFrameOrigin(NSPoint(x: -10_000, y: -10_000))
        window.orderBack(nil)
        defer {
            window.orderOut(nil)
            window.contentView = nil
        }
        try body(window, view)
    }

    @Test("an initially screenless view creates its display link when a screen arrives")
    func neutralLifecycle() throws {
        try windowed { window, view in
            let coordinator = StreamingCommitCoordinator()
            var hasWork = false
            coordinator.hasStructuralWork = { hasWork }
            coordinator.startIfNeeded(on: view)

            #expect(coordinator.view === view)
            let screenObserver = coordinator.screenObserver
            #expect(screenObserver != nil)

            hasWork = true
            coordinator.wake()
            let screen = try #require(NSScreen.screens.first)
            window.setFrameOrigin(NSPoint(x: screen.frame.midX, y: screen.frame.midY))
            NotificationCenter.default.post(
                name: NSWindow.didChangeScreenNotification,
                object: window
            )
            #expect(coordinator.view === view)
            #expect(coordinator.displayLink != nil)
            #expect(
                (coordinator.screenObserver as AnyObject?) === (screenObserver as AnyObject?)
            )

            coordinator.detach(from: view)
            #expect(coordinator.view == nil)
            #expect(coordinator.displayLink == nil)
        }
    }

    @Test("rate and screen changes preserve the lifecycle attachment")
    func rateAndScreenRebinding() {
        windowed { window, view in
            let coordinator = StreamingCommitCoordinator()
            coordinator.startIfNeeded(on: view)
            coordinator.structuralRate = 15
            NotificationCenter.default.post(name: NSWindow.didChangeScreenNotification, object: window)

            #expect(coordinator.view === view)
            #expect(coordinator.structuralRate == 15)
            coordinator.invalidate()
        }
    }

    @Test("normalized text revisions publish once per display drain and structural boundaries bypass batching")
    func normalizedDisplayPublication() throws {
        try windowed { window, view in
            let screen = try #require(NSScreen.screens.first)
            window.setFrameOrigin(screen.frame.origin)
            let store = ChatTranscriptStore()
            let conversation = TranscriptFixture.conversationID
            store.merge(.init(coreInstanceID: .init("core"), sequence: 1,
                items: [TranscriptFixture.item("text", body: "A")]), conversationID: conversation)
            store.streamCommits.startIfNeeded(on: view)
            _ = try #require(store.streamCommits.displayLink)
            defer { store.streamCommits.invalidate() }
            var publications: [String] = []
            store.publishLiveText = { publications.append(store.bodies["text"] ?? ""); return true }
            for (revision, body) in [(2, "AB"), (3, "ABC")] {
                _ = store.merge(ConversationTranscriptDeltaParams(workspaceID: .init("workspace"),
                    conversationID: conversation, coreInstanceID: .init("core"), sequence: UInt64(revision),
                    items: [TranscriptFixture.item("text", revision: UInt64(revision), body: body)], bodyAppends: []))
            }
            #expect(publications.isEmpty)
            store.streamCommits.drainStructural?()
            #expect(publications == ["ABC"])
            store.streamCommits.drainStructural?()
            #expect(publications == ["ABC"])
            _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
                coreInstanceID: .init("core"), sequence: 4,
                items: [TranscriptFixture.item("next", position: 2, body: "boundary")], bodyAppends: []))
            #expect(store.streamCommits.hasStructuralWork?() == false)
            #expect(publications == ["ABC"])
        }
    }
}
