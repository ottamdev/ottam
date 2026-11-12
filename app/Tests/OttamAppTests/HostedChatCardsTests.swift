import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import OttamPresentationLogic
import Testing
@testable import OttamApp

/// The stage-3.4 conformers: height is a pure function of the payload, the
/// lifted UI state changes the height and survives adoption, and each card
/// stands in for itself in the accessibility tree while retired.
@Suite("Hosted chat cards")
@MainActor
struct HostedChatCardsTests {
    private let theme = MarkdownTheme.ottam(.builtIn)
    private let environment = HostedCardEnvironment(typography: .builtIn)
    private var factory: LiveChatTranscriptCardFactory {
        LiveChatTranscriptCardFactory(theme: theme, environment: environment, actions: ChatTranscriptActions())
    }

    @Test("an activity group's disclosure is payload state that survives a rebuild")
    func activityGroup() throws {
        let activities = [TranscriptFixture.activity("g/0"), TranscriptFixture.activity("g/1", label: "Ran tests")]
        let card = try #require(factory.makeCard(.activityGroup(activities, isActive: true), id: "g") as? HostedActivityGroupCard)
        let collapsed = card.height(forWidth: 500)
        card.actions.onToggleExpanded()
        #expect(card.height(forWidth: 500) > collapsed)
        let fresh = try #require(factory.makeCard(.activityGroup(activities, isActive: false), id: "g") as? HostedActivityGroupCard)
        #expect(card.adoptPayload(from: fresh))
        #expect(card.payload.isExpanded && !card.payload.isActive)
        #expect(card.accessibilityPlaceholder?.identifier == AccessibilityID.chatActivityGroup)
        #expect(card.accessibilityPlaceholder?.value == "finished")
    }

    @Test("an adopted card's disclosure still toggles after the fresh card is gone")
    func activityGroupTogglesAfterAdoption() throws {
        let activities = [TranscriptFixture.activity("g/0")]
        let card = try #require(factory.makeCard(.activityGroup(activities, isActive: true), id: "g") as? HostedActivityGroupCard)
        func adoptFresh() throws {
            let grown = activities + [TranscriptFixture.activity("g/1", label: "Ran tests")]
            let fresh = try #require(factory.makeCard(.activityGroup(grown, isActive: true), id: "g"))
            #expect(card.adoptPayload(from: fresh))
        }
        try adoptFresh()
        card.actions.onToggleExpanded()
        #expect(card.payload.isExpanded)
    }

    @Test("expanded activity input can refresh synchronously while adopting a live update")
    func activityDetailsRefreshDuringAdoption() async throws {
        var activity = TranscriptFixture.activity("g/0")
        activity.bodyByteCount = 3
        activity.revision = 1
        let card = HostedActivityGroupCard.activityGroup([activity], isActive: true, theme: theme, environment: environment)
        card.payload.details?.catchUpInterval = .zero
        let view = card.makeHostedView()
        card.actions.onToggleExpanded()
        card.actions.onToggleItem(activity)
        let details = try #require(card.payload.details)
        defer { details.close() }
        for _ in 0..<10_000 {
            if !details.loading && details.input?.loading == false { break }
            await Task.yield()
        }
        #expect(!details.loading && details.input?.loading == false)
        let revision = card.payload.detailRevision
        var refreshes = 0
        card.onPayloadChanged = { card in
            _ = card.makeRoot()
            refreshes += 1
        }

        activity.bodyByteCount = 6
        activity.revision = 2
        let fresh = HostedActivityGroupCard.activityGroup([activity], isActive: true, theme: theme, environment: environment)
        #expect(card.adoptPayload(from: fresh))
        #expect(card.payload.details === details)
        #expect(details.expandedID == activity.id)
        #expect(card.payload.isExpanded)
        #expect(card.payload.detailRevision > revision)
        #expect(refreshes > 0)
        #expect(card.hostedView === view)
        card.actions.onToggleItem(activity)
        #expect(details.expandedID == nil)
    }

