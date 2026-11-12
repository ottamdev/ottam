import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamInstrumentation
import OttamMarkdownEngine
import SwiftUI

@MainActor
private final class TranscriptScrollView: NSScrollView {
    var onLiveResizeEnded: () -> Void = {}
    var onEffectiveAppearanceChanged: () -> Void = {}

    // No `scrollWheel(with:)` override here: an NSScrollView subclass that
    // overrides it opts out of responsive scrolling, and the wheel event is
    // already visible through `NSApp.currentEvent` where provenance is checked.

    override func viewDidEndLiveResize() {
        super.viewDidEndLiveResize()
        onLiveResizeEnded()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        onEffectiveAppearanceChanged()
    }
}

@MainActor
private final class TranscriptCollectionView: NSCollectionView, NSUserInterfaceValidations {
    var selectedText: () -> String? = { nil }
    var copySelection: (String) -> Void = { WorkspacePasteboard.copy($0) }

    override class var isCompatibleWithResponsiveScrolling: Bool {
        #if DEBUG
        if ProcessInfo.processInfo.environment[
            "OTTAM_DEBUG_TRANSCRIPT_DISABLE_RESPONSIVE_SCROLLING"
        ] == "1" {
            return false
        }
        #endif
        return super.isCompatibleWithResponsiveScrolling
    }

    override var acceptsFirstResponder: Bool { true }

    /// The text view tracking a press that began on empty space.
    private var forwardedSelectionView: NSView?

    /// The transcript reads as one page of text, as in Notes or TextEdit: a
    /// press in a gap, a gutter, or beside a bubble starts the selection at
    /// the nearest text, which handles it as if pressed directly. Cards keep
    /// their own clicks. Dragging from there already extends anywhere.
    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        guard let target = nearestTextItem(to: point)?.selectionTrackingView else {
            super.mouseDown(with: event)
            return
        }
        forwardedSelectionView = target
        target.mouseDown(with: event)
    }

    override func mouseDragged(with event: NSEvent) {
        guard let forwardedSelectionView else { return super.mouseDragged(with: event) }
        forwardedSelectionView.mouseDragged(with: event)
    }

    override func mouseUp(with event: NSEvent) {
        guard let forwardedSelectionView else { return super.mouseUp(with: event) }
        self.forwardedSelectionView = nil
        forwardedSelectionView.mouseUp(with: event)
    }

    /// The visible text item closest to `point`, vertically first; nil when
    /// the press is on a card or no text is in view.
    private func nearestTextItem(to point: NSPoint) -> TranscriptTextCollectionItem? {
        if let indexPath = indexPathForItem(at: point), !(item(at: indexPath) is TranscriptTextCollectionItem) {
            return nil
        }
        return visibleItems()
            .compactMap { $0 as? TranscriptTextCollectionItem }
            .min { lhs, rhs in
                Self.distance(from: point, to: lhs.view.frame) < Self.distance(from: point, to: rhs.view.frame)
            }
    }

    private static func distance(from point: NSPoint, to frame: NSRect) -> (CGFloat, CGFloat) {
        let vertical = point.y < frame.minY ? frame.minY - point.y : max(0, point.y - frame.maxY)
        let horizontal = point.x < frame.minX ? frame.minX - point.x : max(0, point.x - frame.maxX)
        return (vertical, horizontal)
    }

    @IBAction func copy(_ sender: Any?) {
        guard let text = selectedText(), !text.isEmpty else { return }
        copySelection(text)
    }

    var requestQuoteReply: () -> Void = {}
    /// Whether this transcript's host takes replies; the Plan tab does not yet.
    var acceptsQuoteReply: () -> Bool = { false }

    @IBAction func replyToSelection(_ sender: Any?) {
        guard acceptsQuoteReply(), selectedText() != nil else { return }
        requestQuoteReply()
    }

    /// Command-Shift-R, the shortcut the context menu's Reply… advertises.
    /// Handled here, with the transcript focused, rather than in the app menu:
    /// it only means something while a selection is in the transcript.
    /// AppKit offers key equivalents to every view in the window, so the
    /// focus check keeps the composer or a terminal's ⇧⌘R theirs.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if flags == [.command, .shift], event.charactersIgnoringModifiers?.lowercased() == "r",
           (window?.firstResponder as? NSView)?.isDescendant(of: self) == true,
           acceptsQuoteReply(), selectedText() != nil {
            replyToSelection(nil)
            return true
        }
        return super.performKeyEquivalent(with: event)
    }

    override func validRequestor(
        forSendType sendType: NSPasteboard.PasteboardType?,
        returnType: NSPasteboard.PasteboardType?
    ) -> Any? {
        if sendType == .string, returnType == nil, selectedText() != nil { return self }
        return super.validRequestor(forSendType: sendType, returnType: returnType)
    }

    func writeSelection(
        to pboard: NSPasteboard,
        types: [NSPasteboard.PasteboardType]
    ) -> Bool {
        guard types.contains(.string), let text = selectedText(), !text.isEmpty else { return false }
        pboard.clearContents()
        return pboard.setString(text, forType: .string)
    }

    func validateUserInterfaceItem(_ item: any NSValidatedUserInterfaceItem) -> Bool {
        if item.action == #selector(copy(_:)) { return selectedText() != nil }
        if item.action == #selector(replyToSelection(_:)) {
            return acceptsQuoteReply() && selectedText() != nil
        }
        return true
    }
}

extension TranscriptCollectionView: @MainActor NSServicesMenuRequestor {}
extension TranscriptCollectionView: TranscriptQuoteReplying {}

/// The only SwiftUI/AppKit boundary for the native transcript. AppKit owns the
/// complete transcript controller and SwiftUI supplies immutable presentation
/// inputs plus actions.
struct MacAgentTranscriptView: NSViewControllerRepresentable {
    @MainActor
    final class Controller {
        fileprivate weak var viewController: MacAgentTranscriptViewController?
        func scrollToBottom() { viewController?.scrollToBottom() }
        func scrollBack(viewports: Int) { viewController?.scrollBack(viewports: viewports) }
        func isFollowingBottomForUITest() -> Bool {
            viewController?.debugLayoutState().isFollowingBottom ?? false
        }
        func selectAndCopyForUITest() -> Bool {
            viewController?.selectAndCopyForUITest() ?? false
        }
        func resizeWindowForUITest() -> Bool {
            viewController?.resizeWindowForUITest() ?? false
        }
    }

    let snapshot: ChatTranscriptSnapshot
    let actions: ChatTranscriptActions
    let controller: Controller
    let presentation: ChatTranscriptPresentationCoordinator
    var transcriptStore: ChatTranscriptStore?
    /// Keeps each conversation's rendering, open groups and place across
    /// switches. Nil (the Subagent tab) opens every thread fresh.
    var viewSessions: (any TranscriptViewSessionKeeping)?
    var isLoadingOlderRows = false
    var onFollowsBottomChange: (Bool) -> Void = { _ in }
    var onNearTop: () -> Void = {}
    /// Takes quote replies on a finished selection; nil shows no reply surface.
    var quoteReply: TranscriptQuoteReplyConfiguration?
    /// Height of the composer floating over the transcript's bottom edge. Fed
    /// into the scroll view's safe area so its automatic content insets keep
    /// the last turn clear of it, the same route the titlebar inset takes.
    var bottomOverlayInset: CGFloat = 0
    /// The Subagent tab draws a second transcript; UI tests tell them apart.
    var accessibilityIdentifier = AccessibilityID.chatTranscript

    @Environment(\.appTypography) private var typography

    @MainActor
    final class Coordinator {
        weak var presentation: ChatTranscriptPresentationCoordinator?
        weak var transcriptStore: ChatTranscriptStore?
        var store: ChatTranscriptCardStore?
        var snapshot: ChatTranscriptSnapshot?
        var theme: MarkdownTheme?
        /// The open groups of the conversation being reconciled; cards built
        /// for it start from them and record their toggles.
        var disclosures: ChatConversationUIState?
        /// The conversation the card factory's actions are bound to.
        var cardsConversationID: ConversationID?
        // Cache the attempted input, not the completion result. Recovery belongs
        // to the presentation coordinator and must not reset on a SwiftUI update.
        func beginCanonicalPublication(
            snapshot: ChatTranscriptSnapshot,
            theme: MarkdownTheme
        ) {
            self.snapshot = snapshot
            self.theme = theme
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSViewController(context: Context) -> MacAgentTranscriptViewController {
        let viewController = MacAgentTranscriptViewController()
        viewController.view.setAccessibilityIdentifier(accessibilityIdentifier)
        controller.viewController = viewController
        update(viewController, coordinator: context.coordinator)
        return viewController
    }

    func updateNSViewController(_ viewController: MacAgentTranscriptViewController, context: Context) {
        controller.viewController = viewController
        if viewController.view.additionalSafeAreaInsets.bottom != bottomOverlayInset {
            viewController.view.additionalSafeAreaInsets.bottom = bottomOverlayInset
        }
        update(viewController, coordinator: context.coordinator)
    }

    /// The transcript fills whatever it is offered. Answering here keeps SwiftUI
    /// from measuring the AppKit view through a temporary Auto Layout engine
    /// (`measureMin:max:ideal:`) on every window constraint pass, which a nested
    /// hosted card triggers on each of its own updates.
    func sizeThatFits(
        _ proposal: ProposedViewSize,
        nsViewController: MacAgentTranscriptViewController,
        context: Context
    ) -> CGSize? {
        // Unspecified dimensions ask for an ideal size; let SwiftUI measure
        // those rather than treating them as an explicit zero-sized proposal.
        guard let width = proposal.width, let height = proposal.height else { return nil }
        return CGSize(width: width, height: height)
    }

    static func dismantleNSViewController(
        _ viewController: MacAgentTranscriptViewController,
        coordinator: Coordinator
    ) {
        viewController.keepViewSession()
        viewController.tearDown()
        coordinator.transcriptStore?.publishLiveText = nil
        coordinator.presentation?.cancelPendingPresentation()
        coordinator.presentation = nil
    }

    private func update(
        _ viewController: MacAgentTranscriptViewController,
        coordinator: Coordinator,
        liveSnapshot: ChatTranscriptSnapshot? = nil
    ) {
        let snapshot = liveSnapshot ?? snapshot
        if coordinator.transcriptStore !== transcriptStore {
            // A switch hands the view another conversation's store; the one
            // left behind no longer draws its live text here.
            coordinator.transcriptStore?.publishLiveText = nil
        }
        coordinator.transcriptStore = transcriptStore
        viewController.viewSessions = viewSessions
        if let transcriptStore {
            viewController.updateHistoryPagination(cursor: transcriptStore.nextCursor, isLoading: isLoadingOlderRows)
        }
        transcriptStore?.publishLiveText = { [weak viewController, weak coordinator, weak transcriptStore] in
            guard let viewController, let coordinator, let transcriptStore,
                  var latest = coordinator.snapshot, latest.liveRun != nil,
                  latest.conversationID == transcriptStore.conversationID else { return false }
            latest.normalizedItems = transcriptStore.orderedItems
            latest.normalizedBodies = transcriptStore.bodies
            latest.bodyErrors = transcriptStore.bodyErrors
            update(viewController, coordinator: coordinator, liveSnapshot: latest)
            return true
        }
        let theme = MarkdownTheme.ottam(typography)
        viewController.onFollowsBottomChange = { following in
            TranscriptSignpost.followsBottom(following)
            Task { @MainActor in onFollowsBottomChange(following) }
        }
        viewController.onNearTop = onNearTop
        viewController.quoteReply = quoteReply
        viewController.onVisibleItemsChange = { [weak transcriptStore] ids in
            guard let read = actions.readBody else { return }
            Task { @MainActor in transcriptStore?.visibleBodies(ids, read: read) }
        }
        viewController.copyText = presentation.copyText(for:)
        viewController.copy = actions.copy
        viewController.openLink = actions.openFile

        let presentationChanged = coordinator.presentation !== presentation
        if presentationChanged { coordinator.presentation?.cancelPendingPresentation() }
        let inputChanged = presentationChanged
            || coordinator.snapshot != snapshot
            || coordinator.theme != theme
        coordinator.presentation = presentation
        guard inputChanged else { return }
        presentation.enqueue(snapshot: snapshot) { [viewController] plan, composedSnapshot, completion in
            let previousStore = coordinator.store
            // Actions are bound to one conversation; a cached switch keeps
            // this view, so its cards need the new conversation's.
            let conversationChanged = coordinator.cardsConversationID != composedSnapshot.conversationID
            if coordinator.theme != theme || previousStore == nil || conversationChanged {
                let factory = LiveChatTranscriptCardFactory(
                    theme: theme,
                    environment: HostedCardEnvironment(typography: typography),
                    actions: actions,
                    onPayloadChanged: { [weak viewController] card in
                        viewController?.cardPayloadDidChange(card)
                    },
                    // Weak: the coordinator owns this factory through its card store.
                    disclosures: { [weak owner = coordinator] in owner?.disclosures }
                )
                if let previousStore, coordinator.theme == theme {
                    previousStore.factory = factory
                } else {
                    coordinator.store = ChatTranscriptCardStore(factory: factory)
                }
                coordinator.cardsConversationID = composedSnapshot.conversationID
            }
            guard let store = coordinator.store else {
                completion.finish(false)
                return
            }
            coordinator.disclosures = composedSnapshot.conversationID.flatMap { viewSessions?.uiState(for: $0) }
            let reconciliation = store.reconcile(
                plan,
                inheritingStateFrom: coordinator.theme == theme ? nil : previousStore
            )
            assert(reconciliation.unmaterializedIDs.isEmpty, "every card spec has a conformer")
            coordinator.beginCanonicalPublication(
                snapshot: composedSnapshot,
                theme: theme
            )
            viewController.applyPreparingLargeSettledTranscript(
                plan: plan,
                cards: reconciliation.cards,
                conversationID: composedSnapshot.conversationID,
                theme: theme,
                resourceRoot: composedSnapshot.resourceRoot,
                permitsBackgroundPreparation: composedSnapshot.liveRun == nil,
                completion: completion
            )
        }
    }
}

@MainActor
final class MacAgentTranscriptViewController: NSViewController {
    enum Mode: Equatable {
        case followingBottom
        case detached
    }

