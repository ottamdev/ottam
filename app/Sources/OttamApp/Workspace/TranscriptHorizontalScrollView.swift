import AppKit

/// The horizontal scroller inside a code or table item. It owns sideways
/// gestures only: a gesture that starts mostly vertical, or arrives while
/// nothing overflows, goes to the transcript instead of stalling on a tile
/// that cannot move that way. The choice is made once per gesture so a
/// diagonal drag never flips owners mid-way.
///
/// Overriding `scrollWheel` here opts this inner view out of responsive
/// scrolling, which is fine for a tile; the outer transcript scroll view
/// deliberately keeps AppKit's default.
final class TranscriptHorizontalScrollView: NSScrollView {
    private var forwardsCurrentGesture = false

    var hasHorizontalOverflow: Bool {
        guard let documentView else { return false }
        return documentView.frame.width > contentView.bounds.width + 1
    }

    override func scrollWheel(with event: NSEvent) {
        let startsGesture = event.phase == .began
        let isDiscreteWheel = event.phase == [] && event.momentumPhase == []
        if startsGesture || isDiscreteWheel {
            forwardsCurrentGesture = !hasHorizontalOverflow
                || abs(event.scrollingDeltaY) >= abs(event.scrollingDeltaX)
        }
        if forwardsCurrentGesture {
            nextResponder?.scrollWheel(with: event)
        } else {
            super.scrollWheel(with: event)
        }
    }
}