    @Test("inherited details refresh the replacement card after its predecessor is released")
    func inheritedActivityDetails() async throws {
        let activity = ChatLiveActivity(id: "thought", label: "Thinking", kind: "think", revision: 1, bodyByteCount: 3)
        var old: HostedActivityGroupCard? = .activityGroup([activity], isActive: true, theme: theme, environment: environment)
        old?.actions.onToggleExpanded()
        old?.actions.onToggleItem(activity)
        let details = try #require(old?.payload.details)
        defer { details.close() }
        weak let retired = old
        let activities = [TranscriptFixture.activity("earlier"), activity]
        let replacement = HostedActivityGroupCard.activityGroup(activities, isActive: true, theme: theme, environment: environment)
        let view = replacement.makeHostedView()
        replacement.inheritState(from: try #require(old))
        old = nil
        #expect(retired == nil)
        #expect(replacement.payload.activities == activities)
        #expect(replacement.payload.details === details)
        #expect(replacement.payload.isExpanded)
        #expect(replacement.hostedView === view)
        var refreshes = 0
        replacement.onPayloadChanged = { _ in refreshes += 1 }
        for _ in 0..<10_000 {
            if !details.loading { break }
            await Task.yield()
        }
        #expect(!details.loading)
        #expect(refreshes > 0, "the pending detail read must refresh the replacement")
        let expandedHeight = replacement.height(forWidth: 500)
        let revision = replacement.payload.detailRevision
        replacement.actions.onToggleItem(activity)
        #expect(details.expandedID == nil)
        #expect(replacement.payload.detailRevision > revision)
        #expect(replacement.height(forWidth: 500) < expandedHeight)
    }

    @Test("streamed reasoning neither redraws nor re-measures the card")
    func reasoningGrowthKeepsHeight() throws {
        func thought(_ revision: UInt64) -> ChatLiveActivity {
            var activity = ChatTranscriptAssembler.reasoningActivity(
                id: "thought", body: "Planning\n" + String(repeating: "more reasoning ", count: Int(revision) * 100)
            )
            activity.revision = revision
            activity.bodyByteCount = revision * 1_500
            return activity
        }
        #expect(try #require(thought(3).detail).count <= ChatTranscriptAssembler.reasoningPreviewLength + 1)
        let card = HostedActivityGroupCard.activityGroup([thought(1)], isActive: true, theme: theme, environment: environment)
        let height = card.height(forWidth: 500)
        let heightSignature = card.payload.heightSignature
        let fresh = HostedActivityGroupCard.activityGroup([thought(2)], isActive: true, theme: theme, environment: environment)
        #expect(!card.adoptPayload(from: fresh), "a revision alone is not a visible change")
        #expect(card.payload.activities.first?.revision == 2, "the detail store still sees the new revision")
        #expect(card.payload.heightSignature == heightSignature)
        #expect(card.height(forWidth: 500) == height)
        #expect(card.measurementCount == 1)
        card.actions.onToggleExpanded()
        #expect(card.height(forWidth: 500) > height)
        #expect(card.measurementCount == 2)
    }

    @Test("expanded wrapped reasoning grows to its cap without remeasuring later chunks")
    func wrappedReasoningHeight() async throws {
        let texts = [
            1: "Planning",
            2: String(repeating: "wrapped reasoning ", count: 6),
            3: String(repeating: "wrapped reasoning ", count: 200),
            4: String(repeating: "wrapped reasoning ", count: 400)
        ]
        func thought(_ revision: Int) -> ChatLiveActivity {
            ChatLiveActivity(
                id: "thought", label: "Thinking", kind: "think",
                revision: UInt64(revision), bodyByteCount: UInt64(texts[revision]!.utf8.count)
            )
        }
        func makeCard(_ revision: Int) -> HostedActivityGroupCard {
            .activityGroup([thought(revision)], isActive: false, theme: theme, environment: environment, readBody: { id, revision, _ in
                .init(itemID: id, revision: revision, text: texts[Int(revision)] ?? texts[4]!)
            })
        }
        let card = makeCard(1)
        card.payload.details?.catchUpInterval = .zero
        card.actions.onToggleItem(thought(1))
        let details = try #require(card.payload.details)
        defer { details.close() }
        func wait(for text: String) async {
            for _ in 0..<10_000 {
                if !details.loading && details.text == text { return }
                await Task.yield()
            }
            Issue.record("Detail request did not settle")
        }
        await wait(for: texts[1]!)
        let shortHeight = card.height(forWidth: 300)

        _ = card.adoptPayload(from: makeCard(2))
        await wait(for: texts[2]!)
        let wrappedHeight = card.height(forWidth: 300)
        #expect(wrappedHeight > shortHeight)

        _ = card.adoptPayload(from: makeCard(3))
        await wait(for: texts[3]!)
        let cappedHeight = card.height(forWidth: 300)
        #expect(cappedHeight > wrappedHeight)
        let measurementsAtCap = card.measurementCount

        _ = card.adoptPayload(from: makeCard(4))
        await wait(for: texts[4]!)
        #expect(card.height(forWidth: 300) == cappedHeight)
        #expect(card.measurementCount == measurementsAtCap)
    }

    @Test("a retired plan row exposes its plan text without a payload disclosure")
    func planAccessibility() {
        let plan = ChatLiveActivity(id: "plan", label: "Plan", detail: "in_progress: Inspect saved payloads", kind: "plan")
        let card = HostedActivityGroupCard.activityGroup([plan], isActive: false, theme: theme, environment: environment)
        #expect(card.accessibilityPlaceholder?.identifier == "chat.plan.activity")
        #expect(card.accessibilityPlaceholder?.value == plan.detail)
    }

    @Test("the turn trailer keeps one host and fixed geometry through every phase")
    func turnTrailerLifecycle() throws {
        let runID = ConversationRunID("run-live")
        let factory = LiveChatTranscriptCardFactory(
            theme: theme,
            environment: environment,
            actions: ChatTranscriptActions()
        )
        let presentations: [ChatTurnTrailerPresentation] = [
            .running(
                runID: runID,
                status: .queued,
                startedAt: "2026-08-31T18:00:00Z",
                activityCount: 0
            ),
            .running(
                runID: runID,
                status: .running,
                startedAt: "2026-08-31T18:00:00Z",
                activityCount: 12
            ),
            .finished(runID: runID, body: "stand-in", outcome: .complete),
            .persisted(
                runID: runID,
                body: "canonical",
                createdAt: "2026-08-21T10:00:00Z",
                outcome: .complete
            ),
            .persisted(
                runID: runID,
                body: "partial",
                createdAt: "2026-08-21T10:00:00Z",
                outcome: .endedEarly
            ),
        ]
        let cards = try presentations.map { presentation in
            try #require(factory.makeCard(
                .turnTrailer(presentation), id: "run/run-live/trailer"
            ) as? HostedTurnTrailerCard)
        }