    var onFollowsBottomChange: (Bool) -> Void = { _ in }
    var onNearTop: () -> Void = {}
    var onVisibleItemsChange: (Set<String>) -> Void = { _ in }
    var copyText: (String) -> String? = { _ in nil }
    var copy: (String) -> Void = { WorkspacePasteboard.copy($0) }
    /// Opens a clicked link; the host routes markdown files to a tab.
    var openLink: (URL) -> Void = { NSWorkspace.shared.open($0) }
    var quoteReply: TranscriptQuoteReplyConfiguration? {
        didSet { quoteReplyConfigurationDidChange() }
    }

    private let scrollView = TranscriptScrollView()
    /// Created on the first configuration, so a host that takes no replies
    /// carries no reply layer.
    private var quoteReplyPresenter: QuoteReplyPresenter?
    private let collectionView = TranscriptCollectionView()
    private let transcriptLayout = TranscriptCollectionLayout()
    private let suppliedBatchUpdater: TranscriptRenderTransactionCoordinator.BatchUpdater?
    private let suppliedSelectionAutoscroller: ((NSCollectionView, NSEvent) -> Bool)?
    private lazy var layoutCoordinator = TranscriptLayoutCoordinator(
        scrollView: scrollView,
        collectionView: collectionView,
        layout: transcriptLayout
    )
    /// With `textArtifactCache`, `heightCache` and `expandedCodeBlocks`, one
    /// conversation's `TranscriptViewSession`: a switch swaps them together.
    private var projectionStore = TranscriptProjectionStore()
    private let selectionController = TranscriptSelectionController()
    private let measurementService = TranscriptMeasurementService()
    private var textArtifactCache = TranscriptTextArtifactCache()
    /// Keeps each conversation's rendering and place across switches; nil
    /// starts every conversation fresh at the bottom.
    weak var viewSessions: (any TranscriptViewSessionKeeping)?
    /// A place a rebuilt projection does not hold yet: older pages are asked
    /// for, up to `pageBacks` more, before the bottom stands. It lapses at
    /// `deadline`, so a slow or failed page can't move a reader who has
    /// since settled at the bottom.
    private var pendingPlace: (place: TranscriptReadingPlace, pageBacks: Int, deadline: ContinuousClock.Instant)?
    private static let placePageBackLimit = 3
    /// How long a pending place waits for its rows. Tests on a busy main
    /// actor lengthen it.
    var placeWait: Duration = .seconds(3)
    private var sourcePlan: [TranscriptItem] { projectionStore.sourcePlan }
    private var blocks: [TranscriptBlock] { projectionStore.blocks }
    private var blockIndexByID: [TranscriptBlockID: Int] { projectionStore.indexByID }
    private var completedPresentation: TranscriptCompletedPresentation {
        renderTransactions.completedPresentation
    }
    private var selectionBlocks: [TranscriptBlock] { completedPresentation.blocks }
    private var selectionIndexByID: [TranscriptBlockID: Int] {
        completedPresentation.indexByID
    }
    private lazy var renderTransactions = TranscriptRenderTransactionCoordinator(
        batchUpdater: suppliedBatchUpdater ?? { [unowned collectionView] updates, completion in
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0
                context.allowsImplicitAnimation = false
                collectionView.performBatchUpdates(updates, completionHandler: completion)
            }
        },
        applyCollectionMutation: { [weak self] commit in
            self?.applyCollectionMutation(
                commit.collectionMutation,
                preparing: commit.viewportTarget,
                transactionID: commit.transactionID
            )
        },
        commitLayout: { [weak self] commit in
            self?.finishLayout(for: commit)
        },
        promotePresentation: { [weak self] presentation in
            self?.promotePresentation(presentation)
        },
        settle: { [weak self] settlement, result in
            self?.settle(settlement, result: result)
        },
        ready: { [weak self] in
            self?.rendererDidBecomeReady()
        }
    )
    private var cards: [String: any TranscriptCard] = [:]
    private var cardItemIDByIdentity: [ObjectIdentifier: String] = [:]
    struct HeightCacheEntry {
        let content: TranscriptBlock.Content
        let width: CGFloat
        let theme: MarkdownTheme
        let resourceRoot: URL?
        let height: CGFloat
        var blockOptions: TranscriptScrollableBlockOptions = .init()
    }
    private struct PendingPresentation {
        let plan: [TranscriptItem]
        let cards: [String: any TranscriptCard]
        let conversationID: ConversationID?
        let theme: MarkdownTheme
        let resourceRoot: URL?
        let permitsBackgroundPreparation: Bool
        let completion: CanonicalPresentationCompletion
    }
    private struct DirtyLayoutReason: OptionSet {
        let rawValue: UInt16

        static let canonical = Self(rawValue: 1 << 0)
        static let layout = Self(rawValue: 1 << 1)
        static let viewport = Self(rawValue: 1 << 2)
        static let resize = Self(rawValue: 1 << 3)
        static let insets = Self(rawValue: 1 << 4)
        static let liveScroll = Self(rawValue: 1 << 5)
        static let cardGeometry = Self(rawValue: 1 << 6)
        static let imageGeometry = Self(rawValue: 1 << 7)
        static let selectionAutoscroll = Self(rawValue: 1 << 8)
        static let backgroundPreparation = Self(rawValue: 1 << 9)
        static let refresh = Self(rawValue: 1 << 10)
    }
    private struct PendingViewportInput {
        let epoch: UInt64
        let size: NSSize
        let insets: NSEdgeInsets
        let offsetY: CGFloat
    }
    private struct PendingWork {
        var canonicalPresentation: PendingPresentation?
        var reasons: DirtyLayoutReason = []
        var changedCardIDs: Set<String> = []
        var changedImageIDs: Set<TranscriptBlockID> = []
        var refreshIDs: Set<TranscriptBlockID> = []
        var viewport: PendingViewportInput?
        var viewportTarget: TranscriptViewportTarget?
        var forceFullLayout = false
        var refreshAllMaterializedItems = false

        var isEmpty: Bool {
            canonicalPresentation == nil
                && reasons.isEmpty
                && changedCardIDs.isEmpty
                && changedImageIDs.isEmpty
                && refreshIDs.isEmpty
                && viewportTarget == nil
                && !forceFullLayout
                && !refreshAllMaterializedItems
        }
    }
    private var heightCache: [TranscriptBlockID: HeightCacheEntry] = [:]
    /// Long code blocks the reader has expanded. Transient view state keyed by
    /// stable block identity, so it survives reuse, scrolling and streaming
    /// appends, and is cleared with the conversation.
    private var expandedCodeBlocks: Set<TranscriptBlockID> = []
    /// Height a legacy horizontal scroller takes inside an overflowing code
    /// block; zero under overlay scrollers. Tracked from the system setting.
    private var horizontalScrollerInset = MacAgentTranscriptViewController.horizontalScrollerInset(
        for: NSScroller.preferredScrollerStyle
    )

    private static func horizontalScrollerInset(for style: NSScroller.Style) -> CGFloat {
        style == .legacy
            ? NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
            : 0
    }

    private func blockOptions(for block: TranscriptBlock) -> TranscriptScrollableBlockOptions {
        var options = TranscriptScrollableBlockOptions(
            isExpanded: expandedCodeBlocks.contains(block.id),
            horizontalScrollerInset: horizontalScrollerInset
        )
        if let theme {
            let continuation = block.quoteContinuation(spacing: theme.metrics.blockSpacing)
            options.quoteLeadIn = continuation.leadIn
            options.continuedQuoteDepth = continuation.depth
        }
        return options
    }
    private var animations: [TranscriptBlockID: LiveBatchAnimationCoordinator] = [:]
    #if DEBUG
    private let debugDisablesStreamingFades =
        ProcessInfo.processInfo.environment["OTTAM_DEBUG_TRANSCRIPT_DISABLE_FADES"] == "1"
    private let debugDisablesFollowBottom =
        ProcessInfo.processInfo.environment["OTTAM_DEBUG_TRANSCRIPT_DISABLE_FOLLOW_BOTTOM"] == "1"
    #else
    private let debugDisablesStreamingFades = false
    private let debugDisablesFollowBottom = false
    #endif
    private var theme: MarkdownTheme?
    private var resourceRoot: URL?
    private var conversationID: ConversationID?
    private var observers: [NSObjectProtocol] = []
    private var mode: Mode = .followingBottom
    private var userIsLiveScrolling = false
    private var lastLiveOffsetY: CGFloat?
    private var pendingInitialBottom = false
    /// A place `restoreReadingPosition` keeps until the view has its width.
    private var pendingReadingPosition: TranscriptViewportAnchor?
    private var hasReadingWidth: Bool { view.window != nil && scrollView.contentView.bounds.width > 44 }
    private var nearTopArmed = true
    private var managesHistoryPagination = false
    private var historyCursor: String?
    private var historyIsLoading = false
    private var historyCheckScheduled = false
    private var lastMeasuredWidth: CGFloat = -1
    private var observedContentInsets = NSEdgeInsets()
    private var observedOffsetY: CGFloat?
    private var lastCommittedViewportSize = NSSize(width: -1, height: -1)
    /// Changes only for genuine reader input. Automatic inset and geometry
    /// reconciliation must not invalidate an in-flight follow-bottom target.
    private var readerIntentEpoch: UInt64 = 0
    private var preparationGeneration: UInt64 = 0
    private var pendingPreparation: PendingPresentation?
    private var pendingPreparationWidth: CGFloat?
    private var pendingWork = PendingWork()
    private var isDrainingPendingWork = false
    private var pendingDrainScheduled = false
    private var requiresCanonicalRecovery = false
    private var selectionAutoscrollTimer: Timer?
    private var lastSelectionDragEvent: NSEvent?
    private(set) var debugApplyCount = 0
    private(set) var debugDirectCollectionUpdateCount = 0
    private(set) var debugBatchUpdateCount = 0
    private(set) var debugItemReloadCount = 0
    private(set) var debugNonanimatedStreamingUpdateCount = 0
    private(set) var debugPostCompletionVisibleRefreshCount = 0
    private(set) var debugPendingWorkDrainCount = 0
    private(set) var debugNativeSelectionAutoscrollCount = 0
    var debugSynchronousMaterializationCount: Int {
        layoutCoordinator.synchronousMaterializationCount
    }
    var debugTransactionIsBusy: Bool { renderTransactions.isBusy }
    /// Artifacts built by the shown conversation's cache since it was made.
    var debugTextArtifactBuildCount: Int { textArtifactCache.buildCount }
    var debugPendingImageGeometryCount: Int { pendingWork.changedImageIDs.count }
    func debugCompletedBlockIDs() -> [TranscriptBlockID] {
        completedPresentation.blocks.map(\.id)
    }
    func debugCompletedMarkdownText(itemID: String) -> String? {
        completedPresentation.sourcePlan.first { $0.id == itemID }?.markdownText
    }

