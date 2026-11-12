import Testing
import OttamBackendClient
@testable import OttamApp

@MainActor
struct ChatTranscriptPresentationCoordinatorTests {
    @Test("a failed final publication retries without another update and exposes bounded recovery")
    func finalPublicationRecovery() async {
        for failureCount in 1...2 {
            let presentation = ChatTranscriptPresentationCoordinator()
            let snapshot = TranscriptFixture.snapshot()
            let (attempts, signal) = AsyncStream<Int>.makeStream()
            var count = 0
            presentation.enqueue(snapshot: snapshot) { _, _, completion in
                count += 1
                completion.finish(count > failureCount)
                signal.yield(count)
            }
            var iterator = attempts.makeAsyncIterator()
            #expect(await iterator.next() == 1)
            #expect(await iterator.next() == 2)
            if failureCount == 2 {
                #expect(presentation.hasFailedPublication)
                #expect(presentation.copyText(for: "answer") == nil)
                presentation.retryFailedPublication()
                #expect(await iterator.next() == 3)
            }
            #expect(!presentation.hasFailedPublication)
            #expect(presentation.copyText(for: "answer") == "answer")
            signal.finish()
        }
    }

    @Test("whole-message copy excludes a streaming Run but keeps completed previous turns")
    func completedTurnCopyOnly() {
        var snapshot = TranscriptFixture.snapshot(liveRun: TranscriptFixture.liveRun("run-2"))
        snapshot.runStatuses = [.init("run-1"): .completed, .init("run-2"): .running]
        var active = TranscriptFixture.item("active", position: 2, body: "streaming")
        active.runID = .init("run-2")
        active.completion = .complete // A closed segment is not a completed turn.
        snapshot.normalizedItems.append(active)
        #expect(ChatTranscriptPresentationCoordinator.copySources(snapshot)["answer"] == "answer")
        #expect(ChatTranscriptPresentationCoordinator.copySources(snapshot)["active"] == nil)
        for status in [ConversationRunStatus.completed, .canceled, .interrupted, .failed] {
            snapshot.runStatuses[.init("run-2")] = status
            #expect(ChatTranscriptPresentationCoordinator.copySources(snapshot)["active"] == "streaming")
        }
    }

    @Test("live snapshots wait for native completion and terminal publication cancels pending work")
    func coalescedPublication() async throws {
        let presentation = ChatTranscriptPresentationCoordinator()
        var snapshot = TranscriptFixture.snapshot(liveRun: TranscriptFixture.liveRun("run-1"))
        var published: [String] = []
        var completions: [CanonicalPresentationCompletion] = []
        let (publications, publicationContinuation) = AsyncStream<String>.makeStream()
        let publish: ChatTranscriptPresentationCoordinator.Publication = { _, snapshot, completion in
            let body = snapshot.normalizedItems.last?.bodyPreview ?? ""
            published.append(body)
            publicationContinuation.yield(body)
            completions.append(completion)
        }
        snapshot.normalizedItems = [TranscriptFixture.item("answer", body: "A")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 2, body: "AB")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 3, body: "ABC")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        #expect(published == ["A"])
        completions[0].finish(true)
        var events = publications.makeAsyncIterator()
        #expect(await events.next() == "A")
        #expect(await events.next() == "ABC")
        #expect(published == ["A", "ABC"])
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 4, body: "ABCD")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        snapshot.liveRun = nil
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 5, body: "Final")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        #expect(await events.next() == "Final")
        publicationContinuation.finish()
        #expect(published == ["A", "ABC", "Final"])
    }

    @Test("switching conversations cancels queued publication and ignores old native completion")
    func conversationSwitch() {
        let presentation = ChatTranscriptPresentationCoordinator()
        var snapshot = TranscriptFixture.snapshot(liveRun: TranscriptFixture.liveRun("run-1"))
        var published: [String] = []
        var completions: [CanonicalPresentationCompletion] = []
        let publish: ChatTranscriptPresentationCoordinator.Publication = { _, snapshot, completion in
            published.append(snapshot.normalizedItems[0].bodyPreview)
            completions.append(completion)
        }
        snapshot.normalizedItems = [TranscriptFixture.item("answer", body: "old")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 2, body: "queued old")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        snapshot.conversationID = .init("other")
        snapshot.liveRun = nil
        snapshot.runStatuses = [.init("run-1"): .completed]
        snapshot.normalizedItems = [TranscriptFixture.item("new-answer", body: "new")]
        presentation.enqueue(snapshot: snapshot, publish: publish)
        completions[1].finish(true)
        completions[0].finish(true)
        #expect(published == ["old", "new"])
        #expect(presentation.copyText(for: "answer") == nil)
        #expect(presentation.copyText(for: "new-answer") == "new")
    }

    @Test("incremental projection is limited to a single unchanged-identity text append")
    func appendEligibility() {
        func text(_ source: String, id: String = "answer") -> TranscriptItem {
            .init(id: id, content: .markdown(source, syntax: .full, container: nil))
        }
        let append = MacAgentTranscriptViewController.appendOnlyChange(from: [text("e")], to: [text("e\u{301}!")])
        #expect(append?.delta == "\u{301}!")
        #expect(MacAgentTranscriptViewController.appendOnlyChange(from: [text("A")], to: [text("replacement")]) == nil)
        #expect(MacAgentTranscriptViewController.appendOnlyChange(from: [text("A")], to: [text("AB", id: "new")]) == nil)
        #expect(MacAgentTranscriptViewController.appendOnlyChange(from: [text("A")], to: [text("AB"), text("steer", id: "user")]) == nil)
    }

    @Test("A superseded native publication cannot overwrite a newer normalized revision")
    func stalePublication() {
        let presentation = ChatTranscriptPresentationCoordinator()
        var completions: [CanonicalPresentationCompletion] = []
        var snapshot = TranscriptFixture.snapshot()
        presentation.reconcile(snapshot: snapshot) { _, _, completion in completions.append(completion) }
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 2, body: "new answer")]
        presentation.reconcile(snapshot: snapshot) { _, _, completion in completions.append(completion) }
        completions[1].finish(true)
        completions[0].finish(true)
        #expect(presentation.copyText(for: "answer") == "new answer")
    }

    @Test("Failed publication retains the last successfully presented content")
    func failedPublication() {
        let presentation = ChatTranscriptPresentationCoordinator()
        var snapshot = TranscriptFixture.snapshot()
        presentation.reconcile(snapshot: snapshot) { _, _, completion in completion.finish(true) }
        snapshot.normalizedItems = [TranscriptFixture.item("answer", revision: 2, body: "new answer")]
        presentation.reconcile(snapshot: snapshot) { _, _, completion in completion.finish(false) }
        #expect(presentation.copyText(for: "answer") == "answer")
    }
}
