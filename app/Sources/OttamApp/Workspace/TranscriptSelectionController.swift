import Foundation

struct TranscriptSelectionPosition: Equatable, Sendable {
    let blockID: TranscriptBlockID
    let utf16Offset: Int
}

struct TranscriptSelection: Equatable, Sendable {
    let anchor: TranscriptSelectionPosition
    let extent: TranscriptSelectionPosition
}

/// Canonical selection state for the virtualized native transcript.
///
/// Collection items are disposable projections. This controller keeps the
/// logical endpoints in stable block coordinates so scrolling and reuse cannot
/// lose a selection, while cards remain hard boundaries between text regions.
final class TranscriptSelectionController {
    enum ExtensionResult: Equatable {
        case extended
        case clampedAtBoundary
        case ignored
    }

    private(set) var selection: TranscriptSelection?
    private(set) var focusedItemID: String?
    private(set) var isDragging = false
    /// Whether the pointer has moved since the press began. A press that
    /// never moved ends as a click, a caret whatever lies under the release.
    private(set) var didDrag = false

    private var region: ClosedRange<Int>?

    var hasSelection: Bool {
        guard let selection else { return false }
        return selection.anchor != selection.extent
    }

    func begin(
        at position: TranscriptSelectionPosition,
        blockIndex: Int,
        blocks: [TranscriptBlock]
    ) {
        guard blocks.indices.contains(blockIndex),
              blocks[blockIndex].id == position.blockID,
              case .text = blocks[blockIndex].content
        else {
            clear()
            return
        }
        let normalized = normalized(position, in: blocks[blockIndex])
        selection = TranscriptSelection(anchor: normalized, extent: normalized)
        focusedItemID = blocks[blockIndex].itemID
        region = selectableRegion(containing: blockIndex, blocks: blocks)
        isDragging = true
        didDrag = false
    }

    func select(
        range: NSRange,
        in blockID: TranscriptBlockID,
        blockIndex: Int,
        blocks: [TranscriptBlock]
    ) {
        guard blocks.indices.contains(blockIndex), blocks[blockIndex].id == blockID,
              case .text = blocks[blockIndex].content
        else {
            clear()
            return
        }
        let lower = normalized(
            TranscriptSelectionPosition(blockID: blockID, utf16Offset: range.location),
            in: blocks[blockIndex]
        )
        let upper = normalized(
            TranscriptSelectionPosition(blockID: blockID, utf16Offset: NSMaxRange(range)),
            in: blocks[blockIndex]
        )
        selection = TranscriptSelection(anchor: lower, extent: upper)
        focusedItemID = blocks[blockIndex].itemID
        region = selectableRegion(containing: blockIndex, blocks: blocks)
        isDragging = false
        didDrag = false
    }

    @discardableResult
    func extend(
        to proposed: TranscriptSelectionPosition,
        blockIndex proposedIndex: Int,
        blocks: [TranscriptBlock],
        indexByID: [TranscriptBlockID: Int]
    ) -> ExtensionResult {
        guard let current = selection,
              let anchorIndex = indexByID[current.anchor.blockID],
              blocks.indices.contains(anchorIndex),
              blocks.indices.contains(proposedIndex)
        else { return .ignored }
        let bounds = region ?? selectableRegion(containing: anchorIndex, blocks: blocks)
        region = bounds

        let clampedIndex = min(max(proposedIndex, bounds.lowerBound), bounds.upperBound)
        let reachedBoundary = clampedIndex != proposedIndex || blocks[proposedIndex].isSelectionBoundary
        let clamped: TranscriptSelectionPosition
        if reachedBoundary {
            let movesForward = proposedIndex >= anchorIndex
            let targetIndex = movesForward ? bounds.upperBound : bounds.lowerBound
            guard blocks.indices.contains(targetIndex) else { return .ignored }
            clamped = TranscriptSelectionPosition(
                blockID: blocks[targetIndex].id,
                utf16Offset: movesForward ? blocks[targetIndex].selectionTextLength : 0
            )
        } else {
            clamped = normalized(proposed, in: blocks[clampedIndex])
        }
        selection = TranscriptSelection(anchor: current.anchor, extent: clamped)
        return reachedBoundary ? .clampedAtBoundary : .extended
    }

