import OttamBackendClient
import OttamInstrumentation
import Foundation
import QuartzCore

struct TranscriptCompletedPresentation {
    let sourcePlan: [TranscriptItem]
    let blocks: [TranscriptBlock]
    let indexByID: [TranscriptBlockID: Int]

    static let empty = TranscriptCompletedPresentation(
        sourcePlan: [],
        blocks: [],
        indexByID: [:]
    )
}

struct TranscriptCollectionMutation {
    struct Move {
        let from: IndexPath
        let to: IndexPath
    }

    enum Mode: Equatable {
        case none
        case reload
        case batch
    }

    let mode: Mode
    let deletions: Set<IndexPath>
    let insertions: Set<IndexPath>
    let reloads: Set<IndexPath>
    let moves: [Move]

    static let none = TranscriptCollectionMutation(
        mode: .none,
        deletions: [],
        insertions: [],
        reloads: [],
        moves: []
    )

    static let reload = TranscriptCollectionMutation(
        mode: .reload,
        deletions: [],
        insertions: [],
        reloads: [],
        moves: []
    )
}

struct TranscriptRenderTransactionMetadata: Equatable {
    let transactionID: UInt64
    let insertions: Int
    let deletions: Int
    let moves: Int
}

@MainActor
struct TranscriptRenderTransactionTelemetry {
    typealias PresentationMeasurement = (finish: (TranscriptSignpost.CollectionCompletion) -> Void, commit: () -> Void)
    var beginCommitPresent: (UInt64) -> PresentationMeasurement? = { _ in nil }
    typealias CollectionUpdateCompletion = (TranscriptSignpost.CollectionCompletion) -> Void

    let beginCollectionUpdate: (
        TranscriptRenderTransactionMetadata
    ) -> CollectionUpdateCompletion
    let collectionApply: (TranscriptRenderTransactionMetadata, () -> Void) -> Void
    let collectionMutation: (TranscriptRenderTransactionMetadata, () -> Void) -> Void

    static let live = Self(
        beginCommitPresent: { transactionID in
            guard TranscriptSignpost.isEnabled else { return nil }
            let measurement = TranscriptSignpost.CommitPresentation(transactionID: transactionID)
            return (measurement.finish, measurement.commitTransaction)
        },
        beginCollectionUpdate: { metadata in
            let interval = TranscriptSignpost.beginCollectionUpdate(
                transactionID: metadata.transactionID,
                insertions: metadata.insertions,
                deletions: metadata.deletions,
                moves: metadata.moves
            )
            return { completion in
                TranscriptSignpost.endCollectionUpdate(interval, completion: completion)
            }
        },
        collectionApply: { metadata, body in
            TranscriptSignpost.collectionApply(
                transactionID: metadata.transactionID,
                body
            )
        },
        collectionMutation: { metadata, body in
            TranscriptSignpost.collectionMutation(
                transactionID: metadata.transactionID,
                body
            )
        }
    )
}

/// Serializes projection publication with AppKit's native collection update
/// lifecycle. Projection and layout entries are staged before submission;
/// viewport publication, materialization, presentation promotion, and canonical
/// settlement happen here only after native completion.
@MainActor
final class TranscriptRenderTransactionCoordinator {
    typealias BatchUpdater = (
        _ updates: @escaping () -> Void,
        _ completion: @escaping (Bool) -> Void
    ) -> Void

    enum Settlement {
        case none
        case canonical(CanonicalPresentationCompletion)
    }

    struct Commit {
        let transactionID: UInt64
        let collectionMutation: TranscriptCollectionMutation
        let forceFullLayout: Bool
        let viewportTarget: TranscriptViewportTarget
        let viewportEpoch: UInt64
        let refreshIDs: Set<TranscriptBlockID>
        let refreshAllMaterializedItems: Bool
        let candidatePresentation: TranscriptCompletedPresentation
        let settlement: Settlement
    }

    enum CompletionResult: Equatable {
        case presented
        case nativeUpdateFailed
        case tornDown
    }

    enum SubmissionResult: Equatable {
        case completedSynchronously
        case awaitingNativeCompletion
        case busy
        case tornDown
    }

    private struct InFlightUpdate {
        let transactionID: UInt64
        let commit: Commit
        let endCollectionUpdate: TranscriptRenderTransactionTelemetry.CollectionUpdateCompletion?
        let endCommitPresent: ((TranscriptSignpost.CollectionCompletion) -> Void)?
    }

    private enum State {
        case idle
        case updating(InFlightUpdate)
        case tornDown
    }

    private let batchUpdater: BatchUpdater
    private let applyCollectionMutation: (Commit) -> Void
    private let commitLayout: (Commit) -> Void
    private let promotePresentation: (TranscriptCompletedPresentation) -> Void
    private let settle: (Settlement, CompletionResult) -> Void
    private let ready: () -> Void
    private let telemetry: TranscriptRenderTransactionTelemetry

    private var state: State = .idle
    private var nextTransactionID: UInt64 = 0
    private var nativeCallDepth = 0
    private var deferredNativeResult: (transactionID: UInt64, succeeded: Bool)?
    private(set) var completedPresentation = TranscriptCompletedPresentation.empty