    init(
        batchUpdater: TranscriptRenderTransactionCoordinator.BatchUpdater? = nil,
        selectionAutoscroller: ((NSCollectionView, NSEvent) -> Bool)? = nil
    ) {
        suppliedBatchUpdater = batchUpdater
        suppliedSelectionAutoscroller = selectionAutoscroller
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    private static let textIdentifier = NSUserInterfaceItemIdentifier("TranscriptTextItem")
    private static let cardIdentifier = NSUserInterfaceItemIdentifier("TranscriptCardItem")

    struct DebugLayoutState: Equatable {
        let blockCount: Int
        let materializedItemCount: Int
        let visibleAttributeCount: Int
        let offsetY: CGFloat
        let maximumOffsetY: CGFloat
        let readingWidth: CGFloat
        let lastMeasuredWidth: CGFloat
        let isFollowingBottom: Bool
        let userIsLiveScrolling: Bool
    }

    func debugLayoutState() -> DebugLayoutState {
        loadViewIfNeeded()
        if !renderTransactions.isBusy { view.layoutSubtreeIfNeeded() }
        return DebugLayoutState(
            blockCount: blocks.count,
            materializedItemCount: collectionView.visibleItems().count,
            visibleAttributeCount: transcriptLayout.layoutAttributesForElements(
                in: scrollView.contentView.bounds
            ).count,
            offsetY: offsetY,
            maximumOffsetY: maximumOffsetY,
            readingWidth: readingWidth,
            lastMeasuredWidth: lastMeasuredWidth,
            isFollowingBottom: mode == .followingBottom,
            userIsLiveScrolling: userIsLiveScrolling
        )
    }

    func debugSetContentInsets(_ insets: NSEdgeInsets) {
        loadViewIfNeeded()
        observedOffsetY = offsetY
        notifyVisibleItems()
        scrollView.automaticallyAdjustsContentInsets = false
        scrollView.contentInsets = insets
        view.needsLayout = true
        markDirty(.insets)
    }

    func debugSetViewportSize(_ size: NSSize) {
        loadViewIfNeeded()
        view.frame.size = size
        view.layoutSubtreeIfNeeded()
        viewportDidResize()
    }

    func debugBeginLiveScroll() {
        liveScrollWillStart(userInitiated: true)
    }

    func debugScroll(toOffsetY target: CGFloat) {
        let clipView = scrollView.contentView
        let proposed = NSRect(
            x: clipView.bounds.minX,
            y: target,
            width: clipView.bounds.width,
            height: clipView.bounds.height
        )
        clipView.scroll(to: clipView.constrainBoundsRect(proposed).origin)
        scrollView.reflectScrolledClipView(clipView)
        liveScrollDidChange(userInitiated: true)
    }

    func debugSimulateAutomaticLiveScroll(toOffsetY target: CGFloat) {
        let clipView = scrollView.contentView
        let proposed = NSRect(
            x: clipView.bounds.minX,
            y: target,
            width: clipView.bounds.width,
            height: clipView.bounds.height
        )
        liveScrollWillStart(userInitiated: false)
        clipView.scroll(to: clipView.constrainBoundsRect(proposed).origin)
        scrollView.reflectScrolledClipView(clipView)
        liveScrollDidChange(userInitiated: false)
        liveScrollDidEnd()
    }

    func debugEndLiveScroll() {
        liveScrollDidEnd()
    }

    func debugSelect(range: NSRange, in blockID: TranscriptBlockID) -> Bool {
        guard let index = selectionIndexByID[blockID] else { return false }
        selectionController.select(
            range: range,
            in: blockID,
            blockIndex: index,
            blocks: selectionBlocks
        )
        refreshVisibleSelection()
        updateQuoteReply()
        return true
    }

    var debugQuoteReplyPresenter: QuoteReplyPresenter? { quoteReplyPresenter }

    func debugSelectedRange(in blockID: TranscriptBlockID) -> NSRange? {
        guard selectionIndexByID[blockID] != nil else { return nil }
        return selectionController.localRange(
            for: blockID,
            blocks: selectionBlocks,
            indexByID: selectionIndexByID
        )
    }

    func debugSelectedCopyText() -> String? { selectedCopyText() }
    func debugCopyThroughResponderChain() -> Bool {
        guard let window = view.window, window.makeFirstResponder(collectionView) else { return false }
        return collectionView.tryToPerform(#selector(NSText.copy(_:)), with: nil)
    }

    func debugBlockIDs() -> [TranscriptBlockID] { blocks.map(\.id) }

    func debugBlockHeights() -> [CGFloat] {
        blocks.map { cachedHeight(for: $0.id) }
    }

    func debugViewportAnchor() -> TranscriptViewportAnchor? {
        captureAnchor()
    }

    func debugFrame(for id: TranscriptBlockID) -> NSRect? {
        transcriptLayout.frame(for: id)
    }

    /// Presses the transcript's background at `point`, in its document's
    /// coordinates: a click that missed every item view. The pointer drags
    /// through `drags`, then releases at `release`, or where it last was.
    func debugPress(
        at point: NSPoint,
        clickCount: Int,
        drags: [NSPoint] = [],
        release: NSPoint? = nil
    ) {
        let steps = [(NSEvent.EventType.leftMouseDown, point)]
            + drags.map { (NSEvent.EventType.leftMouseDragged, $0) }
            + [(.leftMouseUp, release ?? drags.last ?? point)]
        for (type, point) in steps {
            guard let event = NSEvent.mouseEvent(
                with: type, location: collectionView.convert(point, to: nil), modifierFlags: [], timestamp: 0,
                windowNumber: collectionView.window?.windowNumber ?? 0, context: nil,
                eventNumber: 0, clickCount: clickCount, pressure: 1
            ) else { continue }
            switch type {
            case .leftMouseDown: collectionView.mouseDown(with: event)
            case .leftMouseDragged: collectionView.mouseDragged(with: event)
            default: collectionView.mouseUp(with: event)
            }
        }
    }

    func debugNotifyImageGeometryDidChange(_ id: TranscriptBlockID) {
        imagePayloadDidChange(id: id, height: cachedHeight(for: id))
    }

    func debugBeginSelectionDrag(
        in blockID: TranscriptBlockID,
        offset: Int,
        initialRange: NSRange? = nil
    ) {
        selectionBegan(blockID: blockID, offset: offset, initialRange: initialRange)
    }

    func debugUpdateSelectionDrag(with event: NSEvent) {
        _ = updateSelectionDrag(with: event)
    }

    func debugEndSelectionDrag(with event: NSEvent) {
        selectionEnded(event)
    }

    func debugVisibleThemeSignatures() -> Set<String> {
        Set(collectionView.visibleItems().compactMap {
            ($0 as? TranscriptTextCollectionItem)?.debugThemeSignature
        })
    }

    func debugVisibleArtifactIdentities() -> Set<ObjectIdentifier> {
        Set(collectionView.visibleItems().compactMap {
            ($0 as? TranscriptTextCollectionItem)?.debugArtifactIdentity
        })
    }

    func debugNotifyEffectiveAppearanceChanged() {
        effectiveAppearanceDidChange()
    }

    override func loadView() {
        scrollView.drawsBackground = false
        scrollView.borderType = .noBorder
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        // Keep the shared reading column centered in the full detail pane.
        // A non-overlay scroller reserves its width from the clip view when it
        // appears, shifting the transcript while the SwiftUI composer stays
        // centered in the unchanged pane.
        scrollView.scrollerStyle = .overlay
        scrollView.automaticallyAdjustsContentInsets = true
        scrollView.horizontalScrollElasticity = .none

        collectionView.collectionViewLayout = transcriptLayout
        collectionView.dataSource = self
        collectionView.selectedText = { [weak self] in self?.selectedCopyText() }
        collectionView.acceptsQuoteReply = { [weak self] in self?.quoteReply != nil }
        collectionView.requestQuoteReply = { [weak self] in
            // Show first: a selection made with the keyboard never reached
            // mouse-up, so the surface may not be up yet.
            self?.updateQuoteReply()
            self?.quoteReplyPresenter?.compose()
        }
        collectionView.copySelection = { [weak self] value in self?.copy(value) }
        collectionView.backgroundColors = [.clear]
        collectionView.isSelectable = false
        collectionView.register(
            TranscriptTextCollectionItem.self,
            forItemWithIdentifier: Self.textIdentifier
        )
        collectionView.register(
            TranscriptCardCollectionItem.self,
            forItemWithIdentifier: Self.cardIdentifier
        )
        collectionView.autoresizingMask = [.width]
        scrollView.documentView = collectionView
        scrollView.contentView.postsBoundsChangedNotifications = true
        scrollView.contentView.postsFrameChangedNotifications = true
        scrollView.onLiveResizeEnded = { [weak self] in self?.viewportDidResize() }
        scrollView.onEffectiveAppearanceChanged = { [weak self] in
            self?.effectiveAppearanceDidChange()
        }
        view = scrollView
        observedContentInsets = scrollView.contentInsets
        observedOffsetY = offsetY

        let center = NotificationCenter.default
        observers = [
            center.addObserver(
                forName: NSScrollView.willStartLiveScrollNotification,
                object: scrollView,
                queue: .main
            ) { [weak self] _ in MainActor.assumeIsolated { self?.liveScrollWillStart() } },
            center.addObserver(
                forName: NSScrollView.didLiveScrollNotification,
                object: scrollView,
                queue: .main
            ) { [weak self] _ in MainActor.assumeIsolated { self?.liveScrollDidChange() } },
            center.addObserver(
                forName: NSScrollView.didEndLiveScrollNotification,
                object: scrollView,
                queue: .main
            ) { [weak self] _ in MainActor.assumeIsolated { self?.liveScrollDidEnd() } },
            center.addObserver(
                forName: NSView.boundsDidChangeNotification,
                object: scrollView.contentView,
                queue: .main
            ) { [weak self] _ in MainActor.assumeIsolated { self?.clipViewBoundsDidChange() } },
            center.addObserver(
                forName: NSView.frameDidChangeNotification,
                object: scrollView.contentView,
                queue: .main
            ) { [weak self] _ in MainActor.assumeIsolated { self?.viewportDidResize() } },
            center.addObserver(
                forName: NSScroller.preferredScrollerStyleDidChangeNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.scrollerStyleDidChange(NSScroller.preferredScrollerStyle)
                }
            },
        ]
    }

    /// Switching between overlay and legacy scrollers changes how much body
    /// an overflowing code block needs, which is a global metric change:
    /// every height is stale, so remeasure the whole document.
    func scrollerStyleDidChange(_ style: NSScroller.Style) {
        let inset = Self.horizontalScrollerInset(for: style)
        guard inset != horizontalScrollerInset else { return }
        horizontalScrollerInset = inset
        guard isViewLoaded, theme != nil, !renderTransactions.isTornDown else { return }
        textArtifactCache.removeAll()
        lastMeasuredWidth = -1
        markDirty([.resize, .layout, .refresh], refreshAllMaterializedItems: true)
    }

    private func effectiveAppearanceDidChange() {
        guard isViewLoaded, theme != nil, !renderTransactions.isTornDown else { return }
        textArtifactCache.removeAll()
        markDirty(
            [.refresh],
            refreshAllMaterializedItems: true
        )
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        // SwiftUI can publish the document before its final split-column
        // frame arrives. Reconcile on attachment as well as on resize.
        view.needsLayout = true
    }

    private var hasStaleColumnWidth: Bool {
        let width = scrollView.contentView.bounds.width
        guard width > 44 else { return false }
        if abs(collectionView.bounds.width - width) > 0.5 { return true }
        if let first = transcriptLayout.layoutAttributesForItem(at: IndexPath(item: 0, section: 0)),
           abs(first.frame.width - readingWidth) > 0.5 { return true }
        return collectionView.visibleItems().contains { abs($0.view.frame.width - readingWidth) > 0.5 }
    }

    override func viewDidLayout() {
        super.viewDidLayout()
        let staleColumn = hasStaleColumnWidth
        if staleColumn { lastMeasuredWidth = -1 }
        var reasons: DirtyLayoutReason = []
        if pendingPreparation != nil { reasons.insert(.backgroundPreparation) }
        if !Self.insetsEqual(scrollView.contentInsets, observedContentInsets) {
            reasons.insert(.insets)
        }
        let widthChanged = !blocks.isEmpty && readingWidth != lastMeasuredWidth
        if widthChanged { reasons.insert(.resize) }
        let viewportSize = scrollView.contentView.bounds.size
        let viewportChanged = viewportSize != lastCommittedViewportSize
        if viewportChanged { reasons.insert(.viewport) }
        let target: TranscriptViewportTarget?
        if pendingInitialBottom {
            pendingInitialBottom = false
            target = .bottom
        } else if let anchor = pendingReadingPosition, hasReadingWidth {
            pendingReadingPosition = nil
            target = .anchor(anchor)
        } else {
            target = nil
        }
        placeQuoteReply()
        guard !reasons.isEmpty || target != nil else {
            observedOffsetY = offsetY
            return
        }
        markDirty(reasons.union(.layout), viewportTarget: target,
                  forceFullLayout: staleColumn, refreshAllMaterializedItems: staleColumn)
    }

    func apply(
        plan: [TranscriptItem],
        cards: [String: any TranscriptCard],
        conversationID: ConversationID?,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil
    ) {
        requestPresentation(PendingPresentation(
            plan: plan,
            cards: cards,
            conversationID: conversationID,
            theme: theme,
            resourceRoot: resourceRoot,
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { _ in }
        ))
    }

    func applyPreparingLargeSettledTranscript(
        plan: [TranscriptItem],
        cards: [String: any TranscriptCard],
        conversationID: ConversationID?,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil,
        permitsBackgroundPreparation: Bool
    ) {
        applyPreparingLargeSettledTranscript(
            plan: plan,
            cards: cards,
            conversationID: conversationID,
            theme: theme,
            resourceRoot: resourceRoot,
            permitsBackgroundPreparation: permitsBackgroundPreparation,
            completion: CanonicalPresentationCompletion { _ in }
        )
    }

    func applyPreparingLargeSettledTranscript(
        plan: [TranscriptItem],
        cards: [String: any TranscriptCard],
        conversationID: ConversationID?,
        theme: MarkdownTheme,
        resourceRoot: URL? = nil,
        permitsBackgroundPreparation: Bool,
        completion: CanonicalPresentationCompletion
    ) {
        loadViewIfNeeded()
        requestPresentation(PendingPresentation(
            plan: plan,
            cards: cards,
            conversationID: conversationID,
            theme: theme,
            resourceRoot: resourceRoot,
            permitsBackgroundPreparation: permitsBackgroundPreparation,
            completion: completion
        ))
    }

    private func requestPresentation(_ pending: PendingPresentation) {
        loadViewIfNeeded()
        guard !renderTransactions.isTornDown else {
            pending.completion.finish(false)
            return
        }
        pendingWork.canonicalPresentation?.completion.finish(false)
        pendingWork.canonicalPresentation = pending
        pendingWork.reasons.insert(.canonical)
        capturePendingViewport()
        processPendingWork()
    }

    private func beginPresentation(_ pending: PendingPresentation) {
        precondition(!renderTransactions.isBusy)

        preparationGeneration &+= 1
        pendingPreparation?.completion.finish(false)
        pendingPreparation = nil
        pendingPreparationWidth = nil
        measurementService.cancel()

        let replacement = conversationID != pending.conversationID
        // A kept rendering is already projected and measured.
        let resumes = pending.conversationID.map { viewSessions?.hasViewSession(for: $0) == true } ?? false
        let textBytes = pending.plan.reduce(into: 0) { total, item in
            total += item.markdownText?.utf8.count ?? 0
        }
        guard pending.permitsBackgroundPreparation,
              replacement, !resumes,
              textBytes >= 64 * 1_024
        else {
            applyNow(
                plan: pending.plan,
                cards: pending.cards,
                conversationID: pending.conversationID,
                theme: pending.theme,
                resourceRoot: pending.resourceRoot,
                prepared: nil,
                settlement: .canonical(pending.completion)
            )
            return
        }

        pendingPreparation = pending
        startPendingPreparationIfPossible()
    }

    private func startPendingPreparationIfPossible() {
        guard let pending = pendingPreparation,
              !renderTransactions.isBusy,
              scrollView.contentView.bounds.width > 44,
              !view.inLiveResize
        else { return }
        let width = readingWidth
        guard width > 1, pendingPreparationWidth != width else { return }

        preparationGeneration &+= 1
        let generation = preparationGeneration
        pendingPreparationWidth = width
        measurementService.prepare(.init(
            items: pending.plan.map(TranscriptProjectionItem.init),
            width: width,
            theme: pending.theme,
            resourceRoot: pending.resourceRoot,
            expandedCodeBlocks: expandedCodeBlocks,
            horizontalScrollerInset: horizontalScrollerInset
        )) { [weak self] prepared in
            guard let self,
                  self.preparationGeneration == generation,
                  let current = self.pendingPreparation,
                  current.conversationID == pending.conversationID
            else { return }
            guard prepared.width == self.readingWidth,
                  prepared.themeSignature == current.theme.signature,
                  prepared.resourceRoot == current.resourceRoot
            else {
                self.pendingPreparationWidth = nil
                self.startPendingPreparationIfPossible()
                return
            }
            guard !self.renderTransactions.isBusy, !self.isDrainingPendingWork else {
                self.pendingPreparationWidth = nil
                self.markDirty(.backgroundPreparation)
                return
            }
            self.pendingPreparation = nil
            self.pendingPreparationWidth = nil
            self.applyNow(
                plan: current.plan,
                cards: current.cards,
                conversationID: current.conversationID,
                theme: current.theme,
                resourceRoot: current.resourceRoot,
                prepared: prepared,
                settlement: .canonical(current.completion)
            )
        }
    }

    private func applyNow(
        plan: [TranscriptItem],
        cards: [String: any TranscriptCard],
        conversationID: ConversationID?,
        theme: MarkdownTheme,
        resourceRoot: URL?,
        prepared: TranscriptMeasurementService.Result?,
        settlement: TranscriptRenderTransactionCoordinator.Settlement
    ) {
        loadViewIfNeeded()
        let cardsUnchanged = self.cards.count == cards.count && cards.allSatisfy { key, value in
            self.cards[key] === value
        }
        guard sourcePlan != plan
                || self.conversationID != conversationID
                || self.theme != theme
                || self.resourceRoot != resourceRoot
                || !cardsUnchanged
                || requiresCanonicalRecovery
        else {
            settle(settlement, result: .presented)
            return
        }
        if prepared == nil, cardsUnchanged, !requiresCanonicalRecovery,
           self.conversationID == conversationID, self.theme == theme,
           self.resourceRoot == resourceRoot,
           let append = Self.appendOnlyChange(from: sourcePlan, to: plan),
           appendProjectedText(append.delta, to: append.itemID, settlement: settlement) {
            return
        }
        let transactionID = renderTransactions.reserveTransactionID()
        var prepareInterval: TranscriptSignpost.IntervalToken? = TranscriptSignpost.beginRenderPrepare(
            transactionID: transactionID
        )
        defer {
            if let prepareInterval {
                TranscriptSignpost.endRenderPrepare(
                    prepareInterval,
                    projectedBlocks: blocks.count,
                    artifactCount: textArtifactCache.count
                )
            }
        }
        debugApplyCount += 1
        let replacement = self.conversationID != conversationID
        let recovery = requiresCanonicalRecovery
        let presentationChanged = self.theme != theme || self.resourceRoot != resourceRoot
        let changedCardItemIDs = Self.changedCardItemIDs(from: sourcePlan, to: plan)
        let anchor = replacement || mode == .followingBottom ? nil : captureAnchor()
        var resumed = false
        if replacement {
            cancelSelectionTracking()
            selectionController.clear()
            quoteReplyPresenter?.hide()
            animations.values.forEach { $0.finalize() }
            animations.removeAll(keepingCapacity: true)
            resumed = swapViewSession(to: conversationID)
        }
        let mutation = projectionStore.replace(
            plan: plan,
            resetProjector: (replacement && !resumed) || recovery,
            preparedProjector: prepared?.projector,
            preparedBlocks: prepared?.blocks
        )
        let oldBlocks = mutation.previousBlocks
        let newBlocks = mutation.replacementBlocks
        self.cards = cards
        cardItemIDByIdentity = Dictionary(uniqueKeysWithValues: cards.map {
            (ObjectIdentifier($0.value as AnyObject), $0.key)
        })
        self.theme = theme
        transcriptLayout.blockSpacing = theme.metrics.blockSpacing
        self.resourceRoot = resourceRoot
        self.conversationID = conversationID
        if !changedCardItemIDs.isEmpty {
            for block in newBlocks where changedCardItemIDs.contains(block.itemID) {
                heightCache.removeValue(forKey: block.id)
            }
        }
        let liveIDs = Set(newBlocks.map(\.id))
        textArtifactCache.retain(liveIDs)
        let staleAnimationIDs = animations.keys.filter { !liveIDs.contains($0) }
        for id in staleAnimationIDs {
            animations.removeValue(forKey: id)?.finalize()
        }
        if let prepared {
            installPreparedHeights(prepared.textHeights)
        } else {
            recalculateHeights()
        }
        let changedCardIndex = newBlocks.firstIndex {
            changedCardItemIDs.contains($0.itemID)
        } ?? newBlocks.count
        let invalidationStart = replacement || recovery || presentationChanged
            ? 0
            : min(mutation.firstInvalidatedIndex, changedCardIndex)
        let collectionMutation: TranscriptCollectionMutation
        let refreshIDs: Set<TranscriptBlockID>
        let target: TranscriptViewportTarget
        let forceFullLayout: Bool
        if replacement || recovery {
            collectionMutation = .reload
            refreshIDs = []
            if replacement {
                pendingInitialBottom = false
                target = openingTarget(for: conversationID)
            } else {
                target = viewportTarget(preserving: anchor)
            }
            forceFullLayout = true
        } else {
            let difference = collectionDifference(
                from: oldBlocks,
                to: newBlocks,
                forceRefreshVisible: presentationChanged,
                forceRefreshIDs: Set(newBlocks.lazy.filter {
                    changedCardItemIDs.contains($0.itemID)
                }.map(\.id))
            )
            collectionMutation = difference.mutation
            refreshIDs = difference.refreshIDs
            target = viewportTarget(preserving: anchor)
            forceFullLayout = false
        }

        stageLayoutEntries(invalidatingFrom: invalidationStart)

        if let interval = prepareInterval {
            TranscriptSignpost.endRenderPrepare(
                interval,
                projectedBlocks: blocks.count,
                artifactCount: textArtifactCache.count
            )
            prepareInterval = nil
        }
        let result = renderTransactions.submit(.init(
            transactionID: transactionID,
            collectionMutation: collectionMutation,
            forceFullLayout: forceFullLayout,
            viewportTarget: target,
            viewportEpoch: readerIntentEpoch,
            refreshIDs: refreshIDs,
            refreshAllMaterializedItems: collectionMutation.mode == .batch,
            candidatePresentation: .init(
                sourcePlan: sourcePlan,
                blocks: blocks,
                indexByID: blockIndexByID
            ),
            settlement: settlement
        ))
        precondition(result != .busy, "presentation preparation must not overlap a native update")
        if result == .tornDown { return }
        if collectionMutation.mode == .batch {
            debugBatchUpdateCount += 1
            debugNonanimatedStreamingUpdateCount += 1
        }
        if pendingPlace != nil {
            DispatchQueue.main.async { [weak self] in self?.resolvePendingPlace() }
        }
    }

    // MARK: - Conversation sessions

    /// Hands the outgoing conversation's rendering and place to the keeper,
    /// then installs the incoming one's. True when the incoming rendering
    /// was kept, so its block identities and heights still hold.
    private func swapViewSession(to incoming: ConversationID?) -> Bool {
        keepViewSession()
        guard let incoming, let kept = viewSessions?.takeViewSession(for: incoming) else { return false }
        install(kept)
        return true
    }

    /// Hands this conversation's rendering and place to the keeper, leaving
    /// the view an empty one. Called when the conversation changes and
    /// before the view is torn down.
    func keepViewSession() {
        let place = currentReadingPlace()
        let session = TranscriptViewSession(
            projectionStore: projectionStore,
            textArtifactCache: textArtifactCache,
            heightCache: heightCache,
            expandedCodeBlocks: expandedCodeBlocks
        )
        install(TranscriptViewSession())
        pendingPlace = nil
        pendingReadingPosition = nil
        guard let conversationID, let viewSessions else { return }
        viewSessions.keep(session, place: place, for: conversationID)
    }

    private func install(_ session: TranscriptViewSession) {
        projectionStore = session.projectionStore
        textArtifactCache = session.textArtifactCache
        heightCache = session.heightCache
        expandedCodeBlocks = session.expandedCodeBlocks
    }

    /// Where a conversation opens: the place the reader left it, or the
    /// bottom. A place above the loaded rows is pending until older pages
    /// bring it.
    private func openingTarget(for conversationID: ConversationID?) -> TranscriptViewportTarget {
        if let place = conversationID.flatMap({ viewSessions?.readingPlace(for: $0) }), place != .bottom {
            if let anchor = viewportAnchor(for: place) {
                setMode(.detached)
                guard hasReadingWidth else {
                    pendingReadingPosition = anchor
                    return .unchanged
                }
                return .anchor(anchor)
            }
            pendingPlace = (place, Self.placePageBackLimit, .now + placeWait)
        }
        setMode(.followingBottom)
        return blocks.isEmpty ? .unchanged : .bottom
    }

    /// The reader's place by content. Nil when it can't be read (mid-scroll),
    /// which keeps the place recorded before.
    private func currentReadingPlace() -> TranscriptReadingPlace? {
        if let pendingPlace { return pendingPlace.place }
        if mode == .followingBottom { return .bottom }
        guard let anchor = pendingReadingPosition ?? captureAnchor(),
              let index = projectionStore.indexByID[anchor.id] else { return nil }
        let itemID = blocks[index].itemID
        let start = projectionStore.blockRangeByItemID[itemID]?.lowerBound ?? index
        return .block(itemID: itemID, ordinal: index - start, relativeY: anchor.relativeY)
    }

    private func viewportAnchor(for place: TranscriptReadingPlace) -> TranscriptViewportAnchor? {
        guard case .block(let itemID, let ordinal, let relativeY) = place,
              let range = projectionStore.blockRangeByItemID[itemID], !range.isEmpty
        else { return nil }
        let index = min(range.lowerBound + max(0, ordinal), range.upperBound - 1)
        return TranscriptViewportAnchor(id: blocks[index].id, relativeY: relativeY)
    }

    /// Restores a pending place once its rows arrive; otherwise asks for the
    /// page before, up to the limit, and leaves the reader at the bottom.
    private func resolvePendingPlace() {
        guard let pending = pendingPlace, !renderTransactions.isTornDown else { return }
        guard ContinuousClock.now < pending.deadline else {
            pendingPlace = nil
            return
        }
        if let anchor = viewportAnchor(for: pending.place) {
            pendingPlace = nil
            restoreReadingPosition(anchor)
            return
        }
        guard pending.pageBacks > 0, managesHistoryPagination, historyCursor != nil else {
            pendingPlace = nil
            return
        }
        guard !historyIsLoading else { return }
        pendingPlace?.pageBacks -= 1
        onNearTop()
    }

    func cardPayloadDidChange(_ card: any TranscriptCard) {
        guard let itemID = cardItemIDByIdentity[ObjectIdentifier(card as AnyObject)] else { return }
        markDirty(
            [.layout, .cardGeometry, .refresh],
            changedCardIDs: [itemID]
        )
    }

    func scrollToBottom() {
        loadViewIfNeeded()
        setMode(.followingBottom)
        commitLayout(target: .bottom)
        lastLiveOffsetY = offsetY
        observedOffsetY = offsetY
    }

    func scrollToTop() {
        loadViewIfNeeded()
        setMode(.detached)
        commitLayout(target: .offset(minimumOffsetY))
        lastLiveOffsetY = offsetY
        observedOffsetY = offsetY
    }

    /// The reader's place, to restore once the view is rebuilt.
    func readingPosition() -> TranscriptViewportAnchor? {
        // A place not yet restored is still the reader's place.
        isViewLoaded ? pendingReadingPosition ?? captureAnchor() : nil
    }

    /// Returns to a place `readingPosition` saved, keeping the current offset
    /// when its block is gone.
    func restoreReadingPosition(_ anchor: TranscriptViewportAnchor) {
        loadViewIfNeeded()
        setMode(.detached)
        // An offset names a place only once text wraps at the view's own
        // width, which a view applied before it is in a window lacks.
        guard hasReadingWidth else {
            pendingReadingPosition = anchor
            return
        }
        pendingReadingPosition = nil
        commitLayout(target: .anchor(anchor))
        lastLiveOffsetY = offsetY
        observedOffsetY = offsetY
    }

    func scrollBack(viewports: Int) {
        loadViewIfNeeded()
        setMode(.detached)
        let target = max(
            minimumOffsetY,
            offsetY - scrollView.contentView.bounds.height * CGFloat(viewports)
        )
        commitLayout(target: .offset(target))
        lastLiveOffsetY = offsetY
        observedOffsetY = offsetY
        requestEarlierHistoryIfNeeded(readerInitiated: true)
    }

    /// Drives the same logical selection and responder-chain Copy path as a
    /// pointer drag, while remaining deterministic for the registered
    /// real-window scenario. Pointer autoscroll is still verified manually.
    func selectAndCopyForUITest() -> Bool {
        loadViewIfNeeded()
        guard let index = selectionBlocks.indices.reversed().first(where: {
            guard case .text(let text) = selectionBlocks[$0].content else { return false }
            return !text.source.isEmpty
        }),
        case .text(let text) = selectionBlocks[index].content
        else { return false }

        let length = min(32, text.source.utf16.count)
        guard length > 0 else { return false }
        selectionController.select(
            range: NSRange(location: 0, length: length),
            in: selectionBlocks[index].id,
            blockIndex: index,
            blocks: selectionBlocks
        )
        refreshVisibleSelection()
        guard let expected = selectedCopyText(), debugCopyThroughResponderChain() else { return false }
        return NSPasteboard.general.string(forType: .string) == expected
    }

    /// Resizes the owning native window so width invalidation, content insets,
    /// and bottom restoration are exercised by the scenario runner.
    func resizeWindowForUITest() -> Bool {
        loadViewIfNeeded()
        guard let window = view.window else { return false }
        let originalWidth = window.frame.width
        let targetWidth = max(window.minSize.width, originalWidth - 180)
        guard abs(targetWidth - originalWidth) > 1 else { return false }
        var frame = window.frame
        frame.origin.x += originalWidth - targetWidth
        frame.size.width = targetWidth
        window.setFrame(frame, display: true, animate: false)
        window.contentView?.layoutSubtreeIfNeeded()
        return abs(window.frame.width - targetWidth) <= 1
    }

    func tearDown() {
        renderTransactions.tearDown()
        cancelSelectionTracking()
        selectionController.clear()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers.removeAll()
        cards.removeAll()
        cardItemIDByIdentity.removeAll()
        projectionStore.reset()
        heightCache.removeAll()
        expandedCodeBlocks.removeAll()
        textArtifactCache.removeAll()
        preparationGeneration &+= 1
        pendingPreparation?.completion.finish(false)
        pendingPreparation = nil
        pendingWork.canonicalPresentation?.completion.finish(false)
        pendingWork = PendingWork()
        pendingDrainScheduled = false
        pendingPreparationWidth = nil
        measurementService.cancel()
        animations.values.forEach { $0.finalize() }
        animations.removeAll()
    }

    private func recalculateHeights() {
        guard let theme else { return }
        let width = readingWidth
        lastMeasuredWidth = width
        var nextCache: [TranscriptBlockID: HeightCacheEntry] = [:]
        for block in blocks {
            let height: CGFloat
            switch block.content {
            case .text(let text):
                if let cached = heightCache[block.id],
                   cached.width == width,
                   cached.theme == theme,
                   cached.resourceRoot == resourceRoot,
                   cached.blockOptions == blockOptions(for: block),
                   cached.content == block.content {
                    height = cached.height
                } else {
                    height = TranscriptTextMeasurement.height(
                        for: text,
                        width: width,
                        theme: theme,
                        resourceRoot: resourceRoot,
                        options: blockOptions(for: block)
                    )
                }
            case .card:
                // Always the card's own: it caches by payload, and a kept
                // rendering returns with rebuilt cards whose UI state reset.
                height = cards[block.itemID]?.measuredHeight(forWidth: width) ?? 1
            }
            nextCache[block.id] = HeightCacheEntry(
                content: block.content,
                width: width,
                theme: theme,
                resourceRoot: resourceRoot,
                height: height,
                blockOptions: blockOptions(for: block)
            )
        }
        heightCache = nextCache
    }

    private func installPreparedHeights(_ preparedTextHeights: [TranscriptBlockID: CGFloat]) {
        guard let theme else { return }
        let width = readingWidth
        lastMeasuredWidth = width
        var nextCache: [TranscriptBlockID: HeightCacheEntry] = [:]
        nextCache.reserveCapacity(blocks.count)
        for block in blocks {
            let height: CGFloat
            switch block.content {
            case .text:
                height = preparedTextHeights[block.id] ?? 1
            case .card:
                height = cards[block.itemID]?.measuredHeight(forWidth: width) ?? 1
            }
            nextCache[block.id] = HeightCacheEntry(
                content: block.content,
                width: width,
                theme: theme,
                resourceRoot: resourceRoot,
                height: height,
                blockOptions: blockOptions(for: block)
            )
        }
        heightCache = nextCache
    }

    private func cachedHeight(for id: TranscriptBlockID) -> CGFloat {
        heightCache[id]?.height ?? 1
    }

    private func stageLayoutEntries(invalidatingFrom index: Int) {
        transcriptLayout.setEntries(blocks.map {
            .init(id: $0.id, height: cachedHeight(for: $0.id), relation: $0.relation, block: $0.markdownNeighbor)
        }, invalidatingFrom: index)
    }

    private func viewportTarget(preserving anchor: TranscriptViewportAnchor?) -> TranscriptViewportTarget {
        if userIsLiveScrolling { return .deferred }
        if mode == .followingBottom {
            return debugDisablesFollowBottom ? .unchanged : .bottom
        }
        return .anchor(anchor)
    }

    private func capturePendingViewport() {
        pendingWork.viewport = PendingViewportInput(
            epoch: readerIntentEpoch,
            size: scrollView.contentView.bounds.size,
            insets: scrollView.contentInsets,
            offsetY: offsetY
        )
    }

    private func markDirty(
        _ reasons: DirtyLayoutReason,
        changedCardIDs: Set<String> = [],
        changedImageIDs: Set<TranscriptBlockID> = [],
        refreshIDs: Set<TranscriptBlockID> = [],
        viewportTarget: TranscriptViewportTarget? = nil,
        forceFullLayout: Bool = false,
        refreshAllMaterializedItems: Bool = false
    ) {
        guard !renderTransactions.isTornDown else { return }
        pendingWork.reasons.formUnion(reasons)
        pendingWork.changedCardIDs.formUnion(changedCardIDs)
        pendingWork.changedImageIDs.formUnion(changedImageIDs)
        pendingWork.refreshIDs.formUnion(refreshIDs)
        if let viewportTarget { pendingWork.viewportTarget = viewportTarget }
        pendingWork.forceFullLayout = pendingWork.forceFullLayout || forceFullLayout
        pendingWork.refreshAllMaterializedItems =
            pendingWork.refreshAllMaterializedItems || refreshAllMaterializedItems
        capturePendingViewport()
        processPendingWork()
    }

    private func processPendingWork() {
        guard !renderTransactions.isBusy,
              !renderTransactions.isTornDown,
              !isDrainingPendingWork
        else { return }

        if !pendingWork.isEmpty, canDrainPendingWorkNow {
            drainOnePendingWork()
            if renderTransactions.isBusy { return }
            if !pendingWork.isEmpty, canDrainPendingWorkNow {
                schedulePendingWorkDrain()
                return
            }
        }

        startPendingPreparationIfPossible()
    }

    private var canDrainPendingWorkNow: Bool {
        if pendingWork.canonicalPresentation != nil { return true }
        if userIsLiveScrolling {
            return !pendingWork.reasons.intersection([.viewport, .liveScroll]).isEmpty
        }
        if view.inLiveResize {
            return !pendingWork.reasons.intersection([.viewport, .insets]).isEmpty
        }
        return true
    }

    private func schedulePendingWorkDrain() {
        guard !pendingDrainScheduled else { return }
        pendingDrainScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.pendingDrainScheduled = false
            self.processPendingWork()
        }
    }

