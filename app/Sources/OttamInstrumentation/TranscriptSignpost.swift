import Foundation
import QuartzCore
import os

/// The signpost schema for transcript-rendering measurements.
///
/// Always compiled, never `#if DEBUG`-gated: an `OSSignposter` costs a load, a
/// branch and a ring-buffer write when nothing is listening, and keeping it in
/// optimized builds is the point — the measurement protocol runs the `profile`
/// configuration, and a shipping build stays traceable if a regression only
/// reproduces in the field.
///
/// **Placement is the contract, not the names.** Cross-renderer measurements
/// live at stable seams such as display-link tick, assembly and
/// parsing. Renderer-specific intervals provide attribution for the current
/// native path but are not interchangeable with historical baseline seams.
///
/// **These do not replace `sample`.** A signpost captures framework work only
/// when that work is synchronous inside the interval. AppKit may schedule
/// layout, materialization and presentation after a caller returns, so those
/// phases have explicit intervals and completion barriers. Signposts supply
/// counts, distributions and outliers; the CPU number still comes from
/// `sample` + `ps`.
///
/// Schema and procedure: `docs/operations/performance/transcript-renderer.md`
public enum TranscriptSignpost {
    public static let subsystem = "dev.ottam.Ottam"

    public static let signposter = OSSignposter(
        subsystem: subsystem,
        category: "transcript-render"
    )

    /// Whether anything is currently listening. Guard expensive *metadata*
    /// construction with this — never the measured work itself, or the profile
    /// stops describing the shipping path.
    public static var isEnabled: Bool { signposter.isEnabled }

    public struct DisplayTickPayload: Equatable, Sendable {
        public let configuredHz: Int
        public let physicalHz: Int
        public let callbackSpacingMilliseconds: Double
        public let elapsedSlots: Int
        public let missedSlots: Int
        public let pendingBytes: Int
        public let pendingDeltas: Int

        public static func calculate(
            configuredHz: Int,
            physicalHz: Int,
            timestamp: TimeInterval,
            previousTimestamp: TimeInterval?,
            pendingBytes: Int,
            pendingDeltas: Int
        ) -> Self {
            let rate = max(1, configuredHz)
            let spacing = previousTimestamp.map { max(0, timestamp - $0) } ?? 0
            let elapsedSlots = previousTimestamp == nil
                ? 1
                : max(1, Int((spacing * Double(rate)).rounded()))
            return Self(
                configuredHz: rate,
                physicalHz: max(1, physicalHz),
                callbackSpacingMilliseconds: spacing * 1_000,
                elapsedSlots: elapsedSlots,
                missedSlots: max(0, elapsedSlots - 1),
                pendingBytes: max(0, pendingBytes),
                pendingDeltas: max(0, pendingDeltas)
            )
        }
    }



    public enum CollectionCompletion: String, Sendable {
        case succeeded
        case failed
        case tornDown = "torn_down"
    }

    public enum PresentationProxy: String, Sendable {
        case caTransaction = "ca_transaction"
        case rendererFailed = "renderer_failed"
        case teardown
    }

    public enum CollectionInvalidationScope: String, Sendable {
        case full
        case suffix
        case item
        case viewport
        case offset
    }

    public struct CollectionAttributesPayload: Equatable, Sendable {
        public let requestedHeight: Double
        public let expandedHeight: Double
        public let intersectingItems: Int
        public let returnedItems: Int

        public static func calculate(
            requestedRect: CGRect,
            expandedRect: CGRect,
            itemFrames: [CGRect]
        ) -> Self {
            Self(
                requestedHeight: Double(requestedRect.height),
                expandedHeight: Double(expandedRect.height),
                intersectingItems: itemFrames.lazy.filter {
                    $0.intersects(requestedRect)
                }.count,
                returnedItems: itemFrames.count
            )
        }
    }

    public struct CollectionFramePayload: Equatable, Sendable {
        public let requestedWidthDelta: Double
        public let requestedHeightDelta: Double
        public let actualWidthDelta: Double
        public let actualHeightDelta: Double
        public let changed: Bool

        public static func calculate(
            currentWidth: Double,
            currentHeight: Double,
            requestedWidth: Double,
            requestedHeight: Double,
            actualWidth: Double,
            actualHeight: Double,
            movementThreshold: Double = 0.01
        ) -> Self {
            let actualWidthDelta = actualWidth - currentWidth
            let actualHeightDelta = actualHeight - currentHeight
            return Self(
                requestedWidthDelta: requestedWidth - currentWidth,
                requestedHeightDelta: requestedHeight - currentHeight,
                actualWidthDelta: actualWidthDelta,
                actualHeightDelta: actualHeightDelta,
                changed: abs(actualWidthDelta) > movementThreshold
                    || abs(actualHeightDelta) > movementThreshold
            )
        }
    }

