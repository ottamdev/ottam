import OttamBackendClient
import OttamInstrumentation
import Observation
import Foundation

@MainActor
final class CanonicalPresentationCompletion {
    private var completion: ((Bool) -> Void)?

    init(_ completion: @escaping (Bool) -> Void) {
        self.completion = completion
    }

    func finish(_ succeeded: Bool) {
        guard let completion else { return }
        self.completion = nil
        completion(succeeded)
    }
}

/// Publishes immutable normalized snapshots. Core owns streaming revisions;
/// Swift only commits a presentation after the native renderer accepts it.
@MainActor @Observable
final class ChatTranscriptPresentationCoordinator {
    private(set) var items: [TranscriptItem] = []
    @ObservationIgnored private var sources: [String: String] = [:]
    @ObservationIgnored private var generation: UInt64 = 0
    typealias Publication = ([TranscriptItem], ChatTranscriptSnapshot, CanonicalPresentationCompletion) -> Void
    @ObservationIgnored private var pending: (ChatTranscriptSnapshot, Publication)?
    @ObservationIgnored private var inFlight = false
    @ObservationIgnored private var conversationID: ConversationID?
    private(set) var hasFailedPublication = false
    @ObservationIgnored private var automaticRetriesRemaining = 1

    /// Core revisions remain authoritative. Only UI publication is coalesced;
    /// the latest complete snapshot replaces intermediate presentation work.
    func enqueue(snapshot: ChatTranscriptSnapshot, publish: @escaping Publication) {
        automaticRetriesRemaining = 1
        hasFailedPublication = false
        let changedConversation = conversationID != snapshot.conversationID
        conversationID = snapshot.conversationID
        if changedConversation || snapshot.liveRun == nil {
            cancelPendingPresentation()
            reconcile(snapshot: snapshot, publish: publish)
            return
        }
        pending = (snapshot, publish)
        if !inFlight { publishPending() }
    }

    func cancelPendingPresentation() {
        pending = nil
        hasFailedPublication = false
        generation &+= 1
        inFlight = false
    }

    func retryFailedPublication() {
        guard hasFailedPublication else { return }
        hasFailedPublication = false
        automaticRetriesRemaining = 1
        publishPending()
    }

    private func publishPending() {
        guard let (snapshot, publish) = pending else { return }
        pending = nil
        reconcile(snapshot: snapshot, publish: publish)
    }

    func reconcile(snapshot: ChatTranscriptSnapshot,
        publish: @escaping Publication) {
        generation &+= 1
        inFlight = true
        let ticket = generation
        let plan = ChatTranscriptAssembler.plan(snapshot)
        let copy = Self.copySources(snapshot)
        publish(plan, snapshot, CanonicalPresentationCompletion { [weak self] succeeded in
            TranscriptSignpost.normalizedPublication(generation: ticket, succeeded: succeeded)
            guard let self, self.generation == ticket else { return }
            self.inFlight = false
            if succeeded {
                self.hasFailedPublication = false
                self.items = plan
                self.sources = copy
            } else if self.pending == nil {
                self.pending = (snapshot, publish)
                if self.automaticRetriesRemaining == 0 {
                    self.hasFailedPublication = true
                    return
                }
                self.automaticRetriesRemaining -= 1
            }
            // Native completion may run inside its current transaction. Defer
            // the next latest-only publication until that transaction unwinds.
            Task { @MainActor [weak self] in
                guard let self, self.generation == ticket, !self.inFlight else { return }
                self.publishPending()
            }
        })
    }

    func copyText(for itemID: String) -> String? { sources[itemID] }

    static func copySources(_ snapshot: ChatTranscriptSnapshot) -> [String: String] {
        var sources = Dictionary(snapshot.messages.map { ($0.message.messageID.rawValue, $0.message.body) },
            uniquingKeysWith: { _, latest in latest })
        var statuses = snapshot.runStatuses
        for item in snapshot.normalizedItems {
            if let runID = item.runID, let status = item.runStatus { statuses[runID] = status }
        }
        for item in snapshot.normalizedItems where item.completion != .suppressed && (item.kind == .assistantText || item.kind == .reasoning) {
            guard let runID = item.runID,
                  let status = statuses[runID],
                  status != .running && status != .queued else { continue }
            sources[item.itemID] = snapshot.normalizedBodies[item.itemID] ?? item.bodyPreview
        }
        return sources
    }
}
