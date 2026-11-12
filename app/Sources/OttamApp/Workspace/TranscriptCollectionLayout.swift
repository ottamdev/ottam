import AppKit
import OttamInstrumentation
import OttamMarkdownEngine

@MainActor
final class TranscriptCollectionLayoutInvalidationContext: NSCollectionViewLayoutInvalidationContext {
    var dirtyStart: Int?
    var scope: TranscriptSignpost.CollectionInvalidationScope?
    var requestedItemIndexes: [Int] = []
    var origin = "custom"
}

/// Exact-height one-column collection layout for the transcript.
///
/// Heights are known before publication, so the scroll view never has to
/// revise its document estimate as offscreen rows materialize. Frames are
/// cheap values for the whole loaded page; views are requested only for the
/// visible rectangle plus `prefetchMargin`.
@MainActor
final class TranscriptCollectionLayout: NSCollectionViewLayout {
    struct Entry: Equatable {
        let id: TranscriptBlockID
        let height: CGFloat
        let relation: TranscriptItem.Relation
        /// The markdown block this entry shows, when it is one. Cards and
        /// node-less text have none and keep the flat `attachedSpacing`.
        var block: MarkdownBlockSpacing.Neighbor? = nil
    }

    private var storedEntries: [Entry] = []
    var entries: [Entry] {
        get { storedEntries }
        set { setEntries(newValue, invalidatingFrom: 0) }
    }
    var maximumColumnWidth: CGFloat = ChatLayout.contentWidth
    var horizontalInset: CGFloat = 22
    var verticalInset: CGFloat = 18
    var turnSpacing: CGFloat = 16
    var attachedSpacing: CGFloat = 6
    /// Gap above a markdown block within an item, keyed on the roles either
    /// side; see `MarkdownBlockSpacing`. Entries without a role fall back to
    /// `attachedSpacing`. Changing it re-lays every frame.
    var blockSpacing = MarkdownBlockSpacing() {
        didSet { if blockSpacing != oldValue { invalidateLayout() } }
    }
    var prefetchMargin: CGFloat = 600

    private var attributes: [NSCollectionViewLayoutAttributes] = []
    private var maximumYs: [CGFloat] = []
    private var indexByID: [TranscriptBlockID: Int] = [:]
    private var preparedWidth: CGFloat = -1
    private var preparedViewportHeight: CGFloat = -1
    private var contentSize = NSSize.zero
    private var dirtyStart: Int?
    private var isPerformingFullInvalidation = false
    private var nextUpdateBracketID: UInt64 = 0
    private var updateBracket: UpdateBracket?

    private struct UpdateBracket {
        let id: UInt64
        let updateItems: Int
        let earliestAffectedIndex: Int?
        let insertions: Int
        let deletions: Int
        let reloads: Int
        let moves: Int
    }

    struct DebugInvalidation: Equatable {
        let scope: String
        let dirtyStart: Int?
        let invalidatedItems: [Int]
        let contentSizeAdjustment: NSSize
        let contentOffsetAdjustment: NSPoint
    }

    private(set) var debugLastInvalidation: DebugInvalidation?

    override class var invalidationContextClass: AnyClass {
        TranscriptCollectionLayoutInvalidationContext.self
    }

    func setEntries(_ newEntries: [Entry], invalidatingFrom requestedStart: Int) {
        guard newEntries != storedEntries else { return }
        let oldCount = storedEntries.count
        storedEntries = newEntries
        indexByID = Dictionary(uniqueKeysWithValues: newEntries.indices.map {
            (newEntries[$0].id, $0)
        })
        let start = max(0, min(requestedStart, newEntries.count))
        invalidateEntries(from: start, oldCount: oldCount, newCount: newEntries.count)
    }