    public struct CollectionScrollPayload: Equatable, Sendable {
        public let requestedDeltaY: Double
        public let constrainedDeltaY: Double
        public let actualDeltaY: Double
        public let moved: Bool

        public static func calculate(
            priorY: Double,
            proposedY: Double,
            constrainedY: Double,
            actualY: Double,
            movementThreshold: Double = 0.01
        ) -> Self {
            let actualDeltaY = actualY - priorY
            return Self(
                requestedDeltaY: proposedY - priorY,
                constrainedDeltaY: constrainedY - priorY,
                actualDeltaY: actualDeltaY,
                moved: abs(actualDeltaY) > movementThreshold
            )
        }
    }

    public struct CollectionMaterializationPayload: Equatable, Sendable {
        public let neededLayoutBefore: Bool
        public let visibleItemsBefore: Int
        public let visibleItemsAfter: Int
        public let appearedItems: Int
        public let disappearedItems: Int
        public let visibleSetChanged: Bool

        public static func calculate(
            neededLayoutBefore: Bool,
            visibleBefore: Set<IndexPath>,
            visibleAfter: Set<IndexPath>
        ) -> Self {
            let appeared = visibleAfter.subtracting(visibleBefore).count
            let disappeared = visibleBefore.subtracting(visibleAfter).count
            return Self(
                neededLayoutBefore: neededLayoutBefore,
                visibleItemsBefore: visibleBefore.count,
                visibleItemsAfter: visibleAfter.count,
                appearedItems: appeared,
                disappearedItems: disappeared,
                visibleSetChanged: appeared > 0 || disappeared > 0
            )
        }
    }

    // MARK: - Stream commit

    /// One display-link drain: the buffered deltas that become one screen
    /// commit.
    ///
    /// The gate this serves is "commit count follows the selected display rate
    /// and remains substantially independent of delta rate" — which is a
    /// question about counts and their spacing, so the interval must bracket
    /// exactly one tick's work and nothing else.
    public static func streamCommit<T>(
        deltas: Int,
        bytes: Int,
        _ body: () throws -> T
    ) rethrows -> T {
        let state = signposter.beginInterval(
            "stream.commit",
            id: signposter.makeSignpostID(),
            "deltas=\(deltas) bytes=\(bytes)"
        )
        defer { signposter.endInterval("stream.commit", state) }
        return try body()
    }

    // MARK: - Display scheduling

    public static func normalizedPublication(generation: UInt64, succeeded: Bool) {
        signposter.emitEvent("normalized.publication", "generation=\(generation) succeeded=\(succeeded)")
    }

    public static func displayTick(_ payload: DisplayTickPayload) {
        signposter.emitEvent(
            "display.tick",
            "configured_hz=\(payload.configuredHz) physical_hz=\(payload.physicalHz) spacing_ms=\(payload.callbackSpacingMilliseconds) elapsed_slots=\(payload.elapsedSlots) missed_slots=\(payload.missedSlots) pending_bytes=\(payload.pendingBytes) pending_deltas=\(payload.pendingDeltas)"
        )
    }



    // MARK: - Assembly

    public struct IntervalToken {
        let name: StaticString
        let state: OSSignpostIntervalState
    }

    // MARK: - Serialized renderer transaction

