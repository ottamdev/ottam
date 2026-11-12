import Foundation
import OttamMarkdownEngine

/// Parses and measures a large settled transcript away from the main actor.
/// The controller still publishes one exact geometry snapshot on main; no
/// estimated heights ever reach `NSCollectionView`.
@MainActor
final class TranscriptMeasurementService {
    struct Request: @unchecked Sendable {
        /// Fully sendable transcript structure. The unchecked part of this
        /// request is limited to immutable AppKit font/color values in `theme`.
        let items: [TranscriptProjectionItem]
        let width: CGFloat
        let theme: MarkdownTheme
        let resourceRoot: URL?
        /// Long code blocks the reader has expanded; everything else folds.
        var expandedCodeBlocks: Set<TranscriptBlockID> = []
        /// Legacy horizontal scroller strip, captured on main.
        var horizontalScrollerInset: CGFloat = 0
    }

    struct Result: @unchecked Sendable {
        let projector: TranscriptBlockProjector
        let blocks: [TranscriptBlock]
        let textHeights: [TranscriptBlockID: CGFloat]
        let width: CGFloat
        let themeSignature: String
        let resourceRoot: URL?
    }

    private final class Job: @unchecked Sendable {
        private let lock = NSLock()
        private var cancelled = false

        func cancel() {
            lock.withLock { cancelled = true }
        }

        var isCancelled: Bool {
            lock.withLock { cancelled }
        }
    }

    private let queue = DispatchQueue(
        label: "dev.ottam.transcript-measurement",
        qos: .userInitiated
    )
    private var currentJob: Job?

    func prepare(
        _ request: Request,
        completion: @escaping @MainActor @Sendable (Result) -> Void
    ) {
        currentJob?.cancel()
        let job = Job()
        currentJob = job
        queue.async { [weak self] in
            guard !job.isCancelled else { return }
            let projector = TranscriptBlockProjector()
            let blocks = projector.project(request.items)
            guard !job.isCancelled else { return }
            var heights: [TranscriptBlockID: CGFloat] = [:]
            heights.reserveCapacity(blocks.count)
            for block in blocks {
                guard !job.isCancelled else { return }
                guard case .text(let text) = block.content else { continue }
                // The same options the main thread caches beside this height
                // (`MacAgentTranscriptViewController.blockOptions`): a quoted
                // block's lead-in is part of its height, so leaving it out
                // here would install a short frame the cache then trusts.
                let continuation = block.quoteContinuation(spacing: request.theme.metrics.blockSpacing)
                heights[block.id] = TranscriptTextMeasurement.height(
                    for: text,
                    width: request.width,
                    theme: request.theme,
                    resourceRoot: request.resourceRoot,
                    options: .init(
                        isExpanded: request.expandedCodeBlocks.contains(block.id),
                        horizontalScrollerInset: request.horizontalScrollerInset,
                        quoteLeadIn: continuation.leadIn,
                        continuedQuoteDepth: continuation.depth
                    )
                )
            }
            guard !job.isCancelled else { return }
            let result = Result(
                projector: projector,
                blocks: blocks,
                textHeights: heights,
                width: request.width,
                themeSignature: request.theme.signature,
                resourceRoot: request.resourceRoot
            )
            Task { @MainActor [weak self] in
                guard let self, self.currentJob === job, !job.isCancelled else { return }
                self.currentJob = nil
                completion(result)
            }
        }
    }

    func cancel() {
        currentJob?.cancel()
        currentJob = nil
    }
}
