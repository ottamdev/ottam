import AppKit
import QuartzCore

/// Bounded presentation-only state for streamed glyph fades. Text ownership,
/// layout and copy never consult this object.
@MainActor
final class LiveBatchAnimationCoordinator {
    struct Batch: Equatable, Identifiable {
        let id: UInt64
        let range: NSRange
        let startedAt: CFTimeInterval
        let duration: CFTimeInterval

        func opacity(at time: CFTimeInterval) -> CGFloat {
            let progress = min(1, max(0, (time - startedAt) / duration))
            return 0.35 + CGFloat(progress) * 0.65
        }

        func remaining(at time: CFTimeInterval) -> CFTimeInterval {
            max(0, duration - (time - startedAt))
        }
    }

    static let duration: CFTimeInterval = 0.120
    static let maximumBatches = 4
    static let maximumUTF16Units = 512

    private(set) var batches: [Batch] = []
    private var nextID: UInt64 = 1

    func append(range: NSRange, reduceMotion: Bool, now: CFTimeInterval = CACurrentMediaTime()) {
        prune(now: now)
        guard !reduceMotion, range.length > 0 else { return }
        batches.append(Batch(id: nextID, range: range, startedAt: now, duration: Self.duration))
        nextID &+= 1
        while batches.count > Self.maximumBatches || batches.reduce(0, { $0 + $1.range.length }) > Self.maximumUTF16Units {
            batches.removeFirst()
        }
    }

    func active(at now: CFTimeInterval = CACurrentMediaTime()) -> [Batch] {
        prune(now: now)
        return batches
    }

    func finalize() { batches.removeAll(keepingCapacity: true) }

    private func prune(now: CFTimeInterval) {
        batches.removeAll { $0.remaining(at: now) <= 0 }
    }
}
