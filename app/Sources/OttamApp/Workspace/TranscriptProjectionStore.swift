import Foundation

/// Renderer-neutral ownership for the source plan and its stable projected
/// blocks. Indexes are rebuilt atomically with structural mutations so AppKit,
/// selection, and caches never observe a mismatched array/index pair.
final class TranscriptProjectionStore {
    struct Mutation {
        let previousBlocks: [TranscriptBlock]
        let replacementBlocks: [TranscriptBlock]
        let oldRange: Range<Int>
        let newRange: Range<Int>
        let removedIDs: Set<TranscriptBlockID>
        let firstInvalidatedIndex: Int
        let identitiesChanged: Bool

        var indexOffset: Int { oldRange.lowerBound }
    }

    private(set) var projector = TranscriptBlockProjector()
    private(set) var sourcePlan: [TranscriptItem] = []
    private(set) var blocks: [TranscriptBlock] = []
    private(set) var indexByID: [TranscriptBlockID: Int] = [:]
    private(set) var itemIndexByID: [String: Int] = [:]
    private(set) var blockRangeByItemID: [String: Range<Int>] = [:]

    @discardableResult
    func replace(
        plan: [TranscriptItem],
        resetProjector: Bool,
        preparedProjector: TranscriptBlockProjector? = nil,
        preparedBlocks: [TranscriptBlock]? = nil
    ) -> Mutation {
        let old = blocks
        if let preparedProjector {
            projector = preparedProjector
        } else if resetProjector {
            projector.reset()
        }
        let new = preparedBlocks ?? projector.project(plan)
        sourcePlan = plan
        blocks = new
        rebuildIndexes()
        let firstChanged = Self.firstChangedIndex(from: old, to: new)
        return Mutation(
            previousBlocks: old,
            replacementBlocks: new,
            oldRange: 0..<old.count,
            newRange: 0..<new.count,
            removedIDs: Set(old.map(\.id)).subtracting(new.map(\.id)),
            firstInvalidatedIndex: firstChanged,
            identitiesChanged: old.map(\.id) != new.map(\.id)
        )
    }

    func append(_ delta: String, to itemID: String) -> Mutation? {
        guard let itemIndex = itemIndexByID[itemID],
              sourcePlan.indices.contains(itemIndex),
              let itemRange = blockRangeByItemID[itemID],
              projector.append(delta, to: itemID, in: &sourcePlan),
              let update = projector.projectedUpdate(
                itemID: itemID,
                relation: sourcePlan[itemIndex].relation
              )
        else { return nil }

        let start = itemRange.lowerBound + update.startIndex
        let oldRange = start..<itemRange.upperBound
        let previous = Array(blocks[oldRange])
        let replacement = update.blocks
        blocks.replaceSubrange(oldRange, with: replacement)
        let newRange = start..<(start + replacement.count)
        let identitiesChanged = previous.map(\.id) != replacement.map(\.id)
        if identitiesChanged { rebuildIndexes() }
        let firstChanged = Self.firstChangedIndex(from: previous, to: replacement)
        return Mutation(
            previousBlocks: previous,
            replacementBlocks: replacement,
            oldRange: oldRange,
            newRange: newRange,
            removedIDs: Set(previous.map(\.id)).subtracting(replacement.map(\.id)),
            firstInvalidatedIndex: start + firstChanged,
            identitiesChanged: identitiesChanged
        )
    }

    func reset() {
        projector.reset()
        sourcePlan.removeAll()
        blocks.removeAll()
        indexByID.removeAll()
        itemIndexByID.removeAll()
        blockRangeByItemID.removeAll()
    }

    private func rebuildIndexes() {
        indexByID = Dictionary(uniqueKeysWithValues: blocks.indices.map { (blocks[$0].id, $0) })
        itemIndexByID = Dictionary(uniqueKeysWithValues: sourcePlan.indices.map {
            (sourcePlan[$0].id, $0)
        })
        blockRangeByItemID.removeAll(keepingCapacity: true)
        var index = 0
        while index < blocks.count {
            let itemID = blocks[index].itemID
            var upper = index + 1
            while upper < blocks.count, blocks[upper].itemID == itemID { upper += 1 }
            blockRangeByItemID[itemID] = index..<upper
            index = upper
        }
    }

    private static func firstChangedIndex(
        from old: [TranscriptBlock],
        to new: [TranscriptBlock]
    ) -> Int {
        var index = 0
        while index < old.count, index < new.count, old[index] == new[index] { index += 1 }
        return index
    }
}
