import OttamBackendClient
import OttamPresentationLogic
import Foundation
import Testing
@testable import OttamApp

@Suite("Background tasks")
@MainActor
struct BackgroundTaskTests {
    private let conversation = TranscriptFixture.conversationID
    private let core = CoreInstanceID("core")

    private func delta(_ sequence: UInt64, _ counts: [ConversationRunBackgroundTasks]) -> ConversationTranscriptDeltaParams {
        .init(workspaceID: .init("workspace"), conversationID: conversation, coreInstanceID: core,
              sequence: sequence, items: [], bodyAppends: [], backgroundTasks: counts)
    }

    @Test("a page replaces the live counts, a delta revises them, and a new core clears them")
    func storeCounts() {
        let store = ChatTranscriptStore()
        let first = ConversationRunID("run-1")
        let second = ConversationRunID("run-2")
        store.merge(.init(coreInstanceID: core, sequence: 1, items: [],
                          backgroundTasks: [.init(runID: first, live: 2)]), conversationID: conversation)
        #expect(store.backgroundTasks == [first: 2])

        // A delta names only the runs whose count changed, zero included.
        _ = store.merge(delta(2, [.init(runID: second, live: 1)]))
        #expect(store.backgroundTasks == [first: 2, second: 1])
        let before = ChatTranscriptSnapshot(backgroundTasks: store.backgroundTasks)
        _ = store.merge(delta(3, [.init(runID: first, live: 0)]))
        #expect(store.backgroundTasks == [second: 1])
        #expect(ChatTranscriptSnapshot(backgroundTasks: store.backgroundTasks) != before,
                "a counts-only delta must reach the renderer")

        // A stale delta is ignored; a page omits runs with none live.
        _ = store.merge(delta(2, [.init(runID: first, live: 5)]))
        #expect(store.backgroundTasks == [second: 1])
        store.merge(.init(coreInstanceID: core, sequence: 4, items: []), conversationID: conversation)
        #expect(store.backgroundTasks.isEmpty)