    func replaceEntries(in range: Range<Int>, with replacement: [Entry]) {
        guard range.lowerBound >= 0,
              range.upperBound <= storedEntries.count
        else { return }
        let oldCount = storedEntries.count
        storedEntries.replaceSubrange(range, with: replacement)
        indexByID = Dictionary(uniqueKeysWithValues: storedEntries.indices.map {
            (storedEntries[$0].id, $0)
        })
        invalidateEntries(
            from: range.lowerBound,
            oldCount: oldCount,
            newCount: storedEntries.count
        )
    }

    private func invalidateEntries(from requestedStart: Int, oldCount: Int, newCount: Int) {
        let start = max(0, min(requestedStart, newCount))
        let context = TranscriptCollectionLayoutInvalidationContext()
        context.dirtyStart = start
        context.origin = "entries"
        let survivingUpperBound = min(oldCount, newCount)
        context.requestedItemIndexes = start < survivingUpperBound
            ? Array(start..<survivingUpperBound)
            : []
        let currentItemCount = collectionView?.numberOfItems(inSection: 0) ?? 0
        let validItemIndexes = context.requestedItemIndexes.filter { $0 < currentItemCount }
        if !validItemIndexes.isEmpty {
            context.invalidateItems(at: Set(validItemIndexes.map {
                IndexPath(item: $0, section: 0)
            }))
        }
        let invalidatedCount = max(0, survivingUpperBound - start)
        context.scope = invalidatedCount == 1 ? .item : .suffix
        invalidateLayout(with: context)
    }

    override func prepare() {
        let interval = TranscriptSignpost.beginCollectionPrepare(totalItems: entries.count)
        var rebuiltItems = 0
        defer {
            TranscriptSignpost.endCollectionPrepare(interval, rebuiltItems: rebuiltItems)
        }
        super.prepare()
        guard let collectionView else { return }
        let viewportWidth = max(1, collectionView.bounds.width)
        let clipView = collectionView.enclosingScrollView?.contentView
        let contentInsets = clipView?.contentInsets ?? NSEdgeInsets()
        let viewportHeight = max(
            1,
            (clipView?.bounds.height ?? collectionView.bounds.height)
                - contentInsets.top
                - contentInsets.bottom
        )
        let widthChanged = preparedWidth != viewportWidth
        let viewportHeightChanged = preparedViewportHeight != viewportHeight
        guard widthChanged || viewportHeightChanged || dirtyStart != nil || attributes.count != entries.count
        else { return }
        preparedWidth = viewportWidth
        preparedViewportHeight = viewportHeight

        let available = max(1, viewportWidth - horizontalInset * 2)
        let width = min(maximumColumnWidth, available)
        let x = max(horizontalInset, (viewportWidth - width) / 2)
        let start = widthChanged ? 0 : min(dirtyStart ?? attributes.count, attributes.count, entries.count)
        rebuiltItems = entries.count - start
        var built = Array(attributes.prefix(start))
        var maxYs = Array(maximumYs.prefix(start))
        var y = built.last?.frame.maxY ?? verticalInset
        built.reserveCapacity(entries.count)
        maxYs.reserveCapacity(entries.count)

        for index in start..<entries.count {
            let entry = entries[index]
            if index > 0 {
                y += spacing(before: entry, after: entries[index - 1])
            }
            let value = NSCollectionViewLayoutAttributes(forItemWith: IndexPath(item: index, section: 0))
            value.frame = NSRect(x: x, y: y, width: width, height: max(1, entry.height.rounded(.up)))
            built.append(value)
            y = value.frame.maxY
            maxYs.append(y)
        }
        attributes = built
        maximumYs = maxYs
        contentSize = NSSize(width: viewportWidth, height: max(viewportHeight, y + verticalInset))
        dirtyStart = nil
    }


    private func spacing(before entry: Entry, after previous: Entry) -> CGFloat {
        guard entry.relation == .attachesToPrevious else { return turnSpacing }
        guard let previousBlock = previous.block, let block = entry.block else { return attachedSpacing }
        // A quoted block after a quoted block owns its gap: the item draws it
        // as a lead-in so the quote bar can run through (see `quoteLeadIn`).
        if blockSpacing.quoteLeadIn(from: previousBlock, to: block) > 0 { return 0 }
        return blockSpacing.gap(from: previousBlock, to: block)
    }

