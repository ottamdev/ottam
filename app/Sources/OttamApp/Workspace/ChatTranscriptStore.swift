import OttamBackendClient
import Foundation
import Observation

/// Core owns identity, segmentation, and order. This cache merges revisions and
/// reads body pages; it never decodes an adapter update or replays RunEvents.
@MainActor @Observable
final class ChatTranscriptStore {
    private(set) var conversationID: ConversationID?
    @ObservationIgnored private var visibleBodyIDs: Set<String> = []
    private(set) var coreInstanceID: CoreInstanceID?
    private(set) var sequence: UInt64 = 0
    private(set) var captureProblems: [ConversationRunID: String] = [:]
    private(set) var captureGaps: [ConversationRunID: [TranscriptCoverage]] = [:]
    private(set) var sessionStates: [ConversationRunID: ConversationSessionState] = [:]
    /// Live background tasks per run, in this core instance. A run with none
    /// is absent.
    private(set) var backgroundTasks: [ConversationRunID: Int] = [:]
    /// Running subagent threads per run, in this core instance. A run with
    /// none is absent.
    private(set) var subagentThreads: [ConversationRunID: Int] = [:]
    private var endedSessionRuns: Set<ConversationRunID> = []
    private var disconnectedSessionSequence: UInt64?
    @ObservationIgnored private var cachedItems: [String: ConversationTranscriptItem] = [:]
    @ObservationIgnored private var cachedBodies: [String: String] = [:]
    @ObservationIgnored private var cachedBodyErrors: [String: String] = [:]
    private var publication: UInt64 = 0
    var items: [String: ConversationTranscriptItem] { _ = publication; return cachedItems }
    var bodies: [String: String] { _ = publication; return cachedBodies }
    var bodyErrors: [String: String] { _ = publication; return cachedBodyErrors }
    @ObservationIgnored let streamCommits = StreamingCommitCoordinator()
    @ObservationIgnored private var pendingBytes = 0
    @ObservationIgnored private var pendingDeltas = 0
    /// Mounted native transcript handles text-only display ticks without
    /// invalidating the surrounding SwiftUI workspace graph.
    @ObservationIgnored var publishLiveText: (() -> Bool)?
    private(set) var nextCursor: String?
    @ObservationIgnored private var bodyTasks: [String: Task<Void, Never>] = [:]
    /// A scan is one conversation's, or one subagent thread's of it.
    private struct ResyncKey: Hashable {
        let conversation: ConversationID
        let thread: String?
    }
    @ObservationIgnored private var resyncs: Set<ResyncKey> = []
    @ObservationIgnored private var pendingResyncs: Set<ResyncKey> = []

    init() {
        streamCommits.hasStructuralWork = { [weak self] in (self?.pendingDeltas ?? 0) > 0 }
        streamCommits.pendingStructuralMetrics = { [weak self] in
            (self?.pendingBytes ?? 0, self?.pendingDeltas ?? 0)
        }
        streamCommits.drainStructural = { [weak self] in self?.flushPublication() }
    }

    func flushPublication(notifyObservers: Bool = false) {
        guard pendingDeltas > 0 else { return }
        pendingBytes = 0
        pendingDeltas = 0
        if notifyObservers || publishLiveText?() != true { publication &+= 1 }
    }

    private func changed(bytes: Int = 0, immediately: Bool = false) {
        pendingBytes += bytes
        pendingDeltas += 1
        if immediately || streamCommits.displayLink == nil { flushPublication(notifyObservers: true) }
        else { streamCommits.wake() }
    }

    var orderedItems: [ConversationTranscriptItem] { items.values.sorted { $0.position < $1.position } }

    /// Stops reading bodies while the conversation is out of view. Deltas
    /// still merge; the view names its rows again on return.
    func suspendBodies() {
        visibleBodyIDs.removeAll()
        for task in bodyTasks.values { task.cancel() }
        bodyTasks.removeAll()
    }

