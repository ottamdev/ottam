import OttamBackendClient
import Foundation

/// One recently opened conversation, kept whole so returning to it is
/// instant: its transcript store (still merging deltas in the background),
/// the detail last shown with it, and its rendered view. The cache evicts it
/// as one unit.
@MainActor
final class ConversationTranscriptSession {
    let conversationID: ConversationID
    let store: ChatTranscriptStore
    var detail: ConversationDetail?
    /// A gap or recovery notice arrived while out of view: resync on return.
    var needsResync = false
    /// When the conversation was last left or its run last ended, whichever
    /// is later; nil while it is shown.
    var idleSince: Date?
    var viewSession: TranscriptViewSession?
    fileprivate var lastUse: UInt64 = 0

    init(conversationID: ConversationID, store: ChatTranscriptStore? = nil) {
        let store = store ?? ChatTranscriptStore()
        self.conversationID = conversationID
        self.store = store
        store.select(conversationID)
    }
}

/// What the reader left open and where they were in one conversation. Held
/// apart from its session so it outlives eviction and a Core restart.
@MainActor
final class ChatConversationUIState {
    /// Members of the activity groups left open. A group's ID follows its
    /// first member, which paging back can change; any member names it.
    private(set) var expandedActivityIDs: Set<String> = []
    var readingPlace: TranscriptReadingPlace?

    /// Nothing worth keeping: no group open, and the place is the default.
    var isEmpty: Bool { expandedActivityIDs.isEmpty && (readingPlace ?? .bottom) == .bottom }

    func isExpanded(_ activityIDs: [String]) -> Bool {
        activityIDs.contains(where: expandedActivityIDs.contains)
    }

    func setExpanded(_ expanded: Bool, activityIDs: [String]) {
        if expanded { expandedActivityIDs.formUnion(activityIDs) }
        else { expandedActivityIDs.subtract(activityIDs) }
    }
}

/// The recently opened conversations, least recently used first out. A
/// shown conversation and one with a run in progress are never evicted.
@MainActor
final class ConversationTranscriptCache {
    struct Policy: Equatable {
        /// A count, not bytes: a session's memory is mostly what was built
        /// from its text, about 10 MB for a 250 KB conversation. Memory
        /// pressure is the guard for heavier ones.
        var capacity = 10
        var idleInterval: TimeInterval = 30 * 60

        static var standard: Policy {
            var policy = Policy()
            #if DEBUG
            // UI tests reach the evicted return by holding one conversation.
            if let raw = ProcessInfo.processInfo.environment["OTTAM_DEBUG_CONVERSATION_CACHE"],
               let capacity = Int(raw), capacity >= 1 {
                policy.capacity = capacity
            }
            #endif
            return policy
        }
    }

    let policy: Policy
    private let now: () -> Date
    private var sessions: [ConversationID: ConversationTranscriptSession] = [:]
    private var uiStates: [ConversationID: ChatConversationUIState] = [:]
    private var useCounter: UInt64 = 0

    init(policy: Policy = .standard, now: @escaping () -> Date = Date.init) {
        self.policy = policy
        self.now = now
    }

    var cachedIDs: Set<ConversationID> { Set(sessions.keys) }

    func session(for conversationID: ConversationID) -> ConversationTranscriptSession? {
        sessions[conversationID]
    }

    /// The conversation's session, made current. A new one takes `store`
    /// when given (the draft store a first message bound to it).
    func open(_ conversationID: ConversationID, adopting store: ChatTranscriptStore? = nil) -> ConversationTranscriptSession {
        let session = sessions[conversationID]
            ?? ConversationTranscriptSession(conversationID: conversationID, store: store)
        sessions[conversationID] = session
        useCounter &+= 1
        session.lastUse = useCounter
        session.idleSince = nil
        return session
    }

    /// Starts a conversation's idle clock: when it is left, and again when a
    /// run ends out of view.
    func markIdle(_ conversationID: ConversationID) {
        sessions[conversationID]?.idleSince = now()
    }

    func markStale(except conversationID: ConversationID?) {
        for session in sessions.values where session.conversationID != conversationID {
            session.needsResync = true
        }
    }

    func uiState(for conversationID: ConversationID) -> ChatConversationUIState {
        if let state = uiStates[conversationID] { return state }
        let state = ChatConversationUIState()
        uiStates[conversationID] = state
        return state
    }

    /// Drops idle sessions, then the least recently used until the count
    /// fits, then what readers left in uncached conversations when it holds
    /// nothing. Returns the evicted conversations.
    @discardableResult
    func evict(shown: ConversationID?, running: Set<ConversationID>) -> [ConversationID] {
        func evictable(_ session: ConversationTranscriptSession) -> Bool {
            session.conversationID != shown && !running.contains(session.conversationID)
        }
        let time = now()
        var evicted = sessions.values.filter { session in
            evictable(session) && session.idleSince.map { time.timeIntervalSince($0) >= policy.idleInterval } == true
        }.map(\.conversationID)
        evicted.forEach(drop)
        var candidates = sessions.values.filter(evictable).sorted { $0.lastUse < $1.lastUse }
        while sessions.count > policy.capacity, !candidates.isEmpty {
            let oldest = candidates.removeFirst()
            drop(oldest.conversationID)
            evicted.append(oldest.conversationID)
        }
        // A cached conversation's state may be the one its view writes into.
        uiStates = uiStates.filter { sessions[$0.key] != nil || !$0.value.isEmpty }
        return evicted
    }

    /// Memory pressure: keep only the shown conversation. Running ones go
    /// too; a return reloads them from Core.
    func removeSessions(keeping shown: ConversationID? = nil) {
        for id in sessions.keys where id != shown { drop(id) }
    }

    /// A deleted conversation: its session and what the reader left.
    func forget(_ conversationID: ConversationID) {
        drop(conversationID)
        uiStates[conversationID] = nil
    }

    /// Another workspace: nothing here applies to it.
    func forgetAll() {
        removeSessions()
        uiStates.removeAll()
    }

    private func drop(_ conversationID: ConversationID) {
        sessions.removeValue(forKey: conversationID)?.store.suspendBodies()
    }
}

extension ConversationTranscriptCache: TranscriptViewSessionKeeping {
    func hasViewSession(for conversationID: ConversationID) -> Bool {
        sessions[conversationID]?.viewSession != nil
    }

    func takeViewSession(for conversationID: ConversationID) -> TranscriptViewSession? {
        defer { sessions[conversationID]?.viewSession = nil }
        return sessions[conversationID]?.viewSession
    }

    /// An evicted conversation keeps only its place; its rendering is dropped.
    func keep(_ session: TranscriptViewSession, place: TranscriptReadingPlace?, for conversationID: ConversationID) {
        if let place { uiState(for: conversationID).readingPlace = place }
        guard let cached = sessions[conversationID] else { return }
        session.textArtifactCache.trim(toMostRecent: TranscriptViewSession.retainedArtifactCount)
        cached.viewSession = session
    }

    func readingPlace(for conversationID: ConversationID) -> TranscriptReadingPlace? {
        uiStates[conversationID]?.readingPlace
    }
}