    override var collectionViewContentSize: NSSize { contentSize }

    override func layoutAttributesForElements(in rect: NSRect) -> [NSCollectionViewLayoutAttributes] {
        let expanded = rect.insetBy(dx: 0, dy: -prefetchMargin)
        let interval = TranscriptSignpost.beginCollectionAttributesQuery(
            requestedMinY: Double(rect.minY),
            requestedHeight: Double(rect.height),
            expandedMinY: Double(expanded.minY),
            expandedHeight: Double(expanded.height)
        )
        guard !attributes.isEmpty else {
            TranscriptSignpost.endCollectionAttributesQuery(
                interval,
                intersectingItems: 0,
                returnedItems: 0
            )
            return []
        }
        var low = 0
        var high = maximumYs.count
        while low < high {
            let mid = (low + high) / 2
            if maximumYs[mid] < expanded.minY { low = mid + 1 } else { high = mid }
        }
        var result: [NSCollectionViewLayoutAttributes] = []
        var index = low
        while index < attributes.count, attributes[index].frame.minY <= expanded.maxY {
            result.append(attributes[index])
            index += 1
        }
        let intersectingItems = TranscriptSignpost.isEnabled
            ? result.lazy.filter { $0.frame.intersects(rect) }.count
            : 0
        TranscriptSignpost.endCollectionAttributesQuery(
            interval,
            intersectingItems: intersectingItems,
            returnedItems: result.count
        )
        return result
    }

    override func layoutAttributesForItem(at indexPath: IndexPath) -> NSCollectionViewLayoutAttributes? {
        guard attributes.indices.contains(indexPath.item) else { return nil }
        return attributes[indexPath.item]
    }

    override func shouldInvalidateLayout(forBoundsChange newBounds: NSRect) -> Bool {
        newBounds.width != preparedWidth
    }

    override func invalidateLayout() {
        dirtyStart = 0
        isPerformingFullInvalidation = true
        defer { isPerformingFullInvalidation = false }
        super.invalidateLayout()
    }

    override func invalidateLayout(with context: NSCollectionViewLayoutInvalidationContext) {
        let custom = context as? TranscriptCollectionLayoutInvalidationContext
        if isPerformingFullInvalidation || context.invalidateEverything {
            dirtyStart = 0
        } else if let start = custom?.dirtyStart {
            dirtyStart = min(dirtyStart ?? start, start)
        }
        let itemIndexes = isPerformingFullInvalidation
            ? Array(entries.indices)
            : custom?.requestedItemIndexes
                ?? (context.invalidatedItemIndexPaths ?? []).map(\.item).sorted()
        let scope: TranscriptSignpost.CollectionInvalidationScope = if isPerformingFullInvalidation {
            .full
        } else {
            custom?.scope
                ?? (context.invalidateEverything ? .full : dirtyStart == nil ? .viewport : .suffix)
        }
        let requestedDirtyStart = isPerformingFullInvalidation ? 0 : custom?.dirtyStart
        debugLastInvalidation = DebugInvalidation(
            scope: scope.rawValue,
            dirtyStart: requestedDirtyStart,
            invalidatedItems: itemIndexes,
            contentSizeAdjustment: context.contentSizeAdjustment,
            contentOffsetAdjustment: context.contentOffsetAdjustment
        )
        TranscriptSignpost.collectionInvalidation(
            scope: scope,
            invalidatedItems: itemIndexes.count,
            dirtyStart: requestedDirtyStart,
            origin: isPerformingFullInvalidation
                ? "parameterless"
                : custom?.origin ?? (updateBracket == nil ? "appkit" : "appkit_update"),
            invalidateEverything: context.invalidateEverything,
            invalidateDataSourceCounts: context.invalidateDataSourceCounts,
            updateBracketID: updateBracket?.id,
            contentSizeDelta: Double(context.contentSizeAdjustment.height),
            contentOffsetDelta: Double(context.contentOffsetAdjustment.y)
        )
        super.invalidateLayout(with: context)
    }

