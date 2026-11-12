import OttamBackendClient
import Foundation

/// Where a Message is about to run: an Engine and the session options that go
/// with *that* Engine.
///
/// A value rather than four reads, because the options are only meaningful
/// paired with the Engine they were chosen for. A model id from one Engine
/// handed to another is rejected by Core as unavailable.
struct SubmissionRouting: Equatable {
    let engine: EngineSummary
    let modelID: String?
    let modeID: String?
    let thoughtLevelID: String?
}

/// One message waiting for the running turn to finish.
///
/// The engine and session options are captured here rather than read when the
/// message drains: the reader may have navigated to another conversation or
/// moved the picker since queueing, and the message should run with the
/// settings that were on screen when they sent it.
///
/// `attachmentIDs` outlive a relaunch. A draft attachment is a legal unclaimed
/// row in Core, swept only 24 hours after it was pasted and only when a
/// Workspace opens (`server/chat/attachments.rs`), so a queue that comes back
/// the same day still carries its images. Past that the claim fails, the send
/// fails with it, and the message stays in the pill with Remove — which is the
/// intended outcome: better a message that visibly will not send than one that
/// silently sends without the image it was written about.
struct QueuedMessage: Equatable, Identifiable, Codable, Sendable {
    let id: UUID
    let body: String
    let attachmentIDs: [ConversationAttachmentID]
    let engineID: EngineID
    let modelID: String?
    let modeID: String?
    let thoughtLevelID: String?

    init(
        id: UUID = UUID(),
        body: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        engineID: EngineID,
        modelID: String? = nil,
        modeID: String? = nil,
        thoughtLevelID: String? = nil
    ) {
        self.id = id
        self.body = body
        self.attachmentIDs = attachmentIDs
        self.engineID = engineID
        self.modelID = modelID
        self.modeID = modeID
        self.thoughtLevelID = thoughtLevelID
    }

    init(
        id: UUID = UUID(),
        body: String,
        attachmentIDs: [ConversationAttachmentID] = [],
        routing: SubmissionRouting
    ) {
        self.init(
            id: id,
            body: body,
            attachmentIDs: attachmentIDs,
            engineID: routing.engine.engineID,
            modelID: routing.modelID,
            modeID: routing.modeID,
            thoughtLevelID: routing.thoughtLevelID
        )
    }
}

/// Messages the reader sent while a run was working, held per conversation
/// until that run completes.
///
/// Deliberately client state. Core has no queued delivery mode and should not
/// grow one: a Run Input cannot exist without a Message row, so a Core-owned
/// queue would have to publish the Message before it is sent. That both puts a
/// bubble in the transcript ahead of the turn that answers it and parks the
/// live run off the transcript tail for the whole turn, which is the
/// configuration the incremental streaming grant and follow-bottom are not
/// built for. Holding the queue here keeps the live run last.
struct ChatMessageQueue: Equatable {
    private var byConversation: [ConversationID: [QueuedMessage]] = [:]

    init(_ byConversation: [ConversationID: [QueuedMessage]] = [:]) {
        self.byConversation = byConversation.filter { !$0.value.isEmpty }
    }

    subscript(conversationID: ConversationID) -> [QueuedMessage] {
        byConversation[conversationID] ?? []
    }

    var isEmpty: Bool { byConversation.isEmpty }

    mutating func append(_ message: QueuedMessage, to conversationID: ConversationID) {
        byConversation[conversationID, default: []].append(message)
    }

    /// Removes one message wherever it sits, leaving the rest in order.
    /// Returns it so the caller can send what it just took.
    @discardableResult
    mutating func take(_ id: QueuedMessage.ID, from conversationID: ConversationID) -> QueuedMessage? {
        guard var messages = byConversation[conversationID],
              let index = messages.firstIndex(where: { $0.id == id })
        else { return nil }
        let message = messages.remove(at: index)
        set(messages, for: conversationID)
        return message
    }

    /// The next message to send. Only the head drains, so a queue always
    /// delivers in the order it was typed.
    mutating func takeHead(from conversationID: ConversationID) -> QueuedMessage? {
        guard var messages = byConversation[conversationID], !messages.isEmpty else { return nil }
        let message = messages.removeFirst()
        set(messages, for: conversationID)
        return message
    }

    /// Puts a message back where it was, for a send that never reached Core.
    mutating func restore(_ message: QueuedMessage, toHeadOf conversationID: ConversationID) {
        byConversation[conversationID, default: []].insert(message, at: 0)
    }

    /// An empty conversation is removed rather than kept as an empty array, so
    /// equality and the persisted form both stay canonical.
    private mutating func set(_ messages: [QueuedMessage], for conversationID: ConversationID) {
        byConversation[conversationID] = messages.isEmpty ? nil : messages
    }
}

extension ChatMessageQueue {
    /// Whether a run reaching `status` should send the next queued message.
    ///
    /// Only a turn the agent finished on its own. Stopping is how the reader
    /// takes the wheel back and must not immediately fire the next message; a
    /// failed run leaves a conversation that needs looking at before more is
    /// added to it; and `interrupted` means a steer's follow-up run is already
    /// starting, so the conversation is not idle at all.
    ///
    /// Exhaustive on purpose: a new run status has to make this decision rather
    /// than inherit one.
    static func drains(after status: ConversationRunStatus) -> Bool {
        switch status {
        case .completed: true
        case .queued, .running, .canceled, .failed, .interrupted: false
        }
    }
}

extension ChatMessageQueue {
    /// Keyed per workspace, because a window only ever holds the queues for the
    /// workspace it is showing. A write replaces its whole key, so two windows
    /// on different workspaces cannot clobber each other; two on the *same*
    /// workspace still last-write-wins, which is the same rule their in-memory
    /// queues already follow — neither sees the other's Return.
    static func storageKey(for workspaceID: WorkspaceID) -> String {
        "chat.queuedMessages.\(workspaceID.rawValue)"
    }

    /// Restored on launch so pressing Return is not quietly undone by a quit or
    /// a crash. Anything that fails to decode is dropped: a queue is a
    /// convenience, and refusing to start is a worse answer than losing it.
    static func restored(from defaults: UserDefaults, workspaceID: WorkspaceID) -> ChatMessageQueue {
        guard let data = defaults.data(forKey: storageKey(for: workspaceID)),
              let stored = try? JSONDecoder().decode([String: [QueuedMessage]].self, from: data)
        else { return ChatMessageQueue() }
        return ChatMessageQueue(
            Dictionary(uniqueKeysWithValues: stored.map { (ConversationID($0.key), $0.value) })
        )
    }

    func persist(to defaults: UserDefaults, workspaceID: WorkspaceID) {
        let key = Self.storageKey(for: workspaceID)
        guard !isEmpty else {
            defaults.removeObject(forKey: key)
            return
        }
        let stored = Dictionary(
            uniqueKeysWithValues: byConversation.map { ($0.key.rawValue, $0.value) }
        )
        guard let data = try? JSONEncoder().encode(stored) else { return }
        defaults.set(data, forKey: key)
    }
}
