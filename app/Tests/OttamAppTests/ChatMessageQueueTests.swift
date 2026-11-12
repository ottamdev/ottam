import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Queued messages")
struct ChatMessageQueueTests {
    private static let conversation = ConversationID("conv")
    private static let other = ConversationID("other")
    private static let workspace = WorkspaceID("ws")

    private func message(_ body: String) -> QueuedMessage {
        QueuedMessage(body: body, engineID: EngineID("claude"))
    }

    private func queue(_ bodies: [String], in conversationID: ConversationID = conversation) -> ChatMessageQueue {
        var queue = ChatMessageQueue()
        for body in bodies {
            queue.append(message(body), to: conversationID)
        }
        return queue
    }

    private func defaults(_ function: String = #function) -> UserDefaults {
        let suite = "queue-tests.\(function).\(UUID().uuidString)"
        return UserDefaults(suiteName: suite) ?? .standard
    }

    @Test("the queue delivers in the order it was typed")
    func drainsInOrder() {
        var queue = queue(["first", "second", "third"])
        #expect(queue[Self.conversation].map(\.body) == ["first", "second", "third"])
        #expect(queue.takeHead(from: Self.conversation)?.body == "first")
        #expect(queue.takeHead(from: Self.conversation)?.body == "second")
        #expect(queue.takeHead(from: Self.conversation)?.body == "third")
        #expect(queue.takeHead(from: Self.conversation) == nil)
    }

    @Test("removing one message leaves the rest in order")
    func removeKeepsOrder() {
        var queue = queue(["first", "second", "third"])
        let middle = queue[Self.conversation][1]
        #expect(queue.take(middle.id, from: Self.conversation)?.body == "second")
        #expect(queue[Self.conversation].map(\.body) == ["first", "third"])
    }

    /// Send Now is the reason there is no reorder affordance: it promotes one
    /// message past the rest without disturbing the queue behind it.
    @Test("Send Now takes a message out of turn")
    func sendNowTakesOutOfOrder() {
        var queue = queue(["first", "second"])
        let second = queue[Self.conversation][1]
        #expect(queue.take(second.id, from: Self.conversation)?.body == "second")
        #expect(queue.takeHead(from: Self.conversation)?.body == "first")
    }

    /// A send that never reached Core must not lose the message, and must not
    /// silently reorder the queue by putting it back at the end.
    @Test("a message that failed to send goes back to the head")
    func restoreGoesToTheHead() {
        var queue = queue(["first", "second"])
        let head = queue.takeHead(from: Self.conversation)
        #expect(head?.body == "first")
        queue.restore(head!, toHeadOf: Self.conversation)
        #expect(queue[Self.conversation].map(\.body) == ["first", "second"])
    }

    @Test("conversations do not share a queue")
    func conversationsAreIndependent() {
        var queue = queue(["mine"])
        queue.append(message("theirs"), to: Self.other)
        #expect(queue[Self.conversation].map(\.body) == ["mine"])
        #expect(queue[Self.other].map(\.body) == ["theirs"])
        _ = queue.takeHead(from: Self.other)
        #expect(queue[Self.conversation].map(\.body) == ["mine"])
    }

    /// An emptied conversation is dropped rather than kept as an empty array,
    /// so equality and the persisted form both stay canonical.
    @Test("a drained conversation leaves no trace")
    func drainedConversationIsDropped() {
        var queue = queue(["only"])
        _ = queue.takeHead(from: Self.conversation)
        #expect(queue.isEmpty)
        #expect(queue == ChatMessageQueue())
    }

    @Test("a queue survives a quit")
    func persistenceRoundTrips() {
        let store = defaults()
        var original = queue(["first", "second"])
        original.append(message("elsewhere"), to: Self.other)
        original.persist(to: store, workspaceID: Self.workspace)
        #expect(ChatMessageQueue.restored(from: store, workspaceID: Self.workspace) == original)
    }