        for width in [240.0, 500.0] {
            let heights = cards.map { $0.height(forWidth: width) }
            #expect(Set(heights).count == 1, "trailer heights at \(width): \(heights)")
            // And one line, not two. The strut is zero-width, so it only
            // measures right when it is fixed-size first; without that it
            // wraps and every trailer reserves twice the room it needs.
            #expect(heights.allSatisfy { $0 < 24 }, "trailer heights at \(width): \(heights)")
        }

        let retained = cards[0]
        for fresh in cards.dropFirst() {
            #expect(retained.adoptPayload(from: fresh))
        }
        #expect(retained.payload.presentation == presentations.last)
        #expect(retained.accessibilityPlaceholder?.identifier == AccessibilityID.chatPartialOutput)

        let queued = try #require(factory.makeCard(.turnTrailer(.running(
            runID: runID,
            status: .queued,
            startedAt: "2026-08-31T18:00:00Z",
            activityCount: 0
        )), id: "status") as? HostedTurnTrailerCard)
        #expect(queued.accessibilityPlaceholder?.identifier == AccessibilityID.chatRunStatus)
        #expect(queued.accessibilityPlaceholder?.value == "starting")
    }

    @Test("a card item's accessibility identity follows the payload it adopted")
    func cardItemIdentityFollowsAdoptedPayload() throws {
        let runID = ConversationRunID("run-live")
        let factory = LiveChatTranscriptCardFactory(
            theme: theme,
            environment: environment,
            actions: ChatTranscriptActions()
        )
        func card(_ presentation: ChatTurnTrailerPresentation) throws -> HostedTurnTrailerCard {
            try #require(
                factory.makeCard(.turnTrailer(presentation), id: "run/run-live/trailer")
                    as? HostedTurnTrailerCard
            )
        }

        let live = try card(.running(
            runID: runID,
            status: .running,
            startedAt: "2026-08-31T18:00:00Z",
            activityCount: 0
        ))
        let item = TranscriptCardCollectionItem()
        item.configure(card: live)
        let hosted = try #require(item.view.subviews.first)
        #expect(hosted.accessibilityIdentifier() == AccessibilityID.chatRunStatus)

        // The store adopts a settled payload into the same card object rather
        // than replacing it, so the item is reconfigured with a card it already
        // holds — the path that used to return early and strand the identifier.
        let settled = try card(.finished(runID: runID, body: "done", outcome: .complete))
        #expect(live.adoptPayload(from: settled))
        item.configure(card: live)
        // AppKit reads a cleared identifier back as "" rather than nil.
        #expect(
            hosted.accessibilityIdentifier().isEmpty,
            "a settled turn must stop announcing itself as the live run"
        )
    }

    @Test("Escape stop confirmation requires a second press within three seconds")
    func escapeStopConfirmation() {
        let clock = ContinuousClock()
        let start = clock.now
        var confirmation = ChatEscapeStopConfirmation()

        let firstPressStops = confirmation.press(at: start)
        #expect(!firstPressStops)
        #expect(confirmation.isArmed)
        let secondPressStops = confirmation.press(at: start.advanced(by: .seconds(2)))
        #expect(secondPressStops)
        #expect(!confirmation.isArmed)

        let laterFirstPressStops = confirmation.press(at: start.advanced(by: .seconds(10)))
        let expiredSecondPressStops = confirmation.press(at: start.advanced(by: .seconds(14)))
        #expect(!laterFirstPressStops)
        #expect(!expiredSecondPressStops)
        #expect(confirmation.isArmed)
    }

    @Test("a permission record expands to its request JSON")
    func permissionRecord() throws {
        let record = ChatPermissionRecord(title: "Run a command", outcomeLabel: "Allowed once", wasAllowed: true, detailJSON: "{\n \"a\": 1\n}")
        let card = try #require(factory.makeCard(.permissionRecord(record), id: "p") as? HostedPermissionRecordCard)
        let collapsed = card.height(forWidth: 500)
        card.actions.onToggleExpanded()
        #expect(card.height(forWidth: 500) > collapsed)
        #expect(card.accessibilityPlaceholder?.value == "allowed")
    }

    @Test("the trailer copies stand-in then canonical text without changing geometry")
    func trailerCopy() throws {
        var copied: String?
        var actions = ChatTranscriptActions()
        actions.copy = { copied = $0 }
        let factory = LiveChatTranscriptCardFactory(theme: theme, environment: environment, actions: actions)
        let runID = ConversationRunID("run-copy")
        let card = try #require(factory.makeCard(
            .turnTrailer(.finished(runID: runID, body: "stand-in", outcome: .complete)),
            id: "run/run-copy/trailer"
        ) as? HostedTurnTrailerCard)
        let height = card.height(forWidth: 500)
        let signature = card.payload.signature

        card.actions.onCopy()
        #expect(copied == "stand-in")
        #expect(card.payload.showsCopied && card.payload.signature == signature)

        let persisted = try #require(factory.makeCard(
            .turnTrailer(.persisted(
                runID: runID,
                body: "canonical",
                createdAt: "2026-08-21T10:00:00Z",
                outcome: .endedEarly
            )),
            id: "run/run-copy/trailer"
        ))
        #expect(card.adoptPayload(from: persisted))
        #expect(card.payload.showsCopied, "copy confirmation survives phase adoption")
        #expect(card.height(forWidth: 500) == height)
        card.actions.onCopy()
        #expect(copied == "canonical")
    }

    @Test("a user trailer copies its message and keeps its height across states")
    func userTrailerCopy() throws {
        var copied: String?
        var actions = ChatTranscriptActions()
        actions.copy = { copied = $0 }
        let factory = LiveChatTranscriptCardFactory(theme: theme, environment: environment, actions: actions)
        let card = try #require(factory.makeCard(
            TranscriptFixture.userTrailer(body: "the question"), id: "message/m1/trailer"
        ) as? HostedUserTrailerCard)
        let height = card.height(forWidth: 500)
        #expect(height < 24, "a one-line row, not a wrapped strut: \(height)")

        card.actions.onCopy()
        #expect(copied == "the question")
        #expect(card.payload.showsCopied)

        // A delivery state joins the row without changing what it measures:
        // every state is one line, so the cached height has to survive.
        let failed = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed)
        let warned = try #require(factory.makeCard(
            TranscriptFixture.userTrailer(body: "the question", input: failed),
            id: "message/m1/trailer"
        ))
        #expect(card.adoptPayload(from: warned))
        #expect(card.payload.showsCopied, "copy confirmation survives adoption")
        #expect(card.height(forWidth: 500) == height)
        #expect(card.payload.state?.label == "Failed")
    }

    @Test("an attachment-only user trailer has nothing to copy")
    func userTrailerWithoutABody() throws {
        var copied: String?
        var actions = ChatTranscriptActions()
        actions.copy = { copied = $0 }
        let factory = LiveChatTranscriptCardFactory(theme: theme, environment: environment, actions: actions)
        let card = try #require(factory.makeCard(
            TranscriptFixture.userTrailer(body: ""), id: "message/m1/trailer"
        ) as? HostedUserTrailerCard)
        card.actions.onCopy()
        #expect(copied == nil)
        #expect(!card.payload.showsCopied)

        // Without the copy button the row is genuinely shorter, so the two
        // shapes must not share a cached measurement. A constant signature
        // would hand this row the taller one and leave the gap it just lost.
        let bare = card.height(forWidth: 500)
        let copyable = try #require(factory.makeCard(
            TranscriptFixture.userTrailer(body: "the question"), id: "message/m1/trailer"
        ))
        #expect(card.adoptPayload(from: copyable))
        let adopted = card.height(forWidth: 500)
        #expect(adopted > bare, "adopting a copyable body remeasures: \(bare) → \(adopted)")
    }

    @Test("delivery cards carry their accessibility value")
    func delivery() throws {
        let failed = TranscriptFixture.runInput("ri", messageID: "m", status: .failed)
        let status = try #require(factory.makeCard(TranscriptFixture.userTrailer(input: failed, showsResend: true), id: "s") as? HostedUserTrailerCard)
        #expect(status.height(forWidth: 500) > 0)
        #expect(status.accessibilityPlaceholder == nil, "only a user-marked delivery is asserted on")
        let unknown = TranscriptFixture.runInput("ri", messageID: "m", status: .deliveryUnknown)
        let recovery = try #require(factory.makeCard(.deliveryRecovery(unknown, isMutating: false), id: "r") as? HostedDeliveryRecoveryCard)
        #expect(recovery.accessibilityPlaceholder?.identifier == AccessibilityID.chatDeliveryRecovery)
        #expect(recovery.accessibilityPlaceholder?.value == "delivery unknown")
    }

    @Test("runtime and auth recovery cards re-measure on their flattened controller state")
    func recovery() throws {
        let runtime = ChatRuntimeRecovery(engine: TranscriptFixture.engine(), problem: .missing(component: "node"), detail: nil)
        let idle = try #require(factory.makeCard(.runtimeRecovery(runtime, install: nil, isChecking: false), id: "rt") as? HostedRuntimeRecoveryCard)
        let installing = try #require(factory.makeCard(
            .runtimeRecovery(runtime, install: .init(phase: .downloading, bytesReceived: 1, bytesTotal: 2), isChecking: false), id: "rt"
        ) as? HostedRuntimeRecoveryCard)
        #expect(idle.adoptPayload(from: installing))
        #expect(idle.payload.install?.phase == .downloading)
        #expect(idle.height(forWidth: 500) > 0)
        #expect(idle.accessibilityPlaceholder?.identifier == AccessibilityID.chatRuntimeRecovery)

        let auth = ChatAuthRecovery(engine: TranscriptFixture.engine(), phase: .signedOut, undeliveredInput: nil)
        let card = try #require(factory.makeCard(.authRecovery(auth, failure: nil, isTerminalLogin: false, isMutating: false), id: "a") as? HostedAuthRecoveryCard)
        let plain = card.height(forWidth: 500)
        let failed = try #require(factory.makeCard(.authRecovery(auth, failure: "Login timed out", isTerminalLogin: false, isMutating: false), id: "a") as? HostedAuthRecoveryCard)
        #expect(card.adoptPayload(from: failed))
        #expect(card.height(forWidth: 500) > plain)
        #expect(card.accessibilityPlaceholder?.value == "signed out")
    }

    @Test("a re-themed store's cards inherit the open disclosures of the cards they replace")
    func rethemeInheritsState() throws {
        let entry = TranscriptItem(
            id: "g", content: .card(.activityGroup([TranscriptFixture.activity("g/0")], isActive: false))
        )
        let old = ChatTranscriptCardStore(factory: factory)
        let oldCard = try #require(old.reconcile([entry]).cards[entry.id] as? HostedActivityGroupCard)
        oldCard.actions.onToggleExpanded()
        let bigger = LiveChatTranscriptCardFactory(
            theme: .ottam(.builtIn), environment: environment, actions: ChatTranscriptActions()
        )
        let new = ChatTranscriptCardStore(factory: bigger)
        let newCard = try #require(
            new.reconcile([entry], inheritingStateFrom: old).cards[entry.id]
                as? HostedActivityGroupCard
        )
        #expect(newCard !== oldCard && newCard.payload.isExpanded)
    }

    @Test("Loading adjacent older activities preserves disclosure by member identity")
    func paginatedDisclosureState() throws {
        let store = ChatTranscriptCardStore(factory: factory)
        let current = TranscriptItem(id: "new/group", content: .card(.activityGroup([TranscriptFixture.activity("new")], isActive: false)))
        let card = try #require(store.reconcile([current]).cards[current.id] as? HostedActivityGroupCard)
        card.actions.onToggleExpanded()
        let expanded = TranscriptItem(id: "old/group", content: .card(.activityGroup([
            TranscriptFixture.activity("old"), TranscriptFixture.activity("new")
        ], isActive: false)))
        let reloaded = try #require(store.reconcile([expanded]).cards[expanded.id] as? HostedActivityGroupCard)
        #expect(reloaded.payload.isExpanded)
    }

    @Test("Copy uses the normalized local item body before and after reload")
    func normalizedCopySources() {
        var snapshot = TranscriptFixture.snapshot()
        snapshot.normalizedItems = [TranscriptFixture.item("part-a", body: "first"), TranscriptFixture.item("part-b", position: 2, body: "second")]
        let sources = ChatTranscriptPresentationCoordinator.copySources(snapshot)
        #expect(sources["part-a"] == "first")
        #expect(sources["part-b"] == "second")
        #expect(sources["m1"] == "question")
    }
}