    /// The pointer moved during the press, so its release extends the
    /// selection like any drag. Ignored once no press is dragging.
    func dragMoved() {
        if isDragging { didDrag = true }
    }

    func endDragging() {
        isDragging = false
        didDrag = false
    }

    func clear() {
        selection = nil
        focusedItemID = nil
        region = nil
        isDragging = false
        didDrag = false
    }

    func reconcile(
        blocks: [TranscriptBlock],
        indexByID: [TranscriptBlockID: Int]
    ) {
        guard let current = selection,
              let anchorIndex = indexByID[current.anchor.blockID],
              let extentIndex = indexByID[current.extent.blockID],
              blocks.indices.contains(anchorIndex),
              blocks.indices.contains(extentIndex),
              !blocks[anchorIndex].isSelectionBoundary,
              !blocks[extentIndex].isSelectionBoundary
        else {
            clear()
            return
        }

        let anchor = normalized(current.anchor, in: blocks[anchorIndex])
        region = selectableRegion(containing: anchorIndex, blocks: blocks)
        guard let region else {
            clear()
            return
        }
        let proposedExtent = normalized(current.extent, in: blocks[extentIndex])
        let targetIndex = min(max(extentIndex, region.lowerBound), region.upperBound)
        let extent: TranscriptSelectionPosition
        if targetIndex == extentIndex {
            extent = proposedExtent
        } else {
            let forward = extentIndex >= anchorIndex
            extent = TranscriptSelectionPosition(
                blockID: blocks[targetIndex].id,
                utf16Offset: forward ? blocks[targetIndex].selectionTextLength : 0
            )
        }
        selection = TranscriptSelection(anchor: anchor, extent: extent)
    }

    func localRange(
        for blockID: TranscriptBlockID,
        blocks: [TranscriptBlock],
        indexByID: [TranscriptBlockID: Int]
    ) -> NSRange {
        guard let ordered = orderedEndpoints(blocks: blocks, indexByID: indexByID),
              let index = indexByID[blockID],
              index >= ordered.lowerIndex,
              index <= ordered.upperIndex,
              blocks.indices.contains(index),
              case .text = blocks[index].content
        else { return NSRange(location: 0, length: 0) }

        let length = blocks[index].selectionTextLength
        let lower = index == ordered.lowerIndex ? ordered.lower.utf16Offset : 0
        let upper = index == ordered.upperIndex ? ordered.upper.utf16Offset : length
        return NSRange(location: min(lower, length), length: max(0, min(upper, length) - min(lower, length)))
    }

    func covers(
        blockID: TranscriptBlockID,
        blocks: [TranscriptBlock],
        indexByID: [TranscriptBlockID: Int]
    ) -> Bool {
        guard let ordered = orderedEndpoints(blocks: blocks, indexByID: indexByID),
              let index = indexByID[blockID]
        else { return false }
        return index >= ordered.lowerIndex && index <= ordered.upperIndex
    }