    private func drainOnePendingWork() {
        guard !pendingWork.isEmpty else { return }
        var work = pendingWork
        pendingWork = PendingWork()

        if userIsLiveScrolling, work.canonicalPresentation == nil {
            pendingWork.changedCardIDs.formUnion(work.changedCardIDs)
            pendingWork.changedImageIDs.formUnion(work.changedImageIDs)
            pendingWork.refreshIDs.formUnion(work.refreshIDs)
            pendingWork.refreshAllMaterializedItems = work.refreshAllMaterializedItems
            pendingWork.reasons.formUnion(
                work.reasons.intersection([.cardGeometry, .imageGeometry, .refresh])
            )
            work.changedCardIDs.removeAll()
            work.changedImageIDs.removeAll()
            work.refreshIDs.removeAll()
            work.refreshAllMaterializedItems = false
            work.reasons.subtract([.cardGeometry, .imageGeometry, .refresh])
        }
        if view.inLiveResize, work.canonicalPresentation == nil {
            pendingWork.reasons.formUnion(work.reasons.intersection([.resize, .layout]))
            pendingWork.viewport = work.viewport
            work.reasons.subtract([.resize, .layout])
        }

        isDrainingPendingWork = true
        debugPendingWorkDrainCount += 1
        if let canonical = work.canonicalPresentation {
            if work.reasons.contains(.insets), let viewport = work.viewport {
                observedContentInsets = viewport.insets
            }
            beginPresentation(canonical)
        } else {
            applyPendingLayoutWork(work)
        }
        isDrainingPendingWork = false
    }