    func select(_ conversationID: ConversationID) {
        guard self.conversationID != conversationID else { return }
        reset()
        self.conversationID = conversationID
    }

    /// A subagent thread's store: the thread's rows and its nested threads'.
    /// Core filters its pages; deltas, which carry the whole conversation,
    /// filter here.
    private(set) var threadScope: String?
    @ObservationIgnored private var threadFamily: Set<String> = []

    func select(_ conversationID: ConversationID, thread: String) {
        guard self.conversationID != conversationID || threadScope != thread else { return }
        threadScope = thread
        reset()
        self.conversationID = conversationID
    }

    /// Stops following a thread: the store holds nothing until selected again.
    func release() {
        threadScope = nil
        reset()
    }

    /// The incoming rows a thread's store keeps: a nested thread joins the
    /// family when its anchor arrives, and with it the rows it gathers. Core
    /// filtered a page already, against every anchor it holds, so a page's
    /// rows are all kept and their threads join the family: a nested thread's
    /// rows can come a page before its anchor.
    private func admitted(_ incoming: [ConversationTranscriptItem], page: Bool) -> [ConversationTranscriptItem] {
        guard threadScope != nil else { return incoming }
        if page {
            for item in incoming {
                if let thread = item.subagentThreadID { threadFamily.insert(thread) }
                if let thread = item.tool?.subagentThread { threadFamily.insert(thread.id) }
            }
            return incoming
        }
        var grew = true
        while grew {
            grew = false
            for item in incoming {
                guard let thread = item.tool?.subagentThread, let parent = thread.parentThreadID,
                      threadFamily.contains(parent) else { continue }
                grew = threadFamily.insert(thread.id).inserted || grew
            }
        }
        return incoming.filter { item in
            item.subagentThreadID.map(threadFamily.contains) == true
                || item.tool?.subagentThread.map { threadFamily.contains($0.id) } == true
                || item.tool?.subagentFollowUp.map { threadFamily.contains($0.threadID) } == true
        }
    }

    func reset() {
        threadFamily = threadScope.map { [$0] } ?? []
        visibleBodyIDs.removeAll()
        for task in bodyTasks.values { task.cancel() }
        bodyTasks.removeAll()
        cachedItems.removeAll()
        cachedBodies.removeAll()
        cachedBodyErrors.removeAll()
        captureProblems.removeAll()
        captureGaps.removeAll()
        sessionStates.removeAll()
        backgroundTasks.removeAll()
        subagentThreads.removeAll()
        endedSessionRuns.removeAll()
        disconnectedSessionSequence = nil
        changed(immediately: true)
        nextCursor = nil
        sequence = 0
        coreInstanceID = nil
        conversationID = nil
    }

    func merge(_ page: ConversationListTranscriptResult, conversationID: ConversationID, older: Bool = false) {
        select(conversationID)
        adopt(page.coreInstanceID)
        merge(admitted(page.items, page: true))
        if page.sequence >= sequence {
            _ = mergeCapture(page.capture)
            _ = mergeSessions(page.sessionStates, sequence: page.sequence)
            // A page lists every run with a live task; one it omits has none.
            backgroundTasks = Dictionary(
                page.backgroundTasks.filter { $0.live > 0 }.map { ($0.runID, $0.live) },
                uniquingKeysWith: { _, latest in latest }
            )
            subagentThreads = Dictionary(
                page.subagentThreads.filter { $0.live > 0 }.map { ($0.runID, $0.live) },
                uniquingKeysWith: { _, latest in latest }
            )
        }
        changed(immediately: true)
        sequence = max(sequence, page.sequence)
        if older || nextCursor == nil { nextCursor = page.nextCursor }
    }