    override func prepare(forCollectionViewUpdates updateItems: [NSCollectionViewUpdateItem]) {
        nextUpdateBracketID &+= 1
        let bracket = Self.makeUpdateBracket(id: nextUpdateBracketID, items: updateItems)
        updateBracket = bracket
        TranscriptSignpost.collectionUpdateLifecycle(
            phase: "prepare",
            bracketID: bracket.id,
            updateItems: bracket.updateItems,
            earliestAffectedIndex: bracket.earliestAffectedIndex,
            insertions: bracket.insertions,
            deletions: bracket.deletions,
            reloads: bracket.reloads,
            moves: bracket.moves
        )
        super.prepare(forCollectionViewUpdates: updateItems)
    }

    override func finalizeCollectionViewUpdates() {
        if let bracket = updateBracket {
            TranscriptSignpost.collectionUpdateLifecycle(
                phase: "finalize",
                bracketID: bracket.id,
                updateItems: bracket.updateItems,
                earliestAffectedIndex: bracket.earliestAffectedIndex,
                insertions: bracket.insertions,
                deletions: bracket.deletions,
                reloads: bracket.reloads,
                moves: bracket.moves
            )
        }
        super.finalizeCollectionViewUpdates()
        updateBracket = nil
    }

    private static func makeUpdateBracket(
        id: UInt64,
        items: [NSCollectionViewUpdateItem]
    ) -> UpdateBracket {
        var affectedIndexes: [Int] = []
        var insertions = 0
        var deletions = 0
        var reloads = 0
        var moves = 0
        for item in items {
            switch item.updateAction {
            case .insert:
                insertions += 1
                if let index = item.indexPathAfterUpdate?.item { affectedIndexes.append(index) }
            case .delete:
                deletions += 1
                if let index = item.indexPathBeforeUpdate?.item { affectedIndexes.append(index) }
            case .reload:
                reloads += 1
                if let index = item.indexPathAfterUpdate?.item
                    ?? item.indexPathBeforeUpdate?.item {
                    affectedIndexes.append(index)
                }
            case .move:
                moves += 1
                if let index = item.indexPathBeforeUpdate?.item { affectedIndexes.append(index) }
                if let index = item.indexPathAfterUpdate?.item { affectedIndexes.append(index) }
            case .none:
                break
            @unknown default:
                break
            }
        }
        return UpdateBracket(
            id: id,
            updateItems: items.count,
            earliestAffectedIndex: affectedIndexes.min(),
            insertions: insertions,
            deletions: deletions,
            reloads: reloads,
            moves: moves
        )
    }

    /// Re-query materialized items for a new visible rectangle without
    /// discarding exact heights or rebuilding the prefix-frame cache.
    func invalidateViewport() {
        TranscriptSignpost.collectionInvalidation(
            scope: .viewport,
            invalidatedItems: entries.count,
            dirtyStart: nil,
            origin: "viewport"
        )
        super.invalidateLayout()
    }

    func frame(for id: TranscriptBlockID) -> NSRect? {
        guard let index = indexByID[id] else { return nil }
        prepare()
        return attributes.indices.contains(index) ? attributes[index].frame : nil
    }

    func nearestItemIndex(atY y: CGFloat) -> Int? {
        prepare()
        guard !attributes.isEmpty else { return nil }
        if y <= attributes[0].frame.minY { return 0 }
        if y >= attributes[attributes.count - 1].frame.maxY { return attributes.count - 1 }

        var low = 0
        var high = maximumYs.count
        while low < high {
            let mid = (low + high) / 2
            if maximumYs[mid] < y { low = mid + 1 } else { high = mid }
        }
        let upper = min(low, attributes.count - 1)
        let frame = attributes[upper].frame
        guard y < frame.minY, upper > 0 else { return upper }
        let previous = attributes[upper - 1].frame
        return y - previous.maxY <= frame.minY - y ? upper - 1 : upper
    }

}