    private func applyPendingLayoutWork(_ work: PendingWork) {
        if work.reasons.contains(.backgroundPreparation) {
            startPendingPreparationIfPossible()
        }
        guard let theme else { return }

        var invalidationStart: Int?
        var refreshIDs = work.refreshIDs
        let anchor = mode == .followingBottom || userIsLiveScrolling ? nil : captureAnchor()
        let widthChanged = work.reasons.contains(.resize) && readingWidth != lastMeasuredWidth
        if widthChanged {
            recalculateHeights()
            // Constant-height rows still need new horizontal frames. Equal
            // entries otherwise skip setEntries' invalidation entirely.
            transcriptLayout.invalidateLayout()
            invalidationStart = 0
            refreshIDs.formUnion(collectionView.indexPathsForVisibleItems().compactMap { indexPath in
                blocks.indices.contains(indexPath.item) ? blocks[indexPath.item].id : nil
            })
        }

        for itemID in work.changedCardIDs {
            guard let index = projectionStore.blockRangeByItemID[itemID]?.lowerBound,
                  blocks.indices.contains(index),
                  let card = cards[itemID]
            else { continue }
            let block = blocks[index]
            heightCache[block.id] = HeightCacheEntry(
                content: block.content,
                width: readingWidth,
                theme: theme,
                resourceRoot: resourceRoot,
                height: card.measuredHeight(forWidth: readingWidth)
            )
            invalidationStart = min(invalidationStart ?? index, index)
            refreshIDs.insert(block.id)
        }

        for id in work.changedImageIDs {
            guard let index = blockIndexByID[id],
                  blocks.indices.contains(index),
                  let artifact = textArtifactCache.artifact(
                    for: blocks[index],
                    width: readingWidth,
                    theme: theme,
                    resourceRoot: resourceRoot,
                    options: blockOptions(for: blocks[index])
                  ),
                  abs(cachedHeight(for: id) - artifact.measuredHeight) > 0.5
            else { continue }
            heightCache[id] = HeightCacheEntry(
                content: blocks[index].content,
                width: readingWidth,
                theme: theme,
                resourceRoot: resourceRoot,
                height: artifact.measuredHeight,
                blockOptions: artifact.options
            )
            invalidationStart = min(invalidationStart ?? index, index)
            refreshIDs.insert(id)
        }

        if let invalidationStart {
            stageLayoutEntries(invalidatingFrom: invalidationStart)
        }

        var target = work.viewportTarget
        if work.reasons.contains(.insets), let viewport = work.viewport {
            observedContentInsets = viewport.insets
            target = mode == .followingBottom
                ? .bottom
                : .offset(viewport.offsetY)
        }
        if userIsLiveScrolling {
            target = .deferred
        } else if target == nil,
                  invalidationStart != nil || work.reasons.contains(.resize)
                    || work.reasons.contains(.liveScroll) {
            target = viewportTarget(preserving: anchor)
        }

        guard target != nil || invalidationStart != nil || !refreshIDs.isEmpty
                || work.forceFullLayout || work.refreshAllMaterializedItems
        else { return }
        let transactionID = renderTransactions.reserveTransactionID()
        let result = renderTransactions.submit(.init(
            transactionID: transactionID,
            collectionMutation: .none,
            forceFullLayout: work.forceFullLayout,
            viewportTarget: target ?? .unchanged,
            viewportEpoch: work.viewport?.epoch ?? readerIntentEpoch,
            refreshIDs: refreshIDs,
            refreshAllMaterializedItems: work.refreshAllMaterializedItems,
            candidatePresentation: completedPresentation,
            settlement: .none
        ))
        precondition(result != .busy, "pending layout work must drain only while idle")
    }

    private func commitLayout(
        forceFull: Bool = false,
        target: TranscriptViewportTarget,
        refreshIDs: Set<TranscriptBlockID> = []
    ) {
        markDirty(
            [.layout, .refresh],
            refreshIDs: refreshIDs,
            viewportTarget: target,
            forceFullLayout: forceFull
        )
    }

