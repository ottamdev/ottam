import AppKit
import OttamInstrumentation
import OSLog

struct TranscriptViewportAnchor {
    let id: TranscriptBlockID
    let relativeY: CGFloat
}

enum TranscriptViewportTarget {
    case bottom
    case anchor(TranscriptViewportAnchor?)
    case offset(CGFloat)
    case unchanged
    case deferred

    var preservesBottom: Bool {
        if case .bottom = self { return true }
        return false
    }
}

/// Owns the imperative AppKit half of a geometry commit. Exact transcript
/// frames are published first, the final viewport is chosen second, and only
/// then may AppKit materialize collection items for that viewport.
@MainActor
final class TranscriptLayoutCoordinator {
    struct RefreshOutcome {
        let refreshedItems: Int
        let selectionRefreshed: Bool

        static let none = Self(refreshedItems: 0, selectionRefreshed: false)
    }

    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "TranscriptLayout"
    )

    private unowned let scrollView: NSScrollView
    private unowned let collectionView: NSCollectionView
    private let layout: TranscriptCollectionLayout
    #if DEBUG
    private let debugSkipsExplicitScrollReflection =
        ProcessInfo.processInfo.environment[
            "OTTAM_DEBUG_TRANSCRIPT_SKIP_EXPLICIT_SCROLL_REFLECTION"
        ] == "1"
    private let debugForcesVisibleRedraw =
        ProcessInfo.processInfo.environment[
            "OTTAM_DEBUG_TRANSCRIPT_FORCE_VISIBLE_REDRAW"
        ] == "1"
    #else
    private let debugSkipsExplicitScrollReflection = false
    private let debugForcesVisibleRedraw = false
    #endif
    private var debugLoggedInitialClipOrigin = false

    private(set) var isProgrammaticScroll = false
    private(set) var synchronousMaterializationCount = 0

    init(
        scrollView: NSScrollView,
        collectionView: NSCollectionView,
        layout: TranscriptCollectionLayout
    ) {
        self.scrollView = scrollView
        self.collectionView = collectionView
        self.layout = layout
    }

    /// Publish exact final geometry and move a following viewport before the
    /// native collection mutation changes which rows are materialized. The
    /// completion commit still performs the authoritative materialization.
    func prepareForNativeMutation(
        transactionID: UInt64,
        target: TranscriptViewportTarget
    ) {
        guard target.preservesBottom else { return }
        synchronizeColumnWidth(transactionID: transactionID)
        layout.prepare()
        publishCollectionFrame(
            transactionID: transactionID,
            requestedSize: NSSize(
                width: max(1, scrollView.contentView.bounds.width),
                height: max(1, layout.collectionViewContentSize.height)
            )
        )
        scroll(
            to: constrainedOffsetY(towardBottom: true),
            transactionID: transactionID,
            target: "native_mutation_bottom"
        )
    }

    func commit(
        transactionID: UInt64,
        forceFull: Bool = false,
        target: TranscriptViewportTarget,
        refreshMode: String = "none",
        requestedRefreshItems: Int = 0,
        refresh: @escaping () -> RefreshOutcome = { .none }
    ) {
        let window = collectionView.window
        if TranscriptSignpost.isEnabled {
            TranscriptSignpost.collectionEnvironment(
                transactionID: transactionID,
                appActive: NSApp.isActive,
                keyWindow: window?.isKeyWindow == true,
                windowVisible: window?.occlusionState.contains(.visible) == true,
                thermalState: ProcessInfo.processInfo.thermalState.signpostName,
                threadQoS: Thread.current.qualityOfService.signpostName
            )
        }
        let interval = TranscriptSignpost.beginLayoutCommit(
            transactionID: transactionID
        )
        defer {
            TranscriptSignpost.endLayoutCommit(
                interval,
                viewportTarget: target.signpostName,
                materialization: target.isDeferred ? "deferred" : "immediate",
                synchronousMaterializationCount: synchronousMaterializationCount
            )
        }
        let changes = { [self] in
            synchronizeColumnWidth(transactionID: transactionID)
            if forceFull { layout.invalidateLayout() }
            layout.prepare()
            publishCollectionFrame(
                transactionID: transactionID,
                requestedSize: NSSize(
                    width: max(1, scrollView.contentView.bounds.width),
                    height: max(1, layout.collectionViewContentSize.height)
                )
            )

            switch target {
            case .bottom:
                scroll(
                    to: constrainedOffsetY(towardBottom: true),
                    transactionID: transactionID,
                    target: target.signpostName
                )
            case .anchor(let anchor):
                if let anchor, let frame = layout.frame(for: anchor.id) {
                    scroll(
                        to: frame.minY - anchor.relativeY,
                        transactionID: transactionID,
                        target: target.signpostName
                    )
                }
            case .offset(let value):
                scroll(
                    to: value,
                    transactionID: transactionID,
                    target: target.signpostName
                )
            case .unchanged, .deferred:
                break
            }

            if case .deferred = target {
                collectionView.needsLayout = true
            } else {
                let neededLayoutBefore = collectionView.needsLayout
                let visibleBefore = TranscriptSignpost.isEnabled
                    ? Set(collectionView.indexPathsForVisibleItems())
                    : []
                let materializationInterval = TranscriptSignpost.beginCollectionMaterialization(
                    transactionID: transactionID,
                    neededLayoutBefore: neededLayoutBefore,
                    visibleItemsBefore: visibleBefore.count
                )
                collectionView.layoutSubtreeIfNeeded()
                let visibleAfter = TranscriptSignpost.isEnabled
                    ? Set(collectionView.indexPathsForVisibleItems())
                    : []
                TranscriptSignpost.endCollectionMaterialization(
                    materializationInterval,
                    payload: .calculate(
                        neededLayoutBefore: neededLayoutBefore,
                        visibleBefore: visibleBefore,
                        visibleAfter: visibleAfter
                    )
                )
                synchronousMaterializationCount += 1
                let refreshInterval = TranscriptSignpost.beginCollectionRefresh(
                    transactionID: transactionID,
                    mode: refreshMode,
                    requestedItems: requestedRefreshItems
                )
                let refreshOutcome = refresh()
                TranscriptSignpost.endCollectionRefresh(
                    refreshInterval,
                    refreshedItems: refreshOutcome.refreshedItems,
                    selectionRefreshed: refreshOutcome.selectionRefreshed
                )
            }
        }

        changes()
    }

    private func synchronizeColumnWidth(transactionID: UInt64) {
        let width = max(1, scrollView.contentView.bounds.width)
        guard abs(collectionView.bounds.width - width) > 0.5 else { return }
        publishCollectionFrame(transactionID: transactionID,
                               requestedSize: NSSize(width: width, height: max(1, collectionView.frame.height)))
        layout.invalidateLayout()
    }

    private func publishCollectionFrame(
        transactionID: UInt64,
        requestedSize: NSSize
    ) {
        let previousSize = collectionView.frame.size
        let interval = TranscriptSignpost.beginCollectionFramePublication(
            transactionID: transactionID
        )
        collectionView.setFrameSize(requestedSize)
        let actualSize = collectionView.frame.size
        TranscriptSignpost.endCollectionFramePublication(
            interval,
            payload: .calculate(
                currentWidth: Double(previousSize.width),
                currentHeight: Double(previousSize.height),
                requestedWidth: Double(requestedSize.width),
                requestedHeight: Double(requestedSize.height),
                actualWidth: Double(actualSize.width),
                actualHeight: Double(actualSize.height)
            )
        )
    }

    func constrainedOffsetY(towardBottom: Bool) -> CGFloat {
        let clipView = scrollView.contentView
        let documentFrame = scrollView.documentView?.frame ?? .zero
        let proposedY: CGFloat
        if towardBottom {
            proposedY = documentFrame.maxY + clipView.contentInsets.bottom + clipView.bounds.height
        } else {
            proposedY = documentFrame.minY - clipView.contentInsets.top - clipView.bounds.height
        }
        return clipView.constrainBoundsRect(NSRect(
            x: clipView.bounds.minX,
            y: proposedY,
            width: clipView.bounds.width,
            height: clipView.bounds.height
        )).minY
    }

    private func scroll(
        to proposedY: CGFloat,
        transactionID: UInt64,
        target: String
    ) {
        let clipView = scrollView.contentView
        let priorX = clipView.bounds.minX
        let priorY = clipView.bounds.minY
        let proposed = NSRect(
            x: priorX,
            y: proposedY,
            width: clipView.bounds.width,
            height: clipView.bounds.height
        )
        let constrainedOrigin = clipView.constrainBoundsRect(proposed).origin
        let interval = TranscriptSignpost.beginCollectionScroll(
            transactionID: transactionID,
            target: target
        )
        isProgrammaticScroll = true
        clipView.scroll(to: constrainedOrigin)
        let reflected = !debugSkipsExplicitScrollReflection
        if !debugSkipsExplicitScrollReflection {
            scrollView.reflectScrolledClipView(clipView)
        }
        TranscriptSignpost.endCollectionScroll(
            interval,
            payload: .calculate(
                priorY: Double(priorY),
                proposedY: Double(proposedY),
                constrainedY: Double(constrainedOrigin.y),
                actualY: Double(clipView.bounds.minY)
            ),
            reflected: reflected
        )
        if debugForcesVisibleRedraw {
            let nextX = clipView.bounds.minX
            collectionView.setNeedsDisplay(clipView.documentVisibleRect)
            if !debugLoggedInitialClipOrigin {
                debugLoggedInitialClipOrigin = true
                Self.logger.notice(
                    "forced visible transcript redraw enabled clipX=\(nextX, privacy: .public)"
                )
            }
            if abs(nextX - priorX) > 0.01 {
                Self.logger.warning(
                    "transcript clip horizontal origin drifted before=\(priorX, privacy: .public) after=\(nextX, privacy: .public)"
                )
            }
        }
        isProgrammaticScroll = false
    }
}

private extension ProcessInfo.ThermalState {
    var signpostName: String {
        switch self {
        case .nominal: "nominal"
        case .fair: "fair"
        case .serious: "serious"
        case .critical: "critical"
        @unknown default: "unknown"
        }
    }
}

private extension QualityOfService {
    var signpostName: String {
        switch self {
        case .userInteractive: "user_interactive"
        case .userInitiated: "user_initiated"
        case .default: "default"
        case .utility: "utility"
        case .background: "background"
        @unknown default: "unknown"
        }
    }
}

private extension TranscriptViewportTarget {
    var isDeferred: Bool {
        if case .deferred = self { return true }
        return false
    }

    var signpostName: String {
        switch self {
        case .bottom: "bottom"
        case .anchor: "anchor"
        case .offset: "offset"
        case .unchanged: "unchanged"
        case .deferred: "deferred"
        }
    }
}