    public static func beginRenderPrepare(
        transactionID: UInt64
    ) -> IntervalToken {
        IntervalToken(
            name: "render.prepare",
            state: signposter.beginInterval(
                "render.prepare",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID)"
            )
        )
    }

    public static func endRenderPrepare(
        _ interval: IntervalToken,
        projectedBlocks: Int,
        artifactCount: Int
    ) {
        signposter.endInterval(
            "render.prepare",
            interval.state,
            "projected_blocks=\(projectedBlocks) artifacts=\(artifactCount)"
        )
    }

    public static func beginCollectionUpdate(
        transactionID: UInt64,
        insertions: Int,
        deletions: Int,
        moves: Int
    ) -> IntervalToken {
        IntervalToken(
            name: "collection.update",
            state: signposter.beginInterval(
                "collection.update",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID) insertions=\(insertions) deletions=\(deletions) moves=\(moves)"
            )
        )
    }

    public static func endCollectionUpdate(
        _ interval: IntervalToken,
        completion: CollectionCompletion
    ) {
        signposter.endInterval(
            "collection.update",
            interval.state,
            "result=\(completion.rawValue, privacy: .public)"
        )
    }

    public static func collectionApply<T>(
        transactionID: UInt64,
        _ body: () throws -> T
    ) rethrows -> T {
        let state = signposter.beginInterval(
            "collection.apply",
            id: signposter.makeSignpostID(),
            "transaction=\(transactionID)"
        )
        defer { signposter.endInterval("collection.apply", state) }
        return try body()
    }

    public static func collectionMutation<T>(
        transactionID: UInt64,
        _ body: () throws -> T
    ) rethrows -> T {
        let state = signposter.beginInterval(
            "collection.mutation",
            id: signposter.makeSignpostID(),
            "transaction=\(transactionID)"
        )
        defer { signposter.endInterval("collection.mutation", state) }
        return try body()
    }

    public static func beginLayoutCommit(
        transactionID: UInt64
    ) -> IntervalToken {
        IntervalToken(
            name: "layout.commit",
            state: signposter.beginInterval(
                "layout.commit",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID)"
            )
        )
    }

    public static func endLayoutCommit(
        _ interval: IntervalToken,
        viewportTarget: String,
        materialization: String,
        synchronousMaterializationCount: Int
    ) {
        signposter.endInterval(
            "layout.commit",
            interval.state,
            "viewport=\(viewportTarget, privacy: .public) materialization=\(materialization, privacy: .public) synchronous_count=\(synchronousMaterializationCount)"
        )
    }

    // MARK: - Native collection layout

    public static func collectionInvalidation(
        scope: CollectionInvalidationScope,
        invalidatedItems: Int,
        dirtyStart: Int?,
        origin: String = "unspecified",
        invalidateEverything: Bool = false,
        invalidateDataSourceCounts: Bool = false,
        updateBracketID: UInt64? = nil,
        contentSizeDelta: Double = 0,
        contentOffsetDelta: Double = 0
    ) {
        signposter.emitEvent(
            "collection.invalidation",
            "scope=\(scope.rawValue, privacy: .public) origin=\(origin, privacy: .public) invalidated_items=\(max(0, invalidatedItems)) dirty_start=\(dirtyStart ?? -1) invalidate_everything=\(invalidateEverything ? 1 : 0) invalidate_data_source_counts=\(invalidateDataSourceCounts ? 1 : 0) update_bracket=\(updateBracketID ?? 0) content_size_delta=\(contentSizeDelta) content_offset_delta=\(contentOffsetDelta)"
        )
    }

    public static func collectionUpdateLifecycle(
        phase: String,
        bracketID: UInt64,
        updateItems: Int,
        earliestAffectedIndex: Int?,
        insertions: Int,
        deletions: Int,
        reloads: Int,
        moves: Int
    ) {
        signposter.emitEvent(
            "collection.update-lifecycle",
            "phase=\(phase, privacy: .public) bracket=\(bracketID) update_items=\(max(0, updateItems)) earliest_affected=\(earliestAffectedIndex ?? -1) insertions=\(max(0, insertions)) deletions=\(max(0, deletions)) reloads=\(max(0, reloads)) moves=\(max(0, moves))"
        )
    }

    public static func collectionEnvironment(
        transactionID: UInt64,
        appActive: Bool,
        keyWindow: Bool,
        windowVisible: Bool,
        thermalState: String,
        threadQoS: String
    ) {
        signposter.emitEvent(
            "collection.environment",
            "transaction=\(transactionID) app_active=\(appActive ? 1 : 0) key_window=\(keyWindow ? 1 : 0) window_visible=\(windowVisible ? 1 : 0) thermal_state=\(thermalState, privacy: .public) thread_qos=\(threadQoS, privacy: .public)"
        )
    }

    public static func beginCollectionPrepare(totalItems: Int) -> IntervalToken {
        IntervalToken(
            name: "collection.prepare",
            state: signposter.beginInterval(
                "collection.prepare",
                id: signposter.makeSignpostID(),
                "total_items=\(max(0, totalItems))"
            )
        )
    }

    public static func endCollectionPrepare(
        _ interval: IntervalToken,
        rebuiltItems: Int
    ) {
        signposter.endInterval(
            "collection.prepare",
            interval.state,
            "rebuilt_items=\(max(0, rebuiltItems))"
        )
    }

    public static func beginCollectionAttributesQuery(
        requestedMinY: Double,
        requestedHeight: Double,
        expandedMinY: Double,
        expandedHeight: Double
    ) -> IntervalToken {
        IntervalToken(
            name: "collection.attributes",
            state: signposter.beginInterval(
                "collection.attributes",
                id: signposter.makeSignpostID(),
                "requested_min_y=\(requestedMinY) requested_height=\(requestedHeight) expanded_min_y=\(expandedMinY) expanded_height=\(expandedHeight)"
            )
        )
    }

    public static func endCollectionAttributesQuery(
        _ interval: IntervalToken,
        intersectingItems: Int,
        returnedItems: Int
    ) {
        signposter.endInterval(
            "collection.attributes",
            interval.state,
            "intersecting_items=\(max(0, intersectingItems)) returned_items=\(max(0, returnedItems))"
        )
    }

    public static func collectionClipBoundsChanged(deltaY: Double) {
        signposter.emitEvent(
            "collection.clip-bounds",
            "delta_y=\(deltaY)"
        )
    }

    public static func beginCollectionFramePublication(
        transactionID: UInt64
    ) -> IntervalToken {
        IntervalToken(
            name: "collection.frame",
            state: signposter.beginInterval(
                "collection.frame",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID)"
            )
        )
    }

    public static func endCollectionFramePublication(
        _ interval: IntervalToken,
        payload: CollectionFramePayload
    ) {
        signposter.endInterval(
            "collection.frame",
            interval.state,
            "requested_width_delta=\(payload.requestedWidthDelta) requested_height_delta=\(payload.requestedHeightDelta) actual_width_delta=\(payload.actualWidthDelta) actual_height_delta=\(payload.actualHeightDelta) changed=\(payload.changed ? 1 : 0)"
        )
    }

    public static func beginCollectionScroll(
        transactionID: UInt64,
        target: String
    ) -> IntervalToken {
        IntervalToken(
            name: "collection.scroll",
            state: signposter.beginInterval(
                "collection.scroll",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID) target=\(target, privacy: .public)"
            )
        )
    }

    public static func endCollectionScroll(
        _ interval: IntervalToken,
        payload: CollectionScrollPayload,
        reflected: Bool
    ) {
        signposter.endInterval(
            "collection.scroll",
            interval.state,
            "requested_delta_y=\(payload.requestedDeltaY) constrained_delta_y=\(payload.constrainedDeltaY) actual_delta_y=\(payload.actualDeltaY) moved=\(payload.moved ? 1 : 0) reflected=\(reflected ? 1 : 0)"
        )
    }

    public static func beginCollectionMaterialization(
        transactionID: UInt64,
        neededLayoutBefore: Bool,
        visibleItemsBefore: Int
    ) -> IntervalToken {
        IntervalToken(
            name: "collection.materialize",
            state: signposter.beginInterval(
                "collection.materialize",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID) needed_layout_before=\(neededLayoutBefore ? 1 : 0) visible_before=\(max(0, visibleItemsBefore))"
            )
        )
    }

    public static func endCollectionMaterialization(
        _ interval: IntervalToken,
        payload: CollectionMaterializationPayload
    ) {
        signposter.endInterval(
            "collection.materialize",
            interval.state,
            "visible_after=\(payload.visibleItemsAfter) appeared=\(payload.appearedItems) disappeared=\(payload.disappearedItems) visible_set_changed=\(payload.visibleSetChanged ? 1 : 0)"
        )
    }

    public static func beginCollectionRefresh(
        transactionID: UInt64,
        mode: String,
        requestedItems: Int
    ) -> IntervalToken {
        IntervalToken(
            name: "collection.refresh",
            state: signposter.beginInterval(
                "collection.refresh",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID) mode=\(mode, privacy: .public) requested_items=\(max(0, requestedItems))"
            )
        )
    }

    public static func endCollectionRefresh(
        _ interval: IntervalToken,
        refreshedItems: Int,
        selectionRefreshed: Bool
    ) {
        signposter.endInterval(
            "collection.refresh",
            interval.state,
            "refreshed_items=\(max(0, refreshedItems)) selection_refreshed=\(selectionRefreshed ? 1 : 0)"
        )
    }

    /// Snapshot to `[TranscriptItem]`. The item count is only known at the end,
    /// so this is a begin/end pair rather than a scoped closure.
    public static func beginAssembly() -> IntervalToken {
        IntervalToken(
            name: "assembly",
            state: signposter.beginInterval("assembly", id: signposter.makeSignpostID())
        )
    }

    public static func endAssembly(_ token: IntervalToken, itemCount: Int) {
        signposter.endInterval("assembly", token.state, "items=\(itemCount)")
    }

    // MARK: - Markdown parse

    /// Parsing one item. Keyed by item identity and content revision so a
    /// reparse of the same revision — which the design forbids — is visible as
    /// a duplicate rather than hiding inside an aggregate.
    public static func markdownParse<T>(
        itemID: String,
        revision: UInt64,
        _ body: () throws -> T
    ) rethrows -> T {
        let state = signposter.beginInterval(
            "markdown.parse",
            id: signposter.makeSignpostID(),
            "item=\(itemID, privacy: .public) rev=\(revision)"
        )
        defer { signposter.endInterval("markdown.parse", state) }
        return try body()
    }

    /// Parsing at the shared parser, which knows text but not transcript
    /// identity. Keyed by size and syntax mode so parse cost can be related to
    /// input without the caller's vocabulary leaking into a renderer-neutral
    /// type.
    public static func markdownParse<T>(
        bytes: Int,
        syntax: StaticString,
        _ body: () throws -> T
    ) rethrows -> T {
        let state = signposter.beginInterval(
            "markdown.parse",
            id: signposter.makeSignpostID(),
            "bytes=\(bytes) syntax=\(syntax, privacy: .public)"
        )
        defer { signposter.endInterval("markdown.parse", state) }
        return try body()
    }

    // MARK: - Layout

    public static func layout<T>(_ body: () throws -> T) rethrows -> T {
        let state = signposter.beginInterval("layout", id: signposter.makeSignpostID())
        defer { signposter.endInterval("layout", state) }
        return try body()
    }

    // MARK: - Events

    public enum ScrollCorrection: String {
        case pin
        case anchor
        case prepend
        case restore
    }

    /// A programmatic scroll correction. Distinguishing these from user detach
    /// is what keeps a geometry/update feedback loop visible in a trace.
    public static func scrollCorrection(_ reason: ScrollCorrection) {
        signposter.emitEvent("scroll.correct", "reason=\(reason.rawValue, privacy: .public)")
    }

    /// The reader attached to, or detached from, the end of the transcript.
    ///
    /// Detached streaming is a separate cost profile — no bottom-pinning work
    /// per commit — so a measurement run has to know which regime it was in.
    /// Reading it from pixels does not work: a detached viewport and a pinned
    /// one can look alike mid-stream, and the surface silently re-pins whenever
    /// the viewport returns to the bottom.
    public static func followsBottom(_ following: Bool) {
        signposter.emitEvent("scroll.follows", "following=\(following, privacy: .public)")
    }

    // MARK: - Commit to present

    /// Wall time from native submission to a Core Animation completion proxy.
    ///
    /// This one straddles a frame boundary rather than a function call, so it
    /// cannot be a scoped interval: the work is handed to the render server and
    /// finishes after the calling function has returned. `CATransaction`'s
    /// completion block is a proxy; it does not establish physical screen timing.
    ///
    /// Call this *inside* the same main-actor turn that mutates the surface, or
    /// the transaction it attaches to is not the one carrying the change.
    public static func beginCommitPresent(transactionID: UInt64) -> IntervalToken {
        IntervalToken(
            name: "commit.present",
            state: signposter.beginInterval(
                "commit.present",
                id: signposter.makeSignpostID(),
                "transaction=\(transactionID)"
            )
        )
    }

    public static func endCommitPresent(
        _ interval: IntervalToken,
        proxy: PresentationProxy
    ) {
        signposter.endInterval(
            "commit.present",
            interval.state,
            "proxy=\(proxy.rawValue, privacy: .public)"
        )
    }

    /// Wraps the transaction carrying the mutation. Both native success and CA
    /// completion are required; this is a presentation proxy, not screen timing.
    @MainActor
    public final class CommitPresentation {
        private let interval: IntervalToken
        private var nativeResult: CollectionCompletion?
        private var transactionCompleted = false
        private var ended = false

        public init(transactionID: UInt64) {
            interval = beginCommitPresent(transactionID: transactionID)
            CATransaction.begin()
        }

        public func commitTransaction() { CATransaction.commit() }

        public func finish(_ result: CollectionCompletion) {
            guard nativeResult == nil else { return }
            nativeResult = result
            if case .succeeded = result {
                CATransaction.setCompletionBlock { [self] in
                    transactionCompleted = true
                    endIfReady()
                }
            }
            endIfReady()
        }

        private func endIfReady() {
            guard !ended, let nativeResult else { return }
            let proxy: PresentationProxy
            switch nativeResult {
            case .succeeded:
                guard transactionCompleted else { return }
                proxy = .caTransaction
            case .failed: proxy = .rendererFailed
            case .tornDown: proxy = .teardown
            }
            ended = true
            endCommitPresent(interval, proxy: proxy)
        }
    }
}