    /// Emptying the queue has to clear the stored copy too, or the next launch
    /// restores messages the reader already sent or removed.
    @Test("an emptied queue clears what was stored")
    func emptyQueueClearsStorage() {
        let store = defaults()
        queue(["only"]).persist(to: store, workspaceID: Self.workspace)
        ChatMessageQueue().persist(to: store, workspaceID: Self.workspace)
        #expect(ChatMessageQueue.restored(from: store, workspaceID: Self.workspace).isEmpty)
    }

    /// Each window writes its whole key, so the key has to be the workspace's.
    /// Otherwise one window draining its last message clears every other
    /// window's queue along with its own.
    @Test("one workspace's queue does not disturb another's")
    func queuesAreScopedPerWorkspace() {
        let store = defaults()
        let otherWorkspace = WorkspaceID("ws-other")
        let kept = queue(["kept"])
        kept.persist(to: store, workspaceID: otherWorkspace)
        queue(["drained"]).persist(to: store, workspaceID: Self.workspace)
        ChatMessageQueue().persist(to: store, workspaceID: Self.workspace)
        #expect(ChatMessageQueue.restored(from: store, workspaceID: Self.workspace).isEmpty)
        #expect(ChatMessageQueue.restored(from: store, workspaceID: otherWorkspace) == kept)
    }

    /// A queue is a convenience. Refusing to launch over one is a worse answer
    /// than starting without it.
    @Test("an unreadable store yields an empty queue")
    func unreadableStoreIsEmpty() {
        let store = defaults()
        store.set(Data("not json".utf8), forKey: ChatMessageQueue.storageKey(for: Self.workspace))
        #expect(ChatMessageQueue.restored(from: store, workspaceID: Self.workspace).isEmpty)
    }

    /// The four routing fields travel together or not at all. Reading the
    /// engine from the live run and the model from the picker pairs one
    /// engine's model with another's, and Core rejects that as unavailable —
    /// which the drain answers by restoring the message to the *head*, so every
    /// later drain re-fails on it and only Remove clears the queue.
    @Test("routing keeps a model with the engine it was chosen for")
    func routingTravelsTogether() {
        let routing = SubmissionRouting(
            engine: TranscriptFixture.engine(),
            modelID: "claude-opus",
            modeID: "plan",
            thoughtLevelID: "high"
        )
        let queued = QueuedMessage(body: "later", routing: routing)
        #expect(queued.engineID == routing.engine.engineID)
        #expect(queued.modelID == "claude-opus")
        #expect(queued.modeID == "plan")
        #expect(queued.thoughtLevelID == "high")
    }

    @Test("the queued message keeps the settings that were on screen")
    func capturesSessionOptions() {
        let queued = QueuedMessage(
            body: "later",
            attachmentIDs: [ConversationAttachmentID("att-1")],
            engineID: EngineID("codex"),
            modelID: "gpt-5",
            modeID: "manual",
            thoughtLevelID: "medium"
        )
        var queue = ChatMessageQueue()
        queue.append(queued, to: Self.conversation)
        let restored = queue.takeHead(from: Self.conversation)
        #expect(restored?.engineID == EngineID("codex"))
        #expect(restored?.modelID == "gpt-5")
        #expect(restored?.modeID == "manual")
        #expect(restored?.thoughtLevelID == "medium")
        #expect(restored?.attachmentIDs == [ConversationAttachmentID("att-1")])
    }

    /// Stop holds the queue. Only a turn the agent finished on its own releases
    /// the next message.
    @Test(
        "only a completed turn drains the queue",
        arguments: [
            (ConversationRunStatus.completed, true),
            (.canceled, false),
            (.failed, false),
            (.interrupted, false),
            (.queued, false),
            (.running, false)
        ] as [(ConversationRunStatus, Bool)]
    )
    func onlyCompletedDrains(status: ConversationRunStatus, drains: Bool) {
        #expect(ChatMessageQueue.drains(after: status) == drains)
    }
}