    /// True asks the owner to refresh the latest committed page after a gap.
    func merge(_ delta: ConversationTranscriptDeltaParams) -> Bool {
        guard conversationID == delta.conversationID else { return false }
        let restarted = coreInstanceID != nil && coreInstanceID != delta.coreInstanceID
        adopt(delta.coreInstanceID)
        // A merged streaming delta covers `sequenceStart...sequence`.
        let missed = (delta.sequenceStart ?? delta.sequence) > sequence + 1
        let incoming = admitted(delta.items, page: false)
        let structural = incoming.contains { item in
            cachedItems[item.itemID] == nil || cachedItems[item.itemID]?.completion != item.completion
                || (item.kind != .assistantText && item.kind != .reasoning)
        }
        // A growth header for a body this store never held carries no text to
        // build on; the owner's page refresh brings the row with its preview.
        let unheld = merge(incoming)
        let captureChanged = delta.sequence >= sequence && mergeCapture(delta.capture)
        let sessionsChanged = delta.sequence >= sequence && mergeSessions(delta.sessionStates, sequence: delta.sequence)
        let current = delta.sequence >= sequence
        let backgroundChanged = current && mergeCounts(delta.backgroundTasks.map { ($0.runID, $0.live) }, into: &backgroundTasks)
        let subagentsChanged = current && mergeCounts(delta.subagentThreads.map { ($0.runID, $0.live) }, into: &subagentThreads)
        let tasksChanged = backgroundChanged || subagentsChanged
        // A thread's store hears every row's text; only its own counts.
        let appends = threadScope == nil ? delta.bodyAppends : delta.bodyAppends.filter { items[$0.itemID] != nil }
        for append in appends {
            guard let item = items[append.itemID],
                  item.kind == .assistantText || item.kind == .reasoning,
                  item.revision == append.revision else { continue }
            let count = UInt64((bodies[append.itemID] ?? "").utf8.count)
            let end = append.offset + UInt64(append.text.utf8.count)
            guard count >= append.offset, count < end, end <= item.bodyByteCount else { continue }
            // A history preview/read may already contain a prefix of this patch.
            let suffix = append.text.utf8.dropFirst(Int(count - append.offset))
            guard let text = String(bytes: suffix, encoding: .utf8) else { continue }
            cachedBodies[append.itemID, default: ""] += text
            cachedBodyErrors[append.itemID] = nil
        }
        if !incoming.isEmpty || !appends.isEmpty || captureChanged || sessionsChanged || tasksChanged {
            changed(bytes: appends.reduce(0) { $0 + $1.text.utf8.count },
                    immediately: structural || captureChanged || sessionsChanged || tasksChanged)
        }
        sequence = max(sequence, delta.sequence)
        return restarted || missed || unheld || delta.recoveryRequired
    }

    private func mergeCapture(_ capture: [TranscriptCaptureStatus]) -> Bool {
        var changed = false
        for status in capture {
            var gaps: [TranscriptCoverage] = []
            for gap in status.gaps.sorted(by: { $0.startPosition < $1.startPosition }) where gap.startPosition <= gap.endPosition {
                if let last = gaps.last,
                   gap.startPosition <= last.endPosition || gap.startPosition - last.endPosition == 1 {
                    gaps[gaps.count - 1].endPosition = max(last.endPosition, gap.endPosition)
                } else { gaps.append(gap) }
            }
            if (captureGaps[status.runID] ?? []) != gaps {
                captureGaps[status.runID] = gaps.isEmpty ? nil : gaps
                changed = true
            }
            if captureProblems[status.runID] != status.problem {
                captureProblems[status.runID] = status.problem
                changed = true
            }
        }
        return changed
    }

    /// A delta names only the runs whose count changed, zero included.
    private func mergeCounts(_ counts: [(ConversationRunID, Int)], into store: inout [ConversationRunID: Int]) -> Bool {
        var didChange = false
        for (runID, count) in counts {
            let live = count > 0 ? count : nil
            if store[runID] != live {
                store[runID] = live
                didChange = true
            }
        }
        return didChange
    }

    func endSessionActivity(_ runID: ConversationRunID) {
        endedSessionRuns.insert(runID)
        if var state = sessionStates[runID] {
            state.live = false
            state.signal.activity = nil
            sessionStates[runID] = state
        }
    }

