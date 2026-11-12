import AppKit
import OttamBackendClient
import Testing
@testable import OttamApp

/// Records the spec it was built from; adopts any same-type fresh card.
class SpecCard: TranscriptCard {
    weak var hostedView: NSView?
    var spec: ChatTranscriptCardSpec
    init(spec: ChatTranscriptCardSpec) {
        self.spec = spec
    }
    func makeHostedView() -> NSView { NSView() }
    func measuredHeight(forWidth width: CGFloat) -> CGFloat { 40 }
    func adoptPayload(from fresh: any TranscriptCard) -> Bool {
        guard type(of: fresh) == type(of: self), let fresh = fresh as? SpecCard, fresh.spec != spec else { return false }
        spec = fresh.spec
        return true
    }
}

/// A second concrete type, so a spec change can force a replacement.
final class OtherSpecCard: SpecCard {}

struct StubFactory: ChatTranscriptCardFactory {
    /// Specs the stub refuses, to exercise the unmaterialized path.
    var declinesTrailers = false
    func makeCard(_ spec: ChatTranscriptCardSpec, id: String) -> (any TranscriptCard)? {
        switch spec {
        case .turnTrailer where declinesTrailers: return nil
        case .deliveryRecovery: return OtherSpecCard(spec: spec)
        default: return SpecCard(spec: spec)
        }
    }
}

@Suite("Chat transcript card store")
@MainActor
struct ChatTranscriptCardStoreTests {
    private func group(_ id: String, active: Bool) -> TranscriptItem {
        .init(id: id, content: .card(.activityGroup([TranscriptFixture.activity("\(id)/0")], isActive: active)))
    }
    private let text = TranscriptItem(id: "t", content: .markdown("hi", syntax: .full, container: .bubble))

    @Test("cards are reused by ID; markdown does not enter the card store")
    func reuse() {
        let store = ChatTranscriptCardStore(factory: StubFactory())
        let first = store.reconcile([text, group("g", active: true)])
        let second = store.reconcile([text, group("g", active: true)])
        #expect(first.cards.keys.sorted() == ["g"])
        #expect(first.cards["g"] === second.cards["g"], "same instance carried forward")
    }

    @Test("a swapped factory builds new cards with the new conversation's actions")
    func factorySwap() {
        var opened: [String] = []
        func factory(_ conversation: String) -> LiveChatTranscriptCardFactory {
            var actions = ChatTranscriptActions()
            actions.openSubagentThread = { opened.append("\(conversation):\($0)") }
            return LiveChatTranscriptCardFactory(
                theme: .ottam(.builtIn), environment: HostedCardEnvironment(typography: .builtIn), actions: actions
            )
        }
        let store = ChatTranscriptCardStore(factory: factory("a"))
        _ = store.reconcile([group("g1", active: false)])
        store.factory = factory("b")
        let card = store.reconcile([group("g2", active: false)]).cards["g2"] as? HostedActivityGroupCard
        card?.actions.onOpenThread("thread")
        #expect(opened == ["b:thread"])
    }

    @Test("a changed spec is adopted in place")
    func adoptInPlace() {
        let store = ChatTranscriptCardStore(factory: StubFactory())
        let before = store.reconcile([group("g", active: true)]).cards["g"]
        let result = store.reconcile([group("g", active: false)])
        #expect(result.cards["g"] === before)
        #expect((before as? SpecCard)?.spec == .activityGroup([TranscriptFixture.activity("g/0")], isActive: false))
    }

