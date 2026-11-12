import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Subagent threads")
@MainActor
struct SubagentThreadTests {
    private let conversation = TranscriptFixture.conversationID
    private let core = CoreInstanceID("core")
    private let run = ConversationRunID("run-1")

    @Test("a page replaces the live thread counts and a delta revises them")
    func storeCounts() {
        let store = ChatTranscriptStore()
        store.merge(.init(coreInstanceID: core, sequence: 1, items: [],
                          subagentThreads: [.init(runID: run, live: 2)]), conversationID: conversation)
        #expect(store.subagentThreads == [run: 2])
        let before = ChatTranscriptSnapshot(subagentThreads: store.subagentThreads)
        _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation, coreInstanceID: core,
                              sequence: 2, items: [], bodyAppends: [], subagentThreads: [.init(runID: run, live: 0)]))
        #expect(store.subagentThreads.isEmpty)
        #expect(ChatTranscriptSnapshot(subagentThreads: store.subagentThreads) != before,
                "a counts-only delta must reach the renderer")
    }

    /// Positions as Core stores a recorded Claude turn: the launch, parent
    /// text the subagent's calls interrupt, a nested launch, then more text.
    @Test("members leave the position stream for their thread, nested threads included")
    func groupsMembers() throws {
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [
            launch("outer", position: 1, task: "Find the definition", name: "general-purpose", state: .running, async: true),
            text("before", position: 2),
            member("grep", position: 3, of: "outer"),
            launch("nested", position: 4, task: "Look closer", name: "Explore", state: .completed, async: false, in: "outer"),
            member("read", position: 5, of: "nested"),
            member("ls", position: 6, of: "outer"),
            text("after", position: 7),
        ]))
        #expect(entries.map(\.id) == ["outer/group", "before", "after"])
        let outer = try thread(entries[0])
        #expect(outer.task == "Find the definition")
        #expect(outer.name == "general-purpose")
        #expect(outer.state == .running)
        #expect(!outer.hasFinalAnswer, "an asynchronous launch returns only a receipt")
        #expect(outer.members.map(\.id) == ["grep", "nested", "ls"])
        let nested = try #require(outer.members[1].thread)
        #expect(nested.members.map(\.id) == ["read"])
        #expect(nested.hasFinalAnswer)
        #expect(outer.latestMember?.id == "ls")
    }

    @Test("rows whose launch is not loaded gather where the first of them is")
    func orphanedMembers() throws {
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [
            member("grep", position: 3, of: "missing"),
            text("between", position: 4),
            member("ls", position: 5, of: "missing"),
        ]))
        #expect(entries.map(\.id) == ["grep/group", "between"])
        let orphan = try thread(entries[0])
        #expect(orphan.state == nil)
        #expect(orphan.members.map(\.id) == ["grep", "ls"])
    }

    @Test("a cycle in stored membership cannot recurse forever")
    func cycleGuard() throws {
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [
            launch("a", position: 1, task: "A", name: nil, state: .running, async: true, in: "b"),
            launch("b", position: 2, task: "B", name: nil, state: .running, async: true, in: "a"),
        ]))
        // Each launch names the other's thread; both stay visible, and neither
        // nests itself.
        #expect(entries.map(\.id) == ["a/group", "b/group"])
        #expect(try thread(entries[0]).members.map(\.id) == ["b"])
        #expect(try thread(entries[0]).members[0].thread?.members.isEmpty == true)
    }

    @Test("the running trailer counts the run's live subagents")
    func trailerCount() {
        let entries = ChatTranscriptAssembler.plan(.init(
            liveRun: .init(runID: run, conversationID: conversation, status: .running),
            subagentThreads: [run: 2],
            normalizedItems: [launch("outer", position: 1, task: "Find", name: nil, state: .running, async: true)]
        ))
        guard case .card(.turnTrailer(.running(_, _, _, _, _, _, let subagents))) = entries.last?.content else {
            Issue.record("expected a running trailer, got \(entries.map(\.id))"); return
        }
        #expect(subagents == 2)
    }

    @Test("a thread card's accessibility says its state, and clicking it opens the thread")
    func cardState() throws {
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [
            launch("outer", position: 1, task: "Find", name: nil, state: .failed, async: false),
        ]))
        guard case .card(.activityGroup(let activities, let isActive)) = entries[0].content else {
            Issue.record("expected a thread card"); return
        }
        let payload = ActivityGroupCardPayload(activities: activities, isActive: isActive)
        #expect(payload.accessibilityPlaceholder?.identifier == "chat.subagent-thread")
        #expect(payload.accessibilityPlaceholder?.value == "failed")
        var opened: [String] = []
        let card = HostedActivityGroupCard.activityGroup(
            activities, isActive: isActive, theme: .ottam(.builtIn),
            environment: HostedCardEnvironment(typography: .builtIn),
            openThread: { opened.append($0) }
        )
        card.actions.onOpenThread("subagent:outer")
        #expect(opened == ["subagent:outer"])
    }

    /// A native subagent's text is its own: the main transcript shows it only
    /// as the card's newest line, and the trailer quotes the parent.
    @Test("a subagent's text joins its thread, not the parent's turn")
    func threadedText() throws {
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [
            launch("outer", position: 1, task: "Read", name: "Reviewer", state: .running, async: true),
            member("grep", position: 2, of: "outer"),
            threaded(text("Looking at the README\nIt says hello", position: 3), in: "outer"),
            text("parent", position: 4),
        ]))
        #expect(entries.map(\.id) == ["outer/group", "parent"])
        let outer = try thread(entries[0])
        #expect(outer.members.map(\.id) == ["grep", "Looking at the README\nIt says hello"])
        #expect(outer.latestMember?.label == "It says hello")
    }

    @Test("a thread's scope renders its rows as a run's, nested threads as cards")
    func scopedPlan() throws {
        let items = [
            launch("outer", position: 1, task: "Find", name: "general-purpose", state: .running, async: true),
            member("grep", position: 3, of: "outer"),
            launch("nested", position: 4, task: "Look closer", name: "Explore", state: .completed, async: false, in: "outer"),
            member("read", position: 5, of: "nested"),
            threaded(text("found it", position: 6), in: "outer"),
        ]
        let entries = ChatTranscriptAssembler.plan(.init(
            conversationID: conversation, subagentThreadScope: "subagent:outer", normalizedItems: items
        ))
        // The task opens the tab, as the user's message opens a turn.
        #expect(entries.map(\.id) == ["outer/prompt", "grep/group", "nested/group", "found it"])
        #expect(bubble(entries[0]) == "Find")
        let nested = try thread(entries[2])
        #expect(nested.members.map(\.id) == ["read"])
    }

    /// A subagent the parent resumed has an anchor per run in one thread; a
    /// message to it while it ran marks the parent's call.
    @Test("the parent's messages to a subagent open its runs in the tab")
    func parentMessages() throws {
        var first = launch("a1", position: 1, task: "Read README", name: "Reader", state: .completed, async: true)
        first.tool?.subagentThread?.source = .nativeSession
        first.tool?.providerToolCallID = "subagent:a1"
        var resumed = first
        resumed.itemID = "a1-2"
        resumed.position = 4
        resumed.tool?.providerToolCallID = "subagent:a1:generation:2"
        resumed.tool?.subagentThread?.task = "Count the words"
        resumed.tool?.subagentThread?.prompt = "Count the words"
        resumed.tool?.subagentThread?.state = .running
        var send = item("send", position: 6, kind: .tool)
        send.tool = .init(kind: "other", title: "SendMessage", arguments: .null, locations: [], partialMetadata: false, payloads: [])
        send.tool?.subagentFollowUp = .init(threadID: "subagent:a1", message: "Also run hostname")
        let items = [
            first, member("read", position: 2, of: "a1"), threaded(text("11 words", position: 3), in: "a1"),
            resumed, member("count", position: 5, of: "a1"), send, member("hostname", position: 7, of: "a1"),
        ]
        let entries = ChatTranscriptAssembler.plan(.init(
            conversationID: conversation, subagentThreadScope: "subagent:a1", normalizedItems: items
        ))
        #expect(entries.map(\.id) == [
            "a1/prompt", "read/group", "11 words", "a1-2/prompt", "count/group", "send/follow-up", "hostname/group",
        ])
        #expect(bubble(entries[3]) == "From the parent: Count the words")
        #expect(bubble(entries[5]) == "From the parent: Also run hostname")

        // The main transcript has a card per run; both open the thread, and
        // the parent's call is its own row.
        let main = ChatTranscriptAssembler.plan(.init(conversationID: conversation, normalizedItems: items))
        let cards = main.compactMap { entry -> ChatSubagentThread? in
            guard case .card(.activityGroup(let activities, _)) = entry.content else { return nil }
            return activities.first?.thread
        }
        #expect(cards.map(\.id) == ["subagent:a1", "subagent:a1"])
        #expect(cards.map(\.state) == [.completed, .running])
        // The headline is the subagent's short name, not the prompt, and
        // nothing repeats it beside it.
        #expect(cards.map(\.task) == ["Reader", "Reader"])
        #expect(cards.allSatisfy { $0.name == nil })
        #expect(main.contains { $0.id == "send/group" })

        // A store scoped to the thread admits the parent's call to it.
        let store = ChatTranscriptStore()
        store.select(conversation, thread: "subagent:a1")
        store.merge(.init(coreInstanceID: core, sequence: 1, items: []), conversationID: conversation)
        _ = store.merge(.init(
            workspaceID: .init("workspace"), conversationID: conversation, coreInstanceID: core, sequence: 2,
            items: items + [text("parent", position: 8)], bodyAppends: []
        ))
        #expect(store.orderedItems.map(\.itemID) == ["a1", "read", "11 words", "a1-2", "count", "send", "hostname"])
    }

    /// Deltas carry the whole conversation; a thread's store keeps its own
    /// rows and its nested threads', once their anchors arrive.
    @Test("a thread's store admits only its family's rows")
    func scopedStore() {
        let store = ChatTranscriptStore()
        store.select(conversation, thread: "subagent:outer")
        store.merge(.init(coreInstanceID: core, sequence: 1, items: []), conversationID: conversation)
        _ = store.merge(.init(
            workspaceID: .init("workspace"), conversationID: conversation, coreInstanceID: core, sequence: 2,
            items: [
                text("parent", position: 1),
                member("grep", position: 2, of: "outer"),
                member("elsewhere", position: 3, of: "sibling"),
                launch("nested", position: 4, task: "Look", name: nil, state: .running, async: true, in: "outer"),
                member("read", position: 5, of: "nested"),
            ],
            bodyAppends: []
        ))
        #expect(store.orderedItems.map(\.itemID) == ["grep", "nested", "read"])
        #expect(store.threadScope == "subagent:outer")
    }

    /// Retargeting the tab while the old thread's page loads: the old scan
    /// stops, and the new thread's own scan loads, even if the old one's
    /// task is cancelled first.
    @Test("retargeting mid-load loads the new thread")
    func retargetDuringResync() async throws {
        let store = ChatTranscriptStore()
        store.select(conversation, thread: "subagent:outer")
        var release: CheckedContinuation<Void, Never>?
        let outer = member("grep", position: 2, of: "outer")
        let old = Task {
            try await store.resync(conversation) { _ in
                await withCheckedContinuation { release = $0 }
                return .init(coreInstanceID: core, sequence: 1, items: [outer])
            }
        }
        while release == nil { await Task.yield() }
        store.select(conversation, thread: "subagent:sibling")
        try await store.resync(conversation) { _ in
            .init(coreInstanceID: core, sequence: 1, items: [member("elsewhere", position: 3, of: "sibling")])
        }
        old.cancel()
        release?.resume()
        try await old.value
        #expect(store.orderedItems.map(\.itemID) == ["elsewhere"])
    }

    /// Core filters a thread's pages against every anchor it holds, so a
    /// nested thread's rows can come a page before its anchor: they stay, and
    /// that thread's later deltas with them. Released, the store keeps nothing.
    @Test("a thread's page keeps nested rows whose anchor is older")
    func pageKeepsNestedRows() {
        let store = ChatTranscriptStore()
        store.select(conversation, thread: "subagent:outer")
        store.merge(.init(coreInstanceID: core, sequence: 1, items: [member("read", position: 5, of: "nested")]),
                    conversationID: conversation)
        _ = store.merge(.init(
            workspaceID: .init("workspace"), conversationID: conversation, coreInstanceID: core, sequence: 2,
            items: [member("grep", position: 6, of: "nested"), member("elsewhere", position: 7, of: "sibling")],
            bodyAppends: []
        ))
        #expect(store.orderedItems.map(\.itemID) == ["read", "grep"])
        store.release()
        #expect(store.threadScope == nil)
        #expect(store.orderedItems.isEmpty)
    }

    /// A subagent's image is one of its thread's rows, not the parent's.
    @Test("a subagent's image renders in its thread")
    func threadedImage() throws {
        var image = member("image", position: 2, of: "outer")
        image.kind = .image
        image.tool = nil
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [
            launch("outer", position: 1, task: "Look", name: nil, state: .running, async: true), image,
        ]))
        #expect(entries.map(\.id) == ["outer/group"])
        #expect(try thread(entries[0]).members.map(\.kind) == ["image"])
    }

    /// Claude's `Agent` call is kind `think`; its result must not open as
    /// reasoning, which shows the call's input.
    @Test("a launch result opens on its output")
    func launchResultOpensOutput() {
        var launch = ChatLiveActivity(id: "outer", label: "Agent", kind: "think")
        launch.payloads = [.init(role: "output", availability: "available", payloadID: "payload", byteCount: 9,
                                 locallyTruncated: false, revision: 1, parts: [])]
        let answer = ChatActivityGroupRow.launchResult(launch, failed: false)
        #expect(answer.label == "Final answer")
        #expect(answer.id == launch.id)
        let store = ChatPayloadDetailStore()
        store.configure([answer], readPayload: nil, readBody: nil)
        store.toggle(answer)
        #expect(store.section == "output")
        #expect(ChatActivityGroupRow.launchResult(launch, failed: true).label == "Launch error")
    }

    private func thread(_ entry: TranscriptItem) throws -> ChatSubagentThread {
        guard case .card(.activityGroup(let activities, _)) = entry.content, activities.count == 1 else {
            throw ThreadMissing(id: entry.id)
        }
        return try #require(activities[0].thread)
    }

    /// Claude resumes a subagent itself when its background task finishes;
    /// its notification is not a message from the parent.
    @Test("a resume by a background task's end is not the parent's message")
    func taskNotificationResume() {
        var first = launch("a1", position: 1, task: "Wait", name: "Waiter", state: .completed, async: true)
        first.tool?.subagentThread?.source = .nativeSession
        first.tool?.providerToolCallID = "subagent:a1"
        var resumed = first
        resumed.itemID = "a1-2"
        resumed.position = 2
        resumed.tool?.providerToolCallID = "subagent:a1:generation:2"
        resumed.tool?.subagentThread?.prompt = "<task-notification>\n<task-id>b6</task-id>\n</task-notification>"
        let entries = ChatTranscriptAssembler.plan(.init(
            conversationID: conversation, subagentThreadScope: "subagent:a1", normalizedItems: [first, resumed]
        ))
        #expect(entries.map(\.id) == ["a1/prompt", "a1-2/prompt", "a1-2/uncaptured"])
        #expect(bubble(entries[1]) == nil)
        guard case .markdown(let note, _, nil) = entries[1].content else {
            Issue.record("not a note: \(entries[1].content)")
            return
        }
        #expect(note.contains("background task"))
    }

    /// Claude runs a slash command's subagent with no launching call, and the
    /// adapter reports its start and end but none of its work.
    @Test("a finished subagent with no rows says nothing was captured")
    func uncapturedThread() {
        func scoped(_ items: [ConversationTranscriptItem]) -> [String] {
            ChatTranscriptAssembler.plan(.init(
                conversationID: conversation, subagentThreadScope: "subagent:a1", normalizedItems: items
            )).map(\.id)
        }
        let finished = launch("a1", position: 1, task: "Review", name: "/code-review", state: .completed, async: true)
        #expect(scoped([finished]) == ["a1/prompt", "a1/uncaptured"])
        // Rows of its own, or more time to send them, and there is nothing to say.
        #expect(scoped([finished, member("read", position: 2, of: "a1")]) == ["a1/prompt", "read/group"])
        let running = launch("a1", position: 1, task: "Review", name: "/code-review", state: .running, async: true)
        #expect(scoped([running]) == ["a1/prompt"])
        // The main transcript draws the thread as its card, with no note.
        #expect(ChatTranscriptAssembler.plan(.init(conversationID: conversation, normalizedItems: [finished]))
            .map(\.id) == ["a1/group"])
    }

    private struct ThreadMissing: Error { let id: String }

    private func bubble(_ entry: TranscriptItem) -> String? {
        guard case .markdown(let text, _, .bubble) = entry.content else { return nil }
        return text
    }

    private func item(_ id: String, position: UInt64, kind: TranscriptItemKind) -> ConversationTranscriptItem {
        ConversationTranscriptItem(
            itemID: id, conversationID: conversation, runID: run,
            position: position, revision: 1, kind: kind, completion: .complete,
            createdAt: "2026-10-09T10:00:00Z", updatedAt: "2026-10-09T10:00:00Z",
            bodyPreview: kind == .assistantText ? id : "", bodyByteCount: kind == .assistantText ? UInt64(id.utf8.count) : 0
        )
    }

    private func text(_ id: String, position: UInt64) -> ConversationTranscriptItem {
        item(id, position: position, kind: .assistantText)
    }

    private func threaded(_ row: ConversationTranscriptItem, in control: String) -> ConversationTranscriptItem {
        var row = row
        row.subagentThreadID = "subagent:" + control
        return row
    }

    private func member(_ id: String, position: UInt64, of control: String) -> ConversationTranscriptItem {
        var row = item(id, position: position, kind: .tool)
        row.subagentThreadID = "subagent:" + control
        row.tool = .init(kind: "search", title: id, arguments: .null, locations: [], partialMetadata: false, payloads: [])
        return row
    }

    private func launch(
        _ id: String, position: UInt64, task: String, name: String?, state: SubagentThreadState,
        async: Bool, in parent: String? = nil
    ) -> ConversationTranscriptItem {
        var row = item(id, position: position, kind: .tool)
        row.subagentThreadID = parent.map { "subagent:" + $0 }
        row.tool = .init(kind: "think", title: "Agent", arguments: .null, locations: [], partialMetadata: false, payloads: [])
        row.tool?.subagentThread = .init(
            id: "subagent:" + id, parentThreadID: row.subagentThreadID, name: name, task: task,
            state: state, source: .legacyParentId, asynchronous: async
        )
        return row
    }
}