    func disconnectSessionActivity() {
        disconnectedSessionSequence = sequence
        for runID in sessionStates.keys {
            sessionStates[runID]?.live = false
            sessionStates[runID]?.signal.activity = nil
        }
    }

    private func mergeSessions(_ incoming: [ConversationSessionState], sequence: UInt64) -> Bool {
        var didChange = false
        for var state in incoming {
            if !state.live || endedSessionRuns.contains(state.runID)
                || disconnectedSessionSequence.map({ sequence <= $0 }) == true {
                state.live = false
                state.signal.activity = nil
            }
            if sessionStates[state.runID] != state {
                sessionStates[state.runID] = state
                didChange = true
            }
        }
        return didChange
    }

    private func adopt(_ instance: CoreInstanceID) {
        if let current = coreInstanceID, current != instance {
            let selected = conversationID
            reset()
            conversationID = selected
        }
        coreInstanceID = instance
    }

    /// Rescan every position page after a sequence gap. Concurrent requests
    /// coalesce; a Core restart discards the old scan and starts at its head.
    /// A thread store's scan is its thread's: retargeting starts a new one,
    /// and the old one stops at its next page.
    func resync(_ conversation: ConversationID,
                readPage: (String?) async throws -> ConversationListTranscriptResult) async throws {
        guard conversationID == conversation else { return }
        let key = ResyncKey(conversation: conversation, thread: threadScope)
        guard resyncs.insert(key).inserted else {
            pendingResyncs.insert(key)
            return
        }
        defer {
            resyncs.remove(key)
            pendingResyncs.remove(key)
        }
        repeat {
            pendingResyncs.remove(key)
            let oldestLoaded = cachedItems.values.map(\.position).min()
            var instance = coreInstanceID
            var cursor: String?
            repeat {
                let page = try await readPage(cursor)
                guard conversationID == conversation, threadScope == key.thread, !Task.isCancelled else { return }
                if coreInstanceID != instance {
                    pendingResyncs.insert(key)
                    break
                }
                if let instance, page.coreInstanceID != instance {
                    reset()
                    select(conversation)
                    pendingResyncs.insert(key)
                    break
                }
                merge(page, conversationID: conversation, older: cursor != nil)
                instance = page.coreInstanceID
                cursor = page.nextCursor
                // Recover only the already-loaded coverage. Older history stays
                // paginated; revisions inside that coverage must still be revisited.
                if let oldestLoaded, let coverage = page.coverage,
                   coverage.startPosition <= oldestLoaded { break }
                if oldestLoaded == nil { break }
            } while cursor != nil
        } while pendingResyncs.contains(key) && !Task.isCancelled
    }

    /// Merges rows `admitted` already kept. True when a row arrived without
    /// its preview for a body this store does not hold; that row is skipped.
    @discardableResult
    private func merge(_ incoming: [ConversationTranscriptItem]) -> Bool {
        var unheld = false
        for item in incoming {
            if var prior = cachedItems[item.itemID], prior.revision == item.revision {
                prior.committedRevision = max(prior.committedRevision ?? 0, item.committedRevision ?? 0)
                cachedItems[item.itemID] = prior
                continue
            }
            guard items[item.itemID].map({ $0.revision < item.revision }) ?? true else { continue }
            if item.previewOmitted {
                // The delta's appends carry the new text; keep the preview and
                // body already held.
                guard cachedBodies[item.itemID] != nil else { unheld = true; continue }
                var item = item
                item.bodyPreview = cachedItems[item.itemID]?.bodyPreview ?? ""
                item.previewOmitted = false
                cachedItems[item.itemID] = item
                continue
            }
            cachedItems[item.itemID] = item
            if item.kind == .tool { cachedBodies[item.itemID] = item.bodyPreview }
            if UInt64(item.bodyPreview.utf8.count) == item.bodyByteCount { cachedBodies[item.itemID] = item.bodyPreview }
            if cachedBodies[item.itemID] == nil { cachedBodies[item.itemID] = item.bodyPreview }
        }
        return unheld
    }