    private func commitLayoutNow(
        transactionID: UInt64,
        forceFull: Bool,
        target: TranscriptViewportTarget,
        refreshIDs: Set<TranscriptBlockID>,
        refreshAllMaterializedItems: Bool = false,
        refreshSelection: Bool
    ) {
        lastCommittedViewportSize = scrollView.contentView.bounds.size
        if case .deferred = target {
            pendingWork.refreshIDs.formUnion(refreshIDs)
            pendingWork.refreshAllMaterializedItems =
                pendingWork.refreshAllMaterializedItems || refreshAllMaterializedItems
            if refreshAllMaterializedItems {
                pendingWork.reasons.insert(.refresh)
            }
        }
        layoutCoordinator.commit(
            transactionID: transactionID,
            forceFull: forceFull,
            target: target,
            refreshMode: refreshAllMaterializedItems
                ? "all"
                : refreshIDs.isEmpty ? "none" : "ids",
            requestedRefreshItems: refreshIDs.count
        ) { [self] in
            let refreshedItems = if refreshAllMaterializedItems {
                refreshAllVisibleMaterializedItems()
            } else {
                refreshMaterializedItems(ids: refreshIDs)
            }
            if refreshSelection { refreshVisibleSelection() }
            return .init(
                refreshedItems: refreshedItems,
                selectionRefreshed: refreshSelection
            )
        }
        observedOffsetY = offsetY
        notifyVisibleItems()
        scheduleHistoryCheck()
    }

    private func finishLayout(
        for commit: TranscriptRenderTransactionCoordinator.Commit
    ) {
        let hasNewerGeometry = !pendingWork.reasons.intersection([
            .insets, .resize, .viewport,
        ]).isEmpty
        // A reader action invalidates the captured target. Newer automatic
        // geometry merely postpones it: the coalesced inset/resize transaction
        // recomputes bottom or preserves the detached coordinate on fresh bounds.
        let target = commit.viewportEpoch == readerIntentEpoch && !hasNewerGeometry
            ? commit.viewportTarget
            : TranscriptViewportTarget.deferred
        commitLayoutNow(
            transactionID: commit.transactionID,
            forceFull: commit.forceFullLayout,
            target: target,
            refreshIDs: commit.refreshIDs,
            refreshAllMaterializedItems: commit.refreshAllMaterializedItems,
            refreshSelection: false
        )
    }

    private func promotePresentation(_ presentation: TranscriptCompletedPresentation) {
        selectionController.reconcile(
            blocks: presentation.blocks,
            indexByID: presentation.indexByID
        )
        for indexPath in collectionView.indexPathsForVisibleItems() {
            guard blocks.indices.contains(indexPath.item),
                  let item = collectionView.item(at: indexPath)
            else { continue }
            let presented = presentation.indexByID[blocks[indexPath.item].id] != nil
            if let text = item as? TranscriptTextCollectionItem {
                text.setAccessibilityPresented(presented)
            } else if let card = item as? TranscriptCardCollectionItem {
                card.setAccessibilityPresented(presented)
            }
        }
        refreshVisibleSelection()
        // Settled content can move or drop the selected lines.
        updateQuoteReply()
    }

    private func settle(
        _ settlement: TranscriptRenderTransactionCoordinator.Settlement,
        result: TranscriptRenderTransactionCoordinator.CompletionResult
    ) {
        switch settlement {
        case .none:
            break
        case .canonical(let completion):
            if result == .presented {
                requiresCanonicalRecovery = false
            } else {
                requiresCanonicalRecovery = true
            }
            completion.finish(result == .presented)
        }
    }

    private func rendererDidBecomeReady() {
        if readingWidth != lastMeasuredWidth
            || hasStaleColumnWidth
            || scrollView.contentView.bounds.size != lastCommittedViewportSize
            || !Self.insetsEqual(scrollView.contentInsets, observedContentInsets) {
            view.needsLayout = true
        }
        processPendingWork()
    }

    private func collectionDifference(
        from old: [TranscriptBlock],
        to new: [TranscriptBlock],
        indexOffset: Int = 0,
        forceRefreshVisible: Bool = false,
        forceRefreshIDs: Set<TranscriptBlockID> = []
    ) -> (mutation: TranscriptCollectionMutation, refreshIDs: Set<TranscriptBlockID>) {
        let oldIDs = old.map(\.id)
        let newIDs = new.map(\.id)
        guard oldIDs != newIDs else {
            var changedIDs = forceRefreshIDs
            changedIDs.formUnion(new.indices.compactMap { index -> TranscriptBlockID? in
                old.indices.contains(index) && old[index] != new[index] ? new[index].id : nil
            })
            if forceRefreshVisible {
                changedIDs.formUnion(collectionView.indexPathsForVisibleItems().compactMap { indexPath in
                    let localIndex = indexPath.item - indexOffset
                    return new.indices.contains(localIndex) ? new[localIndex].id : nil
                })
            }
            return (.none, changedIDs)
        }

        // AppKit collection identity is positional. Within a non-moving
        // replacement window, pair old/new positions as native item updates
        // and reserve insert/delete for the count delta. Delete+insert pairs at
        // identical paths leave stale materialized items in large pages.
        var prefixCount = 0
        while prefixCount < min(oldIDs.count, newIDs.count),
              oldIDs[prefixCount] == newIDs[prefixCount] {
            prefixCount += 1
        }
        var suffixCount = 0
        while suffixCount < min(oldIDs.count, newIDs.count) - prefixCount,
              oldIDs[oldIDs.count - suffixCount - 1] == newIDs[newIDs.count - suffixCount - 1] {
            suffixCount += 1
        }
        let oldMiddle = prefixCount..<(oldIDs.count - suffixCount)
        let newMiddle = prefixCount..<(newIDs.count - suffixCount)
        let oldMiddleIndexByID = Dictionary(uniqueKeysWithValues: oldMiddle.map {
            (oldIDs[$0], $0 - prefixCount)
        })
        let reordersSurvivingID = newMiddle.contains { index in
            oldMiddleIndexByID[newIDs[index]].map { $0 != index - prefixCount } ?? false
        }
        if !reordersSurvivingID {
            let overlapCount = min(oldMiddle.count, newMiddle.count)
            let reloads = Set((0..<overlapCount).compactMap { localIndex -> IndexPath? in
                let index = prefixCount + localIndex
                return oldIDs[index] == newIDs[index]
                    ? nil
                    : IndexPath(item: index + indexOffset, section: 0)
            })
            let deletions = Set((prefixCount + overlapCount..<oldMiddle.upperBound).map {
                IndexPath(item: $0 + indexOffset, section: 0)
            })
            let insertions = Set((prefixCount + overlapCount..<newMiddle.upperBound).map {
                IndexPath(item: $0 + indexOffset, section: 0)
            })
            var refreshIDs = forceRefreshIDs
            if forceRefreshVisible {
                refreshIDs.formUnion(collectionView.indexPathsForVisibleItems().compactMap {
                    indexPath in
                    let localIndex = indexPath.item - indexOffset
                    return new.indices.contains(localIndex) ? new[localIndex].id : nil
                })
            }
            return (
                TranscriptCollectionMutation(
                    mode: .batch,
                    deletions: deletions,
                    insertions: insertions,
                    reloads: reloads,
                    moves: []
                ),
                refreshIDs
            )
        }

        let difference = newIDs.difference(from: oldIDs).inferringMoves()
        var deletions = Set<IndexPath>()
        var insertions = Set<IndexPath>()
        var moves: [(from: IndexPath, to: IndexPath)] = []
        for change in difference {
            switch change {
            case .remove(let offset, _, let associated):
                if let associated {
                    moves.append((
                        from: IndexPath(item: offset + indexOffset, section: 0),
                        to: IndexPath(item: associated + indexOffset, section: 0)
                    ))
                } else {
                    deletions.insert(IndexPath(item: offset + indexOffset, section: 0))
                }
            case .insert(let offset, _, let associated):
                if associated == nil {
                    insertions.insert(IndexPath(item: offset + indexOffset, section: 0))
                }
            }
        }
        let stableOld = Dictionary(uniqueKeysWithValues: old.map { ($0.id, $0) })
        var refreshIDs = forceRefreshIDs
        refreshIDs.formUnion(new.compactMap { block -> TranscriptBlockID? in
            guard let prior = stableOld[block.id], prior != block else { return nil }
            return block.id
        })
        if forceRefreshVisible {
            refreshIDs.formUnion(collectionView.indexPathsForVisibleItems().compactMap { indexPath in
                let localIndex = indexPath.item - indexOffset
                return new.indices.contains(localIndex) ? new[localIndex].id : nil
            })
        }
        return (
            TranscriptCollectionMutation(
                mode: .batch,
                deletions: deletions,
                insertions: insertions,
                reloads: [],
                moves: moves.map { .init(from: $0.from, to: $0.to) }
            ),
            refreshIDs
        )
    }

    private func applyCollectionMutation(
        _ mutation: TranscriptCollectionMutation,
        preparing viewportTarget: TranscriptViewportTarget,
        transactionID: UInt64
    ) {
        if mutation.mode == .batch, viewportTarget.preservesBottom {
            layoutCoordinator.prepareForNativeMutation(
                transactionID: transactionID,
                target: viewportTarget
            )
        }
        switch mutation.mode {
        case .none:
            break
        case .reload:
            collectionView.reloadData()
        case .batch:
            if !mutation.deletions.isEmpty {
                collectionView.deleteItems(at: mutation.deletions)
            }
            if !mutation.insertions.isEmpty {
                collectionView.insertItems(at: mutation.insertions)
            }
            if !mutation.reloads.isEmpty {
                debugItemReloadCount += mutation.reloads.count
                collectionView.reloadItems(at: mutation.reloads)
            }
            for move in mutation.moves {
                collectionView.moveItem(at: move.from, to: move.to)
            }
        }
    }

    private static func changedCardItemIDs(
        from old: [TranscriptItem],
        to new: [TranscriptItem]
    ) -> Set<String> {
        let prior = Dictionary(uniqueKeysWithValues: old.compactMap { item -> (String, TranscriptItem.Content)? in
            guard item.cardSpec != nil else { return nil }
            return (item.id, item.content)
        })
        return Set(new.compactMap { item -> String? in
            guard item.cardSpec != nil,
                  let priorContent = prior[item.id],
                  priorContent != item.content
            else { return nil }
            return item.id
        })
    }

    private func captureAnchor() -> TranscriptViewportAnchor? {
        guard !userIsLiveScrolling else { return nil }
        let visible = scrollView.contentView.bounds
        let candidate = collectionView.indexPathsForVisibleItems().compactMap { indexPath -> (IndexPath, NSRect)? in
            guard blocks.indices.contains(indexPath.item),
                  let frame = transcriptLayout.layoutAttributesForItem(at: indexPath)?.frame,
                  frame.intersects(visible)
            else { return nil }
            return (indexPath, frame)
        }.min { lhs, rhs in lhs.1.minY < rhs.1.minY }
        guard let (indexPath, frame) = candidate
        else { return nil }
        return TranscriptViewportAnchor(
            id: blocks[indexPath.item].id,
            relativeY: frame.minY - visible.minY
        )
    }

    /// Live-scroll notifications describe AppKit motion, not its provenance.
    /// Only events that originate at a reader-controlled input boundary may
    /// detach following or invalidate a captured viewport intention.
    private var isReaderScrollEventActive: Bool {
        guard let event = NSApp.currentEvent else { return false }
        switch event.type {
        case .scrollWheel:
            return true
        case .leftMouseDown, .leftMouseDragged, .leftMouseUp:
            guard event.window === scrollView.window,
                  let scroller = scrollView.verticalScroller
            else { return false }
            let point = scroller.convert(event.locationInWindow, from: nil)
            return scroller.bounds.contains(point)
        case .keyDown:
            // Arrow keys, Page Up/Down, Home/End and Space/Shift-Space.
            return [49, 115, 116, 117, 119, 121, 123, 124, 125, 126].contains(event.keyCode)
        default:
            return false
        }
    }

    private func liveScrollWillStart(userInitiated: Bool? = nil) {
        let userInitiated = userInitiated ?? isReaderScrollEventActive
        guard userInitiated, !userIsLiveScrolling else { return }
        userIsLiveScrolling = true
        // The reader moved on from the place still being paged toward.
        pendingPlace = nil
        lastLiveOffsetY = offsetY
        // Invalidate viewport intent captured by an in-flight structural
        // transaction before the gesture began. Its completion must defer the
        // stale bottom/anchor correction until native scrolling settles.
        readerIntentEpoch &+= 1
    }

    private func liveScrollDidChange(userInitiated: Bool? = nil) {
        notifyVisibleItems()
        let current = offsetY
        let userInitiated = userInitiated ?? (userIsLiveScrolling || isReaderScrollEventActive)
        guard userInitiated else {
            // AppKit can emit live-scroll notifications while automatic safe-area
            // reconciliation moves the clip view. Bounds/inset callbacks own that
            // geometry; it is not evidence that the reader detached.
            _ = reconcileContentInsetsIfNeeded()
            return
        }
        if !layoutCoordinator.isProgrammaticScroll,
           let previous = lastLiveOffsetY ?? observedOffsetY,
           current < previous - 0.5 {
            setMode(.detached)
        }
        lastLiveOffsetY = current
        observedOffsetY = current
        readerIntentEpoch &+= 1
        markDirty([.viewport, .liveScroll], viewportTarget: .deferred)
        if bottomDistance <= 2 { setMode(.followingBottom) }
        requestEarlierHistoryIfNeeded(readerInitiated: true)
    }

    func updateHistoryPagination(cursor: String?, isLoading: Bool) {
        if !managesHistoryPagination || historyCursor != cursor {
            nearTopArmed = true
        }
        managesHistoryPagination = true
        historyCursor = cursor
        historyIsLoading = isLoading
        scheduleHistoryCheck()
    }

    private func scheduleHistoryCheck() {
        guard managesHistoryPagination, historyCursor != nil, !historyIsLoading,
              !historyCheckScheduled, !renderTransactions.isTornDown else { return }
        historyCheckScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.historyCheckScheduled = false
            guard !self.renderTransactions.isTornDown, !self.renderTransactions.isBusy,
                  !self.isDrainingPendingWork, self.pendingWork.canonicalPresentation == nil,
                  self.pendingPreparation == nil else { return }
            self.requestEarlierHistoryIfNeeded(readerInitiated: false)
        }
    }

