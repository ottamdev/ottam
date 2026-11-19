import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("Attention focus")
struct AttentionFocusTests {
    private let selected = ConversationID("a")

    private func inputs(
        active: Bool = true,
        selection: ConversationID? = ConversationID("a"),
        visible: Bool = true,
        awake: Bool = true,
        session: Bool = true,
        unlocked: Bool = true
    ) -> FocusInputs {
        FocusInputs(
            appIsActive: active,
            mainWorkspaceSelection: selection,
            mainWindowIsVisible: visible,
            screensAreAwake: awake,
            sessionIsActive: session,
            screenIsUnlocked: unlocked
        )
    }

    @Test("the main workspace window's selection is focus while everything is up")
    func focused() {
        #expect(FocusResolver.focus(inputs()) == selected)
    }

    @Test("any one condition down leaves no focus")
    func eachConditionClears() {
        #expect(FocusResolver.focus(inputs(active: false)) == nil)
        #expect(FocusResolver.focus(inputs(selection: nil)) == nil)
        #expect(FocusResolver.focus(inputs(visible: false)) == nil)
        #expect(FocusResolver.focus(inputs(awake: false)) == nil)
        #expect(FocusResolver.focus(inputs(session: false)) == nil)
        #expect(FocusResolver.focus(inputs(unlocked: false)) == nil)
    }
}

@Suite("Attention reducer")
struct AttentionReducerTests {
    private let workspace = WorkspaceID("w")

    private func entry(
        _ conversation: String,
        _ state: AttentionState,
        reason: AttentionReason = .completed
    ) -> AttentionEntry {
        AttentionEntry(
            workspaceID: workspace,
            conversationID: ConversationID(conversation),
            state: state,
            reason: reason,
            title: conversation.uppercased()
        )
    }

    private func change(
        _ revision: UInt64,
        _ conversation: String,
        _ state: AttentionState,
        notify: Bool = false
    ) -> AttentionChangedParams {
        AttentionChangedParams(revision: revision, entry: entry(conversation, state), notify: notify)
    }

    private func connected() -> AttentionReducer {
        var reducer = AttentionReducer()
        reducer.connect(to: CoreInstanceID("one"))
        return reducer
    }

    @Test("a notifying change posts and badges; none withdraws and clears the badge")
    func changeLifecycle() {
        var reducer = connected()
        let posted = reducer.apply(change(1, "a", .unseen, notify: true))
        let silent = reducer.apply(change(2, "a", .unseen))
        let cleared = reducer.apply(change(3, "a", .none))
        #expect(posted == [.post(entry("a", .unseen)), .setBadge(1)])
        #expect(silent == [])
        #expect(cleared == [.withdraw(ConversationID("a")), .setBadge(0)])
        #expect(reducer.entries.isEmpty)
    }

    @Test("a silent first change badges without posting")
    func silentChange() {
        var reducer = connected()
        let shown = reducer.apply(change(1, "a", .needsInput))
        let alerted = reducer.apply(change(2, "a", .needsInput, notify: true))
        #expect(shown == [.setBadge(1)])
        #expect(alerted == [.post(entry("a", .needsInput))])
    }

    @Test("a change of state withdraws the old banner first")
    func stateChange() {
        var reducer = connected()
        _ = reducer.apply(change(1, "a", .needsInput, notify: true))
        let effects = reducer.apply(change(2, "a", .unseen, notify: true))
        #expect(effects == [.withdraw(ConversationID("a")), .post(entry("a", .unseen))])
    }

    @Test("a change at or below the conversation's stamp is stale")
    func staleChange() {
        var reducer = connected()
        _ = reducer.apply(change(5, "a", .unseen))
        let older = reducer.apply(change(4, "a", .none))
        let same = reducer.apply(change(5, "a", .none))
        #expect(older == [])
        #expect(same == [])
        #expect(reducer.entries[ConversationID("a")]?.state == .unseen)
    }

    @Test("a snapshot replaces entries stamped at or below it and keeps newer ones")
    func snapshotKeepsNewer() {
        var reducer = connected()
        _ = reducer.apply(change(3, "a", .unseen))
        _ = reducer.apply(change(9, "b", .needsInput))
        let effects = reducer.apply(snapshot: AttentionListResult(
            revision: 5,
            entries: [entry("c", .unseen)]
        ))
        #expect(effects == [
            .withdraw(ConversationID("a")),
            .reconcile([ConversationID("b"): .needsInput, ConversationID("c"): .unseen]),
            .setBadge(2),
        ])
        #expect(Set(reducer.entries.keys) == [ConversationID("b"), ConversationID("c")])
    }

    @Test("changes the snapshot already holds are stale, for any conversation")
    func snapshotFloor() {
        var reducer = connected()
        _ = reducer.apply(snapshot: AttentionListResult(revision: 7, entries: []))
        let held = reducer.apply(change(6, "z", .unseen, notify: true))
        let newer = reducer.apply(change(8, "z", .unseen, notify: true))
        #expect(held == [])
        #expect(newer == [.post(entry("z", .unseen)), .setBadge(1)])
    }

    @Test("an older snapshot than one applied changes nothing")
    func olderSnapshot() {
        var reducer = connected()
        _ = reducer.apply(snapshot: AttentionListResult(revision: 7, entries: [entry("a", .unseen)]))
        let effects = reducer.apply(snapshot: AttentionListResult(revision: 6, entries: []))
        #expect(effects == [])
        #expect(reducer.entries.count == 1)
    }

    @Test("a new core instance resets stamps so its revision-zero snapshot replaces everything")
    func newInstance() {
        var reducer = connected()
        _ = reducer.apply(change(40, "a", .unseen))
        let sameReset = reducer.connect(to: CoreInstanceID("one"))
        let newReset = reducer.connect(to: CoreInstanceID("two"))
        #expect(!sameReset)
        #expect(newReset)
        #expect(reducer.entries.count == 1, "entries stay until the snapshot")
        let effects = reducer.apply(snapshot: AttentionListResult(
            revision: 0,
            entries: [entry("b", .unseen)]
        ))
        let cleared = reducer.apply(change(1, "b", .none))
        #expect(effects == [
            .withdraw(ConversationID("a")),
            .reconcile([ConversationID("b"): .unseen]),
            .setBadge(1),
        ])
        #expect(cleared == [.withdraw(ConversationID("b")), .setBadge(0)])
    }

    @Test("conversations are filtered by workspace and state")
    func filtering() {
        var reducer = connected()
        _ = reducer.apply(change(1, "a", .unseen))
        _ = reducer.apply(change(2, "b", .needsInput))
        _ = reducer.apply(AttentionChangedParams(
            revision: 3,
            entry: AttentionEntry(
                workspaceID: WorkspaceID("other"),
                conversationID: ConversationID("c"),
                state: .unseen,
                reason: .failed,
                title: "C"
            ),
            notify: false
        ))
        #expect(reducer.conversations(in: workspace, state: .unseen) == [ConversationID("a")])
        #expect(reducer.conversations(in: workspace, state: .needsInput) == [ConversationID("b")])
    }
}