    func copiedText(
        blocks: [TranscriptBlock],
        sourcePlan: [TranscriptItem],
        indexByID: [TranscriptBlockID: Int]
    ) -> String? {
        guard hasSelection,
              let ordered = orderedEndpoints(blocks: blocks, indexByID: indexByID)
        else { return nil }

        let sources = Dictionary(uniqueKeysWithValues: sourcePlan.compactMap { item in
            item.markdownText.map { (item.id, $0) }
        })
        var fragments: [String] = []
        var index = ordered.lowerIndex
        while index <= ordered.upperIndex {
            guard case .text = blocks[index].content else {
                index += 1
                continue
            }
            let itemID = blocks[index].itemID
            let selectedStart = index
            var selectedEnd = index
            while selectedEnd + 1 <= ordered.upperIndex,
                  blocks[selectedEnd + 1].itemID == itemID,
                  !blocks[selectedEnd + 1].isSelectionBoundary {
                selectedEnd += 1
            }

            let allItemIndices = blocks.indices.filter {
                blocks[$0].itemID == itemID && !blocks[$0].isSelectionBoundary
            }
            let firstItemIndex = allItemIndices.first
            let lastItemIndex = allItemIndices.last
            let firstRange = localRange(for: blocks[selectedStart].id, blocks: blocks, indexByID: indexByID)
            let lastRange = localRange(for: blocks[selectedEnd].id, blocks: blocks, indexByID: indexByID)
            let selectsWholeItem = firstItemIndex == selectedStart
                && lastItemIndex == selectedEnd
                && firstRange.location == 0
                && NSMaxRange(lastRange) == blocks[selectedEnd].selectionTextLength

            if selectsWholeItem, let source = sources[itemID] {
                fragments.append(source)
            } else {
                let rendered = (selectedStart...selectedEnd).compactMap { blockIndex -> String? in
                    guard case .text(let text) = blocks[blockIndex].content else { return nil }
                    let range = localRange(
                        for: blocks[blockIndex].id,
                        blocks: blocks,
                        indexByID: indexByID
                    )
                    guard range.length > 0 else { return nil }
                    let substring = (text.source as NSString).substring(with: range)
                    // A whole code block copies as the fence it came from;
                    // a partial range stays the code the reader dragged over.
                    if range.location == 0,
                       range.length == blocks[blockIndex].selectionTextLength {
                        switch text.node?.kind {
                        case .codeBlock(let hint, _)?:
                            return Self.fencedSource(substring, hint: hint)
                        case .table(let header, let rows, let alignments)?:
                            // Likewise a whole table copies as a pipe table;
                            // a partial range stays the tab-separated cells.
                            return TranscriptTableSource.pipeTable(
                                header: header, rows: rows, alignments: alignments
                            )
                        default:
                            break
                        }
                    }
                    return substring
                }.joined(separator: "\n")
                if !rendered.isEmpty { fragments.append(rendered) }
            }
            index = selectedEnd + 1
        }
        return fragments.isEmpty ? nil : fragments.joined(separator: "\n\n")
    }

    /// Re-fences code with a delimiter longer than any backtick run inside it,
    /// so the result parses back to the same block.
    static func fencedSource(_ code: String, hint: String?) -> String {
        var longestRun = 0
        var run = 0
        for character in code {
            run = character == "`" ? run + 1 : 0
            longestRun = max(longestRun, run)
        }
        let delimiter = String(repeating: "`", count: max(3, longestRun + 1))
        return "\(delimiter)\(hint ?? "")\n\(code)\n\(delimiter)"
    }

    private func orderedEndpoints(
        blocks: [TranscriptBlock],
        indexByID: [TranscriptBlockID: Int]
    ) -> (lower: TranscriptSelectionPosition, lowerIndex: Int, upper: TranscriptSelectionPosition, upperIndex: Int)? {
        guard let selection,
              let anchorIndex = indexByID[selection.anchor.blockID],
              let extentIndex = indexByID[selection.extent.blockID]
        else { return nil }
        if anchorIndex < extentIndex
            || anchorIndex == extentIndex && selection.anchor.utf16Offset <= selection.extent.utf16Offset {
            return (selection.anchor, anchorIndex, selection.extent, extentIndex)
        }
        return (selection.extent, extentIndex, selection.anchor, anchorIndex)
    }

    private func selectableRegion(
        containing anchorIndex: Int,
        blocks: [TranscriptBlock]
    ) -> ClosedRange<Int> {
        var lower = anchorIndex
        while lower > 0, !blocks[lower - 1].isSelectionBoundary { lower -= 1 }
        var upper = anchorIndex
        while upper + 1 < blocks.count, !blocks[upper + 1].isSelectionBoundary { upper += 1 }
        return lower...upper
    }

    private func normalized(
        _ position: TranscriptSelectionPosition,
        in block: TranscriptBlock
    ) -> TranscriptSelectionPosition {
        guard case .text(let text) = block.content else {
            return TranscriptSelectionPosition(blockID: position.blockID, utf16Offset: 0)
        }
        let value = text.source as NSString
        var offset = min(max(0, position.utf16Offset), value.length)
        if offset > 0, offset < value.length {
            let sequence = value.rangeOfComposedCharacterSequence(at: offset)
            if offset > sequence.location { offset = sequence.location }
        }
        return TranscriptSelectionPosition(
            blockID: position.blockID,
            utf16Offset: offset
        )
    }
}

private extension TranscriptBlock {
    var isSelectionBoundary: Bool {
        if case .card = content { return true }
        return false
    }

    var selectionTextLength: Int {
        guard case .text(let text) = content else { return 0 }
        return text.source.utf16.count
    }
}