    private func requestEarlierHistoryIfNeeded(readerInitiated: Bool) {
        let distanceFromTop = max(0, offsetY - minimumOffsetY)
        let viewportHeight = max(1, scrollView.contentView.bounds.height
            - scrollView.contentInsets.top - scrollView.contentInsets.bottom)
        let prefetchDistance = 2 * viewportHeight
        // A separate rearm threshold prevents repeated requests while the
        // reader remains inside the prefetch zone, including failed fetches.
        if distanceFromTop > prefetchDistance + max(80, viewportHeight / 2) { nearTopArmed = true }
        guard nearTopArmed, distanceFromTop <= prefetchDistance,
              scrollView.contentView.bounds.width > 44 else { return }
        if managesHistoryPagination {
            guard historyCursor != nil, !historyIsLoading else { return }
        }
        // Do not pull a reader following a long live turn back into history.
        // A collapsed page that fits on screen must still be able to fill it.
        if !readerInitiated, mode == .followingBottom,
           maximumOffsetY - minimumOffsetY > 1 { return }
        nearTopArmed = false
        onNearTop()
    }

    private func notifyVisibleItems() {
        let ids = Set(collectionView.indexPathsForVisibleItems().compactMap { index in
            blocks.indices.contains(index.item) ? blocks[index.item].itemID : nil
        })
        onVisibleItemsChange(ids)
    }

    private func liveScrollDidEnd() {
        guard userIsLiveScrolling else {
            _ = reconcileContentInsetsIfNeeded()
            return
        }
        liveScrollDidChange()
        userIsLiveScrolling = false
        lastLiveOffsetY = nil
        markDirty(
            [.layout, .viewport, .liveScroll],
            viewportTarget: mode == .followingBottom ? .bottom : .unchanged
        )
    }

    private func clipViewBoundsDidChange() {
        if reconcileContentInsetsIfNeeded() { return }
        let previous = observedOffsetY ?? offsetY
        TranscriptSignpost.collectionClipBoundsChanged(
            deltaY: Double(offsetY - previous)
        )
        observedOffsetY = offsetY
        // In this pass, not the next: the surface moves with the text.
        placeQuoteReply()
    }

    /// Automatic titlebar and split-accessory insets arrive independently from
    /// transcript geometry. AppKit may adjust the clip origin as they change, so
    /// following mode must re-pin and detached mode must restore the pre-change
    /// document coordinate explicitly.
    @discardableResult
    private func reconcileContentInsetsIfNeeded() -> Bool {
        let current = scrollView.contentInsets
        guard !Self.insetsEqual(current, observedContentInsets) else { return false }
        let preservedOffset = observedOffsetY ?? offsetY
        markDirty(
            [.layout, .insets, .viewport],
            viewportTarget: userIsLiveScrolling
                ? .deferred
                : mode == .followingBottom ? .bottom : .offset(preservedOffset)
        )
        return true
    }

    private static func insetsEqual(_ lhs: NSEdgeInsets, _ rhs: NSEdgeInsets) -> Bool {
        lhs.top == rhs.top && lhs.left == rhs.left && lhs.bottom == rhs.bottom && lhs.right == rhs.right
    }

    /// Folds or unfolds a long code block. Rides the same identity-stable
    /// height route as an image that finished loading: re-read the block's
    /// artifact height, shift the suffix, refresh the materialized item.
    func toggleCodeBlockExpansion(_ id: TranscriptBlockID) {
        guard blockIndexByID[id] != nil else { return }
        if expandedCodeBlocks.remove(id) == nil { expandedCodeBlocks.insert(id) }
        markDirty([.layout, .imageGeometry, .refresh], changedImageIDs: [id])
    }

    func debugIsCodeBlockExpanded(_ id: TranscriptBlockID) -> Bool {
        expandedCodeBlocks.contains(id)
    }

    private func imagePayloadDidChange(id: TranscriptBlockID, height _: CGFloat) {
        markDirty(
            [.layout, .imageGeometry, .refresh],
            changedImageIDs: [id]
        )
    }

    @discardableResult
    private func refreshMaterializedItems(ids: Set<TranscriptBlockID>) -> Int {
        guard !ids.isEmpty, let theme else { return 0 }
        var refreshedItems = 0
        for indexPath in collectionView.indexPathsForVisibleItems() {
            guard blocks.indices.contains(indexPath.item),
                  ids.contains(blocks[indexPath.item].id),
                  let item = collectionView.item(at: indexPath)
            else { continue }
            let block = blocks[indexPath.item]
            applyPresentation(for: block, to: item, theme: theme)
            refreshedItems += 1
        }
        return refreshedItems
    }

    @discardableResult
    private func refreshAllVisibleMaterializedItems() -> Int {
        guard let theme else { return 0 }
        var refreshedItems = 0
        for indexPath in collectionView.indexPathsForVisibleItems() {
            guard blocks.indices.contains(indexPath.item),
                  let item = collectionView.item(at: indexPath)
            else { continue }
            applyPresentation(for: blocks[indexPath.item], to: item, theme: theme)
            debugPostCompletionVisibleRefreshCount += 1
            refreshedItems += 1
        }
        return refreshedItems
    }

    private func viewportDidResize() {
        markDirty(
            [.layout, .resize, .viewport, .backgroundPreparation],
            viewportTarget: view.inLiveResize ? .deferred : nil
        )
        _ = reconcileContentInsetsIfNeeded()
    }

    private func setMode(_ value: Mode) {
        let wasFollowing = mode == .followingBottom
        mode = value
        let follows = value == .followingBottom
        if wasFollowing != follows { onFollowsBottomChange(follows) }
    }

    private var readingWidth: CGFloat {
        min(ChatLayout.contentWidth, max(1, scrollView.contentView.bounds.width - 44))
    }
    private var offsetY: CGFloat { scrollView.contentView.bounds.minY }
    private var minimumOffsetY: CGFloat { constrainedOffsetY(towardBottom: false) }
    private var maximumOffsetY: CGFloat {
        layoutCoordinator.constrainedOffsetY(towardBottom: true)
    }
    private var bottomDistance: CGFloat { max(0, maximumOffsetY - offsetY) }

    private func constrainedOffsetY(towardBottom: Bool) -> CGFloat {
        layoutCoordinator.constrainedOffsetY(towardBottom: towardBottom)
    }
}

extension MacAgentTranscriptViewController: NSCollectionViewDataSource {
    func numberOfSections(in collectionView: NSCollectionView) -> Int { 1 }

    func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int {
        blocks.count
    }

    func collectionView(
        _ collectionView: NSCollectionView,
        itemForRepresentedObjectAt indexPath: IndexPath
    ) -> NSCollectionViewItem {
        guard blocks.indices.contains(indexPath.item), let theme else { return NSCollectionViewItem() }
        let block = blocks[indexPath.item]
        switch block.content {
        case .text:
            let item = collectionView.makeItem(withIdentifier: Self.textIdentifier, for: indexPath)
            applyPresentation(for: block, to: item, theme: theme)
            return item
        case .card:
            let item = collectionView.makeItem(withIdentifier: Self.cardIdentifier, for: indexPath)
            applyPresentation(for: block, to: item, theme: theme)
            return item
        }
    }
}

private extension MacAgentTranscriptViewController {
    func applyPresentation(
        for block: TranscriptBlock,
        to item: NSCollectionViewItem,
        theme: MarkdownTheme
    ) {
        switch block.content {
        case .text:
            guard let item = item as? TranscriptTextCollectionItem else { return }
            configureTextItem(item, block: block, theme: theme)
        case .card:
            guard let item = item as? TranscriptCardCollectionItem,
                  let card = cards[block.itemID]
            else { return }
            item.configure(card: card)
            item.setAccessibilityPresented(selectionIndexByID[block.id].map {
                selectionBlocks[$0] == block
            } ?? false)
        }
    }

    func selectedCopyText() -> String? {
        selectionController.copiedText(
            blocks: completedPresentation.blocks,
            sourcePlan: completedPresentation.sourcePlan,
            indexByID: completedPresentation.indexByID
        )
    }

    func selectionBegan(
        blockID: TranscriptBlockID,
        offset: Int,
        initialRange: NSRange?
    ) {
        guard let index = selectionIndexByID[blockID] else { return }
        cancelSelectionTracking()
        if let initialRange {
            selectionController.select(
                range: initialRange,
                in: blockID,
                blockIndex: index,
                blocks: selectionBlocks
            )
        } else {
            selectionController.begin(
                at: TranscriptSelectionPosition(blockID: blockID, utf16Offset: offset),
                blockIndex: index,
                blocks: selectionBlocks
            )
        }
        animations[blockID]?.finalize()
        _ = view.window?.makeFirstResponder(collectionView)
        refreshVisibleSelection()
        // A new gesture ends the old reply; a ready range (double-click word)
        // has no drag, so it shows now instead of at mouse-up.
        updateQuoteReply()
    }