    func loadBodies(workspaceID: WorkspaceID, backend: DiffReviewModel) {
        loadBodies(read: { [weak backend] conversationID, itemID, revision, offset in
            guard let backend else { throw CancellationError() }
            return try await backend.readConversationTranscriptBody(workspaceID: workspaceID,
                conversationID: conversationID, itemID: itemID, revision: revision, offset: offset)
        }, itemIDs: visibleBodyIDs)
    }

    func visibleBodies(_ ids: Set<String>, read: @escaping ChatPayloadDetailStore.BodyReader) {
        visibleBodyIDs = ids
        for (id, task) in bodyTasks where !ids.contains(id) { task.cancel(); bodyTasks[id] = nil }
        loadBodies(read: { _, id, revision, offset in try await read(id, revision, offset) }, itemIDs: ids)
    }

    typealias BodyReader = @MainActor (ConversationID, String, UInt64, UInt64) async throws -> ConversationReadTranscriptBodyResult

    @discardableResult
    func loadBodies(read: @escaping BodyReader, itemIDs: Set<String>? = nil) -> [Task<Void, Never>] {
        guard let conversationID else { return [] }
        for item in items.values where item.bodyByteCount > UInt64((bodies[item.itemID] ?? "").utf8.count) {
            // Both streaming kinds: a body held only as its preview skips live
            // appends past it, and only a read fills the gap.
            guard item.kind == .assistantText || item.kind == .reasoning else { continue }
            guard itemIDs?.contains(item.itemID) != false else { continue }
            guard bodyTasks[item.itemID] == nil else { continue }
            let instance = coreInstanceID
            bodyTasks[item.itemID] = Task { [weak self] in
                guard let self else { return }
                defer {
                    if !Task.isCancelled { self.bodyTasks[item.itemID] = nil }
                }
                var requestedRevision = item.revision
                do {
                    while !Task.isCancelled, self.conversationID == conversationID, self.coreInstanceID == instance,
                          let current = self.items[item.itemID] {
                        let offset = UInt64((self.bodies[item.itemID] ?? "").utf8.count)
                        guard offset < current.bodyByteCount else { break }
                        requestedRevision = current.revision
                        let page = try await read(conversationID, item.itemID, current.revision, offset)
                        guard !Task.isCancelled, self.conversationID == conversationID, self.coreInstanceID == instance else { return }
                        guard self.items[item.itemID]?.completion != .suppressed else { return }
                        // Text bodies are append-only within one immutable item;
                        // a metadata revision does not invalidate an earlier prefix.
                        if current.kind == .tool, self.items[item.itemID]?.revision != page.revision { continue }
                        guard !page.text.isEmpty else { break }
                        // Live patches can arrive while the page is in flight.
                        // Append only the part not already represented locally.
                        let currentOffset = UInt64((self.bodies[item.itemID] ?? "").utf8.count)
                        guard currentOffset >= offset else { continue }
                        let skipped = min(UInt64(page.text.utf8.count), currentOffset - offset)
                        if let suffix = String(bytes: page.text.utf8.dropFirst(Int(skipped)), encoding: .utf8) {
                            self.cachedBodies[item.itemID, default: ""] += suffix
                        }
                        self.cachedBodyErrors[item.itemID] = nil
                        self.changed()
                    }
                } catch {
                    guard !Task.isCancelled else { return }
                    if self.items[item.itemID]?.revision != requestedRevision {
                        Task { [weak self] in
                            await Task.yield()
                            guard let self, self.conversationID == conversationID, self.coreInstanceID == instance else { return }
                            self.loadBodies(read: read, itemIDs: itemIDs)
                        }
                        return
                    }
                    self.cachedBodyErrors[item.itemID] = error.localizedDescription
                    self.changed(immediately: true)
                }
            }
        }
        return Array(bodyTasks.values)
    }
}
