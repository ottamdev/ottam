import AppKit
import OttamBackendClient
import OttamInstrumentation
import Testing
@testable import OttamApp

@MainActor
private final class RenderTelemetryRecorder {
    private(set) var began: [UInt64] = []
    private(set) var completed: [UInt64: [TranscriptSignpost.CollectionCompletion]] = [:]
    private(set) var applied: [UInt64] = []
    private(set) var mutated: [UInt64] = []
    private(set) var presentationBegan: [UInt64] = []
    private(set) var presentationFinished: [UInt64: [TranscriptSignpost.CollectionCompletion]] = [:]
    private(set) var transactionsCommitted: [UInt64] = []

    var telemetry: TranscriptRenderTransactionTelemetry {
        TranscriptRenderTransactionTelemetry(
            beginCommitPresent: { [weak self] id in
                self?.presentationBegan.append(id)
                return ({ [weak self] result in self?.presentationFinished[id, default: []].append(result) },
                        { [weak self] in self?.transactionsCommitted.append(id) })
            },
            beginCollectionUpdate: { [weak self] metadata in
                self?.began.append(metadata.transactionID)
                return { [weak self] completion in
                    self?.completed[metadata.transactionID, default: []].append(completion)
                }
            },
            collectionApply: { [weak self] metadata, body in
                self?.applied.append(metadata.transactionID)
                body()
            },
            collectionMutation: { [weak self] metadata, body in
                self?.mutated.append(metadata.transactionID)
                body()
            }
        )
    }
}

@Suite("Transcript render telemetry")
@MainActor
struct TranscriptRenderTelemetryTests {
    private func batchCommit(
        transactionID: UInt64
    ) -> TranscriptRenderTransactionCoordinator.Commit {
        TranscriptRenderTransactionCoordinator.Commit(
            transactionID: transactionID,
            collectionMutation: TranscriptCollectionMutation(
                mode: .batch,
                deletions: [],
                insertions: [IndexPath(item: 0, section: 0)],
                reloads: [],
                moves: []
            ),
            forceFullLayout: false,
            viewportTarget: .unchanged,
            viewportEpoch: 0,
            refreshIDs: [],
            refreshAllMaterializedItems: false,
            candidatePresentation: .empty,
            settlement: .canonical(CanonicalPresentationCompletion { _ in })
        )
    }

    @Test("synchronous native completion closes collection intervals once")
    func synchronousCompletion() {
        let recorder = RenderTelemetryRecorder()
        var layoutCommits = 0
        let coordinator = TranscriptRenderTransactionCoordinator(
            batchUpdater: { updates, completion in
                updates()
                completion(true)
            },
            applyCollectionMutation: { _ in },
            commitLayout: { _ in layoutCommits += 1 },
            promotePresentation: { _ in },
            settle: { _, _ in },
            ready: {},
            telemetry: recorder.telemetry
        )
        let transactionID = coordinator.reserveTransactionID()

        let result = coordinator.submit(batchCommit(transactionID: transactionID))

        #expect(result == .completedSynchronously)
        #expect(recorder.began == [transactionID])
        #expect(recorder.applied == [transactionID])
        #expect(recorder.mutated == [transactionID])
        #expect(recorder.completed[transactionID] == [.succeeded])
        #expect(layoutCommits == 1)
        #expect(recorder.presentationBegan == [transactionID])
        #expect(recorder.presentationFinished[transactionID] == [.succeeded])
        #expect(recorder.transactionsCommitted == [transactionID])
    }

    @Test("false and late native completion close collection intervals once")
    func failedCompletion() throws {
        let recorder = RenderTelemetryRecorder()
        var nativeCompletion: ((Bool) -> Void)?
        var layoutCommits = 0
        let coordinator = TranscriptRenderTransactionCoordinator(
            batchUpdater: { updates, completion in
                updates()
                nativeCompletion = completion
            },
            applyCollectionMutation: { _ in },
            commitLayout: { _ in layoutCommits += 1 },
            promotePresentation: { _ in },
            settle: { _, _ in },
            ready: {},
            telemetry: recorder.telemetry
        )
        let transactionID = coordinator.reserveTransactionID()
        #expect(coordinator.submit(batchCommit(transactionID: transactionID)) == .awaitingNativeCompletion)
        let completion = try #require(nativeCompletion)

        completion(false)
        completion(true)

        #expect(recorder.completed[transactionID] == [.failed])
        #expect(recorder.mutated == [transactionID])
        #expect(layoutCommits == 0)
        #expect(recorder.presentationFinished[transactionID] == [.failed])
        #expect(recorder.transactionsCommitted == [transactionID])
    }

    @Test("teardown closes collection interval before ignoring late native completion")
    func teardownCompletion() throws {
        let recorder = RenderTelemetryRecorder()
        var nativeCompletion: ((Bool) -> Void)?
        let coordinator = TranscriptRenderTransactionCoordinator(
            batchUpdater: { updates, completion in
                updates()
                nativeCompletion = completion
            },
            applyCollectionMutation: { _ in },
            commitLayout: { _ in },
            promotePresentation: { _ in },
            settle: { _, _ in },
            ready: {},
            telemetry: recorder.telemetry
        )
        let transactionID = coordinator.reserveTransactionID()
        _ = coordinator.submit(batchCommit(transactionID: transactionID))
        let completion = try #require(nativeCompletion)

        coordinator.tearDown()
        completion(true)

        #expect(recorder.completed[transactionID] == [.tornDown])
        #expect(recorder.presentationFinished[transactionID] == [.tornDown])
    }
}