    init(
        batchUpdater: @escaping BatchUpdater,
        applyCollectionMutation: @escaping (Commit) -> Void,
        commitLayout: @escaping (Commit) -> Void,
        promotePresentation: @escaping (TranscriptCompletedPresentation) -> Void,
        settle: @escaping (Settlement, CompletionResult) -> Void,
        ready: @escaping () -> Void,
        telemetry: TranscriptRenderTransactionTelemetry? = nil
    ) {
        self.batchUpdater = batchUpdater
        self.applyCollectionMutation = applyCollectionMutation
        self.commitLayout = commitLayout
        self.promotePresentation = promotePresentation
        self.settle = settle
        self.ready = ready
        self.telemetry = telemetry ?? .live
    }

    var isBusy: Bool {
        if case .updating = state { return true }
        return false
    }

    var isTornDown: Bool {
        if case .tornDown = state { return true }
        return false
    }

    func reserveTransactionID() -> UInt64 {
        precondition(!isTornDown, "a torn-down renderer cannot reserve a transaction")
        nextTransactionID &+= 1
        return nextTransactionID
    }

    @discardableResult
    func submit(_ commit: Commit) -> SubmissionResult {
        switch state {
        case .updating:
            return .busy
        case .tornDown:
            settle(commit.settlement, .tornDown)
            return .tornDown
        case .idle:
            break
        }

        precondition(
            commit.transactionID > 0 && commit.transactionID <= nextTransactionID,
            "render transaction IDs must be reserved before preparation"
        )
        let presentation: TranscriptRenderTransactionTelemetry.PresentationMeasurement?
        if case .canonical = commit.settlement {
            presentation = telemetry.beginCommitPresent(commit.transactionID)
        } else { presentation = nil }
        defer { presentation?.commit() }
        var update = InFlightUpdate(
            transactionID: commit.transactionID,
            commit: commit,
            endCollectionUpdate: nil,
            endCommitPresent: presentation?.finish
        )
        state = .updating(update)
        switch commit.collectionMutation.mode {
        case .none:
            finishSynchronously(update)
            return .completedSynchronously
        case .reload:
            applyCollectionMutation(commit)
            finishSynchronously(update)
            return .completedSynchronously
        case .batch:
            break
        }

        let metadata = metadata(for: commit)
        update = InFlightUpdate(
            transactionID: commit.transactionID,
            commit: commit,
            endCollectionUpdate: telemetry.beginCollectionUpdate(metadata),
            endCommitPresent: presentation?.finish
        )
        state = .updating(update)

        nativeCallDepth += 1
        telemetry.collectionApply(metadata) {
            let transactionID = commit.transactionID
            batchUpdater(
                { [applyCollectionMutation, telemetry] in
                    telemetry.collectionMutation(metadata) {
                        applyCollectionMutation(commit)
                    }
                },
                { [weak self] succeeded in
                    self?.finish(transactionID: transactionID, succeeded: succeeded)
                }
            )
        }
        nativeCallDepth -= 1
        drainDeferredNativeResultIfPossible()
        if case .updating = state { return .awaitingNativeCompletion }
        return .completedSynchronously
    }

    func tearDown() {
        guard !isTornDown else { return }
        let active: InFlightUpdate?
        if case .updating(let update) = state { active = update } else { active = nil }
        state = .tornDown
        deferredNativeResult = nil
        if let active {
            active.endCollectionUpdate?(.tornDown)
            active.endCommitPresent?(.tornDown)
            settle(active.commit.settlement, .tornDown)
        }
    }

    private func finish(transactionID: UInt64, succeeded: Bool) {
        guard case .updating(let update) = state,
              update.transactionID == transactionID
        else { return }
        guard nativeCallDepth == 0 else {
            deferredNativeResult = (transactionID, succeeded)
            return
        }
        finalize(update, succeeded: succeeded)
    }

    private func drainDeferredNativeResultIfPossible() {
        guard nativeCallDepth == 0, let deferredNativeResult else { return }
        self.deferredNativeResult = nil
        finish(
            transactionID: deferredNativeResult.transactionID,
            succeeded: deferredNativeResult.succeeded
        )
    }

    private func finishSynchronously(_ update: InFlightUpdate) {
        commitLayout(update.commit)
        completedPresentation = update.commit.candidatePresentation
        promotePresentation(update.commit.candidatePresentation)
        update.endCommitPresent?(.succeeded)
        settle(update.commit.settlement, .presented)
        state = .idle
        notifyReady()
    }

    private func finalize(_ update: InFlightUpdate, succeeded: Bool) {
        guard case .updating(let current) = state,
              current.transactionID == update.transactionID
        else { return }
        update.endCollectionUpdate?(succeeded ? .succeeded : .failed)
        if succeeded {
            // Native batch completion may arrive in a later run-loop turn.
            // Attach the proxy to the transaction carrying final layout too.
            if update.endCommitPresent != nil { CATransaction.begin() }
            defer { if update.endCommitPresent != nil { CATransaction.commit() } }
            commitLayout(update.commit)
            completedPresentation = update.commit.candidatePresentation
            promotePresentation(update.commit.candidatePresentation)
            update.endCommitPresent?(.succeeded)
            settle(update.commit.settlement, .presented)
        } else {
            update.endCommitPresent?(.failed)
            settle(update.commit.settlement, .nativeUpdateFailed)
        }
        state = .idle
        notifyReady()
    }

    private func metadata(for commit: Commit) -> TranscriptRenderTransactionMetadata {
        return TranscriptRenderTransactionMetadata(
            transactionID: commit.transactionID,
            insertions: commit.collectionMutation.insertions.count,
            deletions: commit.collectionMutation.deletions.count,
            moves: commit.collectionMutation.moves.count
        )
    }

    private func notifyReady() {
        guard case .idle = state else { return }
        ready()
    }
}