        _ = store.merge(delta(5, [.init(runID: first, live: 1)]))
        store.merge(.init(coreInstanceID: .init("new-core"), sequence: 1, items: []), conversationID: conversation)
        #expect(store.backgroundTasks.isEmpty)
    }

    @Test("a backgrounded tool gets its own row, carrying its run and task")
    func ownRow() throws {
        let run = ConversationRunID("run-1")
        let read = tool("read", position: 1, kind: "read", title: "Read file '/tmp/a.md'")
        var shell = tool("shell", position: 2, kind: "execute", title: "sleep 30; echo done")
        shell.tool?.backgroundTask = .init(id: "task-1", state: .running)
        let after = tool("after", position: 3, kind: "read", title: "Read file '/tmp/b.md'")
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [read, shell, after]))
        #expect(entries.map(\.id) == ["read/group", "shell/group", "after/group"])
        guard case .card(.activityGroup(let activities, let isActive)) = entries[1].content else {
            Issue.record("expected the shell's own group, got \(entries.map(\.content))"); return
        }
        #expect(!isActive)
        let activity = try #require(activities.first)
        #expect(activities.count == 1)
        #expect(activity.runID == run)
        #expect(activity.backgroundTask?.id == "task-1")
        #expect(activity.canStopBackgroundTask)

        shell.tool?.backgroundTask = .init(id: "task-1", state: .stopped)
        guard case .card(.activityGroup(let ended, _)) = ChatTranscriptAssembler.plan(
            .init(normalizedItems: [shell])
        )[0].content else { Issue.record("expected the shell's group"); return }
        #expect(!ended[0].canStopBackgroundTask)
        // A stamp with no lifecycle frame yet names no task to stop.
        shell.tool?.backgroundTask = .init(state: .running)
        guard case .card(.activityGroup(let unnamed, _)) = ChatTranscriptAssembler.plan(
            .init(normalizedItems: [shell])
        )[0].content else { Issue.record("expected the shell's group"); return }
        #expect(!unnamed[0].canStopBackgroundTask)
    }

    @Test("every task state has a label")
    func stateLabels() {
        #expect(ChatActivityGroupRow.backgroundStateLabel(.running) == "Running in background")
        #expect(ChatActivityGroupRow.backgroundStateLabel(.stopped) == "Stopped")
        #expect(ChatActivityGroupRow.backgroundStateLabel(.lost) == "Lost")
        #expect(ChatActivityGroupRow.backgroundStateLabel(.completed) == "Completed")
    }

    @Test("the trailer counts the run's live tasks, live or ended")
    func trailerCounts() {
        let run = ConversationRunID("run-1")
        var boundary = tool("boundary", position: 2, kind: nil, title: nil)
        boundary.kind = .runBoundary
        boundary.tool = nil
        boundary.runStatus = .completed
        let ended = ChatTranscriptAssembler.plan(.init(
            backgroundTasks: [run: 1],
            normalizedItems: [tool("shell", position: 1, kind: "execute", title: "sleep"), boundary]
        ))
        guard case .card(.turnTrailer(.persisted(_, _, _, _, let count, _, _))) = ended.last?.content else {
            Issue.record("expected a persisted trailer, got \(ended.map(\.id))"); return
        }
        #expect(count == 1)

        let live = ChatTranscriptAssembler.plan(.init(
            liveRun: .init(runID: run, conversationID: conversation, status: .running),
            backgroundTasks: [run: 2],
            normalizedItems: [tool("shell", position: 1, kind: "execute", title: "sleep")]
        ))
        guard case .card(.turnTrailer(.running(_, _, _, _, _, let liveCount, _))) = live.last?.content else {
            Issue.record("expected a running trailer, got \(live.map(\.id))"); return
        }
        #expect(liveCount == 2)
    }

    @Test("a run the agent started on its own opens with a divider; the user's do not")
    func continuedDivider() {
        let user = ConversationRunID("run-1")
        let followUp = ConversationRunID("run-2")
        var reply = tool("reply", position: 3, kind: nil, title: nil)
        reply.kind = .assistantText
        reply.tool = nil
        reply.runID = followUp
        reply.bodyPreview = "The server is ready."
        var more = reply
        more.itemID = "more"
        more.position = 4
        let entries = ChatTranscriptAssembler.plan(.init(
            autonomousRunIDs: [followUp],
            normalizedItems: [tool("shell", position: 1, kind: "execute", title: "sleep"), reply, more]
        ))
        #expect(entries.map(\.id) == ["shell/group", "run/run-2/continued", "reply", "more"])
        #expect(entries[1].cardSpec == .continuedAutomatically(followUp))
        #expect(!entries.contains { $0.id == "run/\(user.rawValue)/continued" })
    }

    @Test("an MCP server's Sign in card follows everything else in the chat")
    func mcpSignInCard() {
        var reply = tool("reply", position: 3, kind: nil, title: nil)
        reply.kind = .assistantText
        reply.tool = nil
        reply.bodyPreview = "The linear server needs a sign-in."
        let signIn = ChatMcpSignIn(serverID: McpServerID("mcp_linear"), serverName: "linear", phase: nil)
        let entries = ChatTranscriptAssembler.plan(.init(
            normalizedItems: [tool("shell", position: 1, kind: "execute", title: "call linear"), reply],
            mcpSignIns: [signIn]
        ))
        #expect(entries.last?.id == "mcp-sign-in/mcp_linear")
        #expect(entries.last?.cardSpec == .mcpSignIn(signIn))
    }

    @Test("a follow-up that is taken over keeps its trailer under its own output")
    func takenOverTrailer() {
        let followUp = ConversationRunID("run-2")
        let takeover = ConversationRunID("run-3")
        func text(_ id: String, _ position: UInt64, _ run: ConversationRunID) -> ConversationTranscriptItem {
            var item = tool(id, position: position, kind: nil, title: nil)
            item.kind = .assistantText
            item.tool = nil
            item.runID = run
            item.runStatus = .running
            item.bodyPreview = id
            item.bodyByteCount = UInt64(id.utf8.count)
            return item
        }
        func boundary(_ id: String, _ position: UInt64, _ run: ConversationRunID) -> ConversationTranscriptItem {
            var item = tool(id, position: position, kind: nil, title: nil)
            item.kind = .runBoundary
            item.tool = nil
            item.runID = run
            item.runStatus = .completed
            item.updatedAt = "2026-10-08T10:00:09Z"
            return item
        }
        // The takeover's reply sits between the follow-up's output and its
        // boundary, as Core recorded a real takeover.
        let entries = ChatTranscriptAssembler.plan(.init(
            autonomousRunIDs: [followUp],
            normalizedItems: [text("stopped", 48, followUp), text("takeover", 49, takeover),
                              boundary("end-2", 50, followUp), boundary("end-3", 51, takeover)]
        ))
        #expect(entries.map(\.id) == ["run/run-2/continued", "stopped", "run/run-2/trailer",
                                      "takeover", "run/run-3/trailer"])
        guard case .card(.turnTrailer(.persisted(_, let body, let endedAt, _, _, _, _))) = entries[2].content else {
            Issue.record("expected the follow-up's trailer, got \(entries[2].content)"); return
        }
        #expect(body == "stopped")
        #expect(endedAt == "2026-10-08T10:00:09Z")
    }

    private func tool(_ id: String, position: UInt64, kind: String?, title: String?) -> ConversationTranscriptItem {
        var item = ConversationTranscriptItem(
            itemID: id, conversationID: conversation, runID: .init("run-1"),
            position: position, revision: 1, kind: .tool, completion: .complete,
            createdAt: "2026-10-08T10:00:00Z", updatedAt: "2026-10-08T10:00:00Z",
            bodyPreview: "", bodyByteCount: 0
        )
        item.tool = .init(kind: kind, title: title, arguments: .null, locations: [], partialMetadata: false, payloads: [])
        return item
    }
}