    @Test("the live card factory retains one trailer host across run completion")
    func turnTrailerAdoptsInPlace() throws {
        let store = ChatTranscriptCardStore(factory: LiveChatTranscriptCardFactory(
            theme: .ottam(.builtIn),
            environment: HostedCardEnvironment(typography: .builtIn),
            actions: ChatTranscriptActions()
        ))
        let runID = ConversationRunID("run-1")
        let id = "run/run-1/trailer"
        let running = TranscriptItem(
            id: id,
            content: .card(.turnTrailer(.running(
                runID: runID, status: .running, startedAt: nil, activityCount: 1
            )))
        )
        let finished = TranscriptItem(
            id: id,
            content: .card(.turnTrailer(.finished(
                runID: runID, body: "stand-in", outcome: .complete
            )))
        )
        let persisted = TranscriptItem(
            id: id,
            content: .card(.turnTrailer(.persisted(
                runID: runID,
                body: "canonical",
                createdAt: "2026-08-21T10:00:00Z",
                outcome: .complete
            )))
        )

        let liveCard = try #require(store.reconcile([running]).cards[id] as? HostedTurnTrailerCard)
        let finishedCard = try #require(store.reconcile([finished]).cards[id] as? HostedTurnTrailerCard)
        let persistedCard = try #require(store.reconcile([persisted]).cards[id] as? HostedTurnTrailerCard)

        #expect(finishedCard === liveCard)
        #expect(persistedCard === liveCard)
        #expect(persistedCard.payload.presentation == .persisted(
            runID: runID,
            body: "canonical",
            createdAt: "2026-08-21T10:00:00Z",
            outcome: .complete
        ))
    }

    @Test("a spec the existing card cannot adopt is replaced")
    func replaceOnTypeMismatch() {
        let store = ChatTranscriptCardStore(factory: StubFactory())
        let input = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed)
        let status = TranscriptItem(id: "runinput/ri", content: .card(TranscriptFixture.userTrailer(input: input, showsResend: true)))
        let recovery = TranscriptItem(id: "runinput/ri", content: .card(.deliveryRecovery(input, isMutating: false)))
        let before = store.reconcile([status]).cards[status.id]
        let result = store.reconcile([recovery])
        #expect(result.cards[recovery.id] !== before)
        #expect(result.cards[recovery.id] is OtherSpecCard)
    }

    /// A streamed reasoning revision changes the spec but nothing visible;
    /// the hosted card reports no change and must stay the same host.
    @Test("a same-type spec change with no visible change keeps the card")
    func keepUnchangedAdoption() throws {
        let store = ChatTranscriptCardStore(factory: LiveChatTranscriptCardFactory(
            theme: .ottam(.builtIn), environment: HostedCardEnvironment(typography: .builtIn), actions: ChatTranscriptActions()
        ))
        func thought(_ revision: UInt64) -> TranscriptItem {
            TranscriptItem(id: "g", content: .card(.activityGroup([
                ChatLiveActivity(id: "g/0", label: "Thinking", kind: "think", revision: revision, bodyByteCount: revision * 10)
            ], isActive: true)))
        }
        let first = try #require(store.reconcile([thought(1)]).cards["g"])
        let second = try #require(store.reconcile([thought(2)]).cards["g"] as? HostedActivityGroupCard)
        #expect(second === first)
        #expect(second.payload.activities.first?.revision == 2)
    }

    @Test("IDs that leave the plan are evicted; unsupported specs are reported")
    func evictAndDecline() {
        let store = ChatTranscriptCardStore(factory: StubFactory(declinesTrailers: true))
        let trailer = TranscriptItem(
            id: "message/m/trailer",
            content: .card(.turnTrailer(.persisted(runID: nil, body: "", createdAt: "", outcome: .complete)))
        )
        let first = store.reconcile([group("g", active: true), trailer])
        #expect(first.cards.keys.sorted() == ["g"])
        #expect(first.unmaterializedIDs == ["message/m/trailer"])
        let gone = store.reconcile([])
        #expect(gone.cards.isEmpty)
        let back = store.reconcile([group("g", active: true)])
        #expect(back.cards["g"] !== first.cards["g"], "evicted card is not resurrected")
    }

    @Test("the live factory keeps a permission card's disclosure across reconciliation")
    func liveFactory() {
        var resolved: (ChatPendingPermission, String?)?
        var actions = ChatTranscriptActions()
        actions.resolvePermission = { resolved = ($0, $1) }
        let factory = LiveChatTranscriptCardFactory(
            theme: .ottam(.builtIn), environment: HostedCardEnvironment(typography: .builtIn), actions: actions
        )
        let store = ChatTranscriptCardStore(factory: factory)
        let permission = HostedPermissionCardTests.previewPermission()
        let entry = TranscriptItem(id: "permission/x", content: .card(.pendingPermission(permission)))
        let card = store.reconcile([entry]).cards[entry.id] as? HostedPermissionCard
        card?.actions.onToggleDetails()
        let again = store.reconcile([entry]).cards[entry.id] as? HostedPermissionCard
        #expect(again === card && again?.payload.showsDetails == true)
        card?.actions.onResolve("allow_once")
        #expect(resolved?.0 === permission && resolved?.1 == "allow_once")
        #expect(factory.makeCard(
            .turnTrailer(.persisted(runID: nil, body: "", createdAt: "", outcome: .complete)),
            id: "f"
        ) is HostedTurnTrailerCard)
    }
}