    func selectionDragged(_ event: NSEvent) {
        selectionController.dragMoved()
        lastSelectionDragEvent = event
        guard updateSelectionDrag(with: event) else {
            stopSelectionAutoscroll()
            return
        }
        guard selectionAutoscrollTimer == nil else { return }
        let timer = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let event = self.lastSelectionDragEvent else { return }
                if !self.updateSelectionDrag(with: event) {
                    self.stopSelectionAutoscroll()
                }
            }
        }
        selectionAutoscrollTimer = timer
        RunLoop.main.add(timer, forMode: .eventTracking)
        RunLoop.main.add(timer, forMode: .common)
    }

    func selectionEnded(_ event: NSEvent) {
        // A press forwarded from empty space anchors at the nearest text, but
        // the release maps by row and may land on a card, which would extend
        // to the paragraph's end. Only a drag moves the extent.
        if selectionController.didDrag { _ = updateSelectionDrag(with: event) }
        selectionController.endDragging()
        cancelSelectionTracking()
        refreshVisibleSelection()
        updateQuoteReply()
    }

    func cancelSelectionTracking() {
        stopSelectionAutoscroll()
        selectionController.endDragging()
    }

    func stopSelectionAutoscroll() {
        selectionAutoscrollTimer?.invalidate()
        selectionAutoscrollTimer = nil
        lastSelectionDragEvent = nil
    }

    /// Returns whether AppKit should keep repeating autoscroll for a stationary
    /// pointer outside the viewport.
    func updateSelectionDrag(with event: NSEvent) -> Bool {
        guard selectionController.isDragging else { return false }
        guard !renderTransactions.isBusy else {
            // Keep the repeating drag timer alive, but do not ask AppKit to
            // autoscroll against the staged collection generation. The queued
            // viewport commit drains after presentation promotion, and the next
            // timer tick resumes native autoscroll on completed geometry.
            readerIntentEpoch &+= 1
            markDirty(
                [.selectionAutoscroll, .viewport],
                viewportTarget: .unchanged
            )
            return true
        }
        debugNativeSelectionAutoscrollCount += 1
        let offsetBeforeAutoscroll = offsetY
        let didAutoscroll = suppliedSelectionAutoscroller?(collectionView, event)
            ?? collectionView.autoscroll(with: event)
        if offsetY < offsetBeforeAutoscroll - 0.5 {
            setMode(.detached)
        }
        readerIntentEpoch &+= 1
        markDirty(
            [.selectionAutoscroll, .viewport],
            viewportTarget: .unchanged
        )
        let presentedBlocks = selectionBlocks
        let presentedIndex = selectionIndexByID
        let point = collectionView.convert(event.locationInWindow, from: nil)
        guard let index = transcriptLayout.nearestItemIndex(atY: point.y),
              presentedBlocks.indices.contains(index)
        else { return false }
        let block = presentedBlocks[index]
        let offset: Int
        if case .text(let text) = block.content {
            if let item = collectionView.item(at: IndexPath(item: index, section: 0))
                as? TranscriptTextCollectionItem {
                offset = item.characterIndex(at: point, from: collectionView)
            } else if let frame = transcriptLayout.layoutAttributesForItem(
                at: IndexPath(item: index, section: 0)
            )?.frame {
                offset = point.y < frame.midY ? 0 : text.source.utf16.count
            } else {
                offset = 0
            }
        } else {
            offset = 0
        }
        let extensionResult = selectionController.extend(
            to: TranscriptSelectionPosition(blockID: block.id, utf16Offset: offset),
            blockIndex: index,
            blocks: presentedBlocks,
            indexByID: presentedIndex
        )
        for (id, animation) in animations where selectionController.covers(
            blockID: id,
            blocks: presentedBlocks,
            indexByID: presentedIndex
        ) {
            animation.finalize()
        }
        refreshVisibleSelection()
        return didAutoscroll && extensionResult != .clampedAtBoundary
    }

    func quoteReplyConfigurationDidChange() {
        guard let quoteReply else {
            quoteReplyPresenter?.hide()
            quoteReplyPresenter?.configuration = nil
            return
        }
        if quoteReplyPresenter == nil {
            loadViewIfNeeded()
            let presenter = QuoteReplyPresenter()
            presenter.layer.frame = scrollView.bounds
            // A sibling of the clip view, not in the document: it stays put
            // while content scrolls, and is placed against the text by hand.
            scrollView.addSubview(presenter.layer, positioned: .above, relativeTo: scrollView.contentView)
            presenter.onReveal = { [weak self] in self?.revealQuoteSelection() }
            presenter.onAdded = { [weak self] in self?.clearSelectionAfterReply() }
            quoteReplyPresenter = presenter
        }
        quoteReplyPresenter?.configuration = quoteReply
    }

    /// Shows the reply surface for a finished, non-blank selection and hides
    /// it otherwise, including while a drag is still choosing.
    func updateQuoteReply() {
        guard let presenter = quoteReplyPresenter else { return }
        guard !selectionController.isDragging, selectionController.hasSelection,
              let text = selectedCopyText(),
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return presenter.hide() }
        presenter.show(quote: text)
        placeQuoteReply()
    }

    func placeQuoteReply() {
        guard let presenter = quoteReplyPresenter, presenter.isShown else { return }
        // NSScrollView tiles its own subviews and never autoresizes others.
        if presenter.layer.frame != scrollView.bounds { presenter.layer.frame = scrollView.bounds }
        guard let anchor = quoteAnchor(in: presenter.layer) else { return }
        let insets = scrollView.contentInsets
        let bounds = presenter.layer.bounds
        let band = NSRect(
            x: bounds.minX + insets.left,
            y: bounds.minY + insets.top,
            width: max(0, bounds.width - insets.left - insets.right),
            height: max(0, bounds.height - insets.top - insets.bottom)
        )
        presenter.place(anchor: anchor, band: band)
    }

    /// The selection's lowest line in `target`'s coordinates. Glyph rects need
    /// a materialized item; once the last selected block scrolls out of the
    /// collection's realized range, its layout frame stands in.
    func quoteAnchor(in target: NSView) -> NSRect? {
        guard let selection = selectionController.selection else { return nil }
        let anchorIndex = selectionIndexByID[selection.anchor.blockID] ?? -1
        let extentIndex = selectionIndexByID[selection.extent.blockID] ?? -1
        let lastID = extentIndex >= anchorIndex ? selection.extent.blockID : selection.anchor.blockID
        var lastIsVisible = false
        var lowest: NSRect?
        for indexPath in collectionView.indexPathsForVisibleItems() where blocks.indices.contains(indexPath.item) {
            if blocks[indexPath.item].id == lastID { lastIsVisible = true }
            guard let item = collectionView.item(at: indexPath) as? TranscriptTextCollectionItem,
                  let rect = item.selectionRect(in: target)
            else { continue }
            if lowest.map({ rect.maxY > $0.maxY }) ?? true { lowest = rect }
        }
        if lastIsVisible, let lowest { return lowest }
        return transcriptLayout.frame(for: lastID).map { collectionView.convert($0, to: target) }
    }

    /// Centers the selection in the visible band, for the pinned surface's arrow.
    func revealQuoteSelection() {
        guard let anchor = quoteAnchor(in: collectionView) else { return }
        let insets = scrollView.contentInsets
        let visibleHeight = scrollView.contentView.bounds.height - insets.top - insets.bottom
        let target = anchor.midY - insets.top - visibleHeight / 2
        setMode(.detached)
        commitLayout(target: .offset(min(max(target, minimumOffsetY), maximumOffsetY)))
        lastLiveOffsetY = offsetY
        observedOffsetY = offsetY
        placeQuoteReply()
    }

    func clearSelectionAfterReply() {
        selectionController.clear()
        refreshVisibleSelection()
        quoteReplyPresenter?.hide()
        _ = view.window?.makeFirstResponder(collectionView)
    }

    func refreshVisibleSelection() {
        let presentedBlocks = selectionBlocks
        let presentedIndex = selectionIndexByID
        for indexPath in collectionView.indexPathsForVisibleItems() {
            guard blocks.indices.contains(indexPath.item),
                  let item = collectionView.item(at: indexPath) as? TranscriptTextCollectionItem
            else { continue }
            let id = blocks[indexPath.item].id
            let range = presentedIndex[id].map { _ in
                selectionController.localRange(
                    for: id,
                    blocks: presentedBlocks,
                    indexByID: presentedIndex
                )
            } ?? NSRange(location: 0, length: 0)
            item.applySelection(range)
        }
    }

    func configureTextItem(
        _ item: TranscriptTextCollectionItem,
        block: TranscriptBlock,
        theme: MarkdownTheme
    ) {
        guard let artifact = textArtifactCache.artifact(
            for: block,
            width: readingWidth,
            theme: theme,
            resourceRoot: resourceRoot,
            options: blockOptions(for: block)
        ) else { return }
        let fadeBatches = debugDisablesStreamingFades
            ? []
            : animations[block.id]?.active() ?? []
        item.delegate = self
        item.apply(TranscriptTextPresentation(
            itemID: block.itemID,
            artifact: artifact,
            fadeBatches: fadeBatches,
            selectedRange: selectionController.localRange(
                for: block.id,
                blocks: selectionBlocks,
                indexByID: selectionIndexByID
            ),
            isAccessibilityPresented: selectionIndexByID[block.id].map {
                selectionBlocks[$0] == block
            } ?? false
        ))
    }

    func isBlockSelected(_ id: TranscriptBlockID) -> Bool {
        selectionController.covers(
            blockID: id,
            blocks: selectionBlocks,
            indexByID: selectionIndexByID
        )
    }
}

extension MacAgentTranscriptViewController: TranscriptTextCollectionItemDelegate {
    func transcriptTextItemHasSelection(_ item: TranscriptTextCollectionItem) -> Bool {
        selectionController.hasSelection
    }

    func transcriptTextItem(
        _ item: TranscriptTextCollectionItem,
        beganAt offset: Int,
        initialRange: NSRange?,
        blockID: TranscriptBlockID
    ) {
        selectionBegan(blockID: blockID, offset: offset, initialRange: initialRange)
    }

    func transcriptTextItem(_ item: TranscriptTextCollectionItem, dragged event: NSEvent) {
        selectionDragged(event)
    }

    func transcriptTextItem(_ item: TranscriptTextCollectionItem, ended event: NSEvent) {
        selectionEnded(event)
    }

    func transcriptTextItemCancelledSelection(_ item: TranscriptTextCollectionItem) {
        cancelSelectionTracking()
    }

    func transcriptTextItem(
        _ item: TranscriptTextCollectionItem,
        attachmentLayoutChangedFor blockID: TranscriptBlockID,
        measuredHeight: CGFloat
    ) {
        imagePayloadDidChange(id: blockID, height: measuredHeight)
    }

    func transcriptTextItem(_ item: TranscriptTextCollectionItem, copyMessage itemID: String) {
        guard let value = copyText(itemID) else { return }
        copy(value)
    }

    func transcriptTextItem(_ item: TranscriptTextCollectionItem, canCopyMessage itemID: String) -> Bool {
        copyText(itemID) != nil
    }

    func transcriptTextItem(
        _ item: TranscriptTextCollectionItem,
        toggleExpansionFor blockID: TranscriptBlockID
    ) {
        toggleCodeBlockExpansion(blockID)
    }

    func transcriptTextItem(_ item: TranscriptTextCollectionItem, openLink url: URL) {
        openLink(url)
    }
}

extension MacAgentTranscriptViewController {


    static func appendOnlyChange(from previous: [TranscriptItem], to next: [TranscriptItem]) -> (itemID: String, delta: String)? {
        guard previous.count == next.count else { return nil }
        var change: (itemID: String, delta: String)?
        for (old, new) in zip(previous, next) where old != new {
            guard change == nil, old.id == new.id, old.relation == new.relation,
                  case .markdown(let before, let oldSyntax, let oldContainer) = old.content,
                  case .markdown(let after, let syntax, let container) = new.content,
                  oldSyntax == syntax, oldContainer == container,
                  after.utf8.count > before.utf8.count, after.utf8.starts(with: before.utf8) else { return nil }
            change = (new.id, String(decoding: after.utf8.dropFirst(before.utf8.count), as: UTF8.self))
        }
        return change
    }



    func appendProjectedText(_ delta: String, to itemID: String) -> Bool {
        appendProjectedText(
            delta,
            to: itemID,
            settlement: .none
        )
    }

    private func appendProjectedText(
        _ delta: String,
        to itemID: String,
        settlement: TranscriptRenderTransactionCoordinator.Settlement
    ) -> Bool {
        guard !renderTransactions.isBusy,
              !renderTransactions.isTornDown,
              let theme,
              readingWidth == lastMeasuredWidth
        else { return false }
        let anchor = mode == .followingBottom || userIsLiveScrolling ? nil : captureAnchor()
        let transactionID = renderTransactions.reserveTransactionID()
        var prepareInterval: TranscriptSignpost.IntervalToken? = TranscriptSignpost.beginRenderPrepare(
            transactionID: transactionID
        )
        defer {
            if let prepareInterval {
                TranscriptSignpost.endRenderPrepare(
                    prepareInterval,
                    projectedBlocks: blocks.count,
                    artifactCount: textArtifactCache.count
                )
            }
        }
        guard let mutation = projectionStore.append(delta, to: itemID) else { return false }
        let oldRange = mutation.oldRange
        let previousItemBlocks = mutation.previousBlocks
        let nextItemBlocks = mutation.replacementBlocks
        let previousByID = Dictionary(uniqueKeysWithValues: previousItemBlocks.map { ($0.id, $0) })
        let reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        for block in nextItemBlocks where !isBlockSelected(block.id) {
            guard case .text(let current) = block.content,
                  let old = previousByID[block.id],
                  case .text(let prior) = old.content,
                  current.source.hasPrefix(prior.source),
                  current.source.utf16.count > prior.source.utf16.count
            else { continue }
            let range = NSRange(
                location: prior.source.utf16.count,
                length: current.source.utf16.count - prior.source.utf16.count
            )
            let coordinator = animations[block.id] ?? LiveBatchAnimationCoordinator()
            coordinator.append(range: range, reduceMotion: reduceMotion)
            animations[block.id] = coordinator
        }
        for staleID in mutation.removedIDs {
            heightCache.removeValue(forKey: staleID)
            textArtifactCache.remove([staleID])
            animations.removeValue(forKey: staleID)?.finalize()
        }
        let width = readingWidth
        for block in nextItemBlocks {
            guard case .text = block.content else { continue }
            let options = blockOptions(for: block)
            let height = textArtifactCache.artifact(
                for: block,
                width: width,
                theme: theme,
                resourceRoot: resourceRoot,
                options: options
            )?.measuredHeight ?? 1
            heightCache[block.id] = HeightCacheEntry(
                content: block.content,
                width: width,
                theme: theme,
                resourceRoot: resourceRoot,
                height: height,
                blockOptions: options
            )
        }
        let difference = collectionDifference(
            from: previousItemBlocks,
            to: nextItemBlocks,
            indexOffset: mutation.indexOffset
        )
        transcriptLayout.replaceEntries(
            in: oldRange,
            with: nextItemBlocks.map {
                .init(id: $0.id, height: cachedHeight(for: $0.id), relation: $0.relation, block: $0.markdownNeighbor)
            }
        )
        if let interval = prepareInterval {
            TranscriptSignpost.endRenderPrepare(
                interval,
                projectedBlocks: blocks.count,
                artifactCount: textArtifactCache.count
            )
            prepareInterval = nil
        }
        let result = renderTransactions.submit(.init(
            transactionID: transactionID,
            collectionMutation: difference.mutation,
            forceFullLayout: false,
            viewportTarget: viewportTarget(preserving: anchor),
            viewportEpoch: readerIntentEpoch,
            refreshIDs: difference.refreshIDs,
            refreshAllMaterializedItems: difference.mutation.mode == .batch,
            candidatePresentation: .init(
                sourcePlan: sourcePlan,
                blocks: blocks,
                indexByID: blockIndexByID
            ),
            settlement: settlement
        ))
        guard result != .busy, result != .tornDown else { return false }
        if difference.mutation.mode == .batch {
            debugBatchUpdateCount += 1
            debugNonanimatedStreamingUpdateCount += 1
        }
        return true
    }
}

@MainActor
final class TranscriptCardCollectionItem: NSCollectionViewItem {
    private weak var card: (any TranscriptCard)?
    private weak var cardView: NSView?

    override func loadView() { view = NSView() }

    override func preferredLayoutAttributesFitting(
        _ layoutAttributes: NSCollectionViewLayoutAttributes
    ) -> NSCollectionViewLayoutAttributes {
        layoutAttributes
    }

    func configure(card: any TranscriptCard) {
        if self.card === card, let cardView {
            // Same host, possibly new payload: the store adopts a changed spec
            // into the existing card rather than replacing it, so the identity
            // still has to be re-applied — a trailer that goes from running to
            // finished trades `chat.run-status` for a different identifier, or
            // for none at all.
            applyAccessibilityIdentity(of: card, to: cardView)
            return
        }
        prepareForReuse()
        let hostedView = card.makeHostedView()
        self.card = card
        cardView = hostedView
        view.addSubview(hostedView)
        hostedView.frame = view.bounds
        hostedView.autoresizingMask = [.width, .height]
        applyAccessibilityIdentity(of: card, to: hostedView)
    }

    /// Assigning nil when the card declares no placeholder is the point: a stale
    /// identifier left on a reused host makes a settled turn keep announcing the
    /// state it had when the host was first configured.
    private func applyAccessibilityIdentity(of card: any TranscriptCard, to hostedView: NSView) {
        hostedView.setAccessibilityIdentifier(card.accessibilityPlaceholder?.identifier)
        hostedView.setAccessibilityValue(card.accessibilityPlaceholder?.value)
    }

    func setAccessibilityPresented(_ presented: Bool) {
        cardView?.setAccessibilityHidden(!presented)
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        if card?.hostedView === cardView { card?.hostedView = nil }
        cardView?.removeFromSuperview()
        card = nil
        cardView = nil
    }
}
