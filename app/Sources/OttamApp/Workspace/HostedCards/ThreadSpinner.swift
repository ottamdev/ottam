import AppKit
import QuartzCore
import SwiftUI

/// The thread mark as a small indeterminate progress indicator: the thread
/// draws in, pulls out the far side, and repeats.
///
/// It stands in for the system spinner only on a running turn's trailer, so
/// the transcript's working state carries the brand mark; controls keep
/// `ProgressView` (see the status vocabulary rule in
/// `docs/architecture/app/macos-ui.md`).
///
/// Core Animation runs the stroke, so nothing ticks the hosted transcript's
/// SwiftUI graph or the main thread while it plays (see `ChatShimmerText`).
/// Reduce Motion shows the still mark.
struct ThreadSpinner: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ThreadSpinnerRepresentable(animates: !reduceMotion)
            .frame(width: 16, height: 16)
            .accessibilityHidden(true)
    }
}

private struct ThreadSpinnerRepresentable: NSViewRepresentable {
    let animates: Bool

    func makeNSView(context: Context) -> ThreadSpinnerView {
        let view = ThreadSpinnerView()
        view.animates = animates
        return view
    }

    func updateNSView(_ view: ThreadSpinnerView, context: Context) {
        view.animates = animates
    }

    static func dismantleNSView(_ view: ThreadSpinnerView, coordinator: ()) {
        view.stopObservingWindow()
    }
}

/// The backing layer is the thread's `CAShapeLayer`, so AppKit keeps it
/// attached and flips it to the mark's top-left geometry.
final class ThreadSpinnerView: WindowVisibleAnimationView {
    private static let animationKey = "thread"
    private static let travel: TimeInterval = 0.7
    private static let hold: TimeInterval = 0.1
    private static let gap: TimeInterval = 0.2
    private static let period = 2 * travel + hold + gap
    /// Optical sizing at 16pt: a translucent 8-unit stroke antialiases thin
    /// next to text, so the spinner draws heavier than mark.svg, with tails a
    /// little longer to keep the thread's proportions.
    static let strokeUnits: CGFloat = 9.5
    private static let tail: CGFloat = 38

    /// The bounds size the current path was built for.
    private var pathSize: CGSize?

    var animates = true {
        didSet { if animates != oldValue { updateAnimation() } }
    }

    var thread: CAShapeLayer? { layer as? CAShapeLayer }
    var isAnimating: Bool { layer?.animation(forKey: Self.animationKey) != nil }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { nil }

    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func makeBackingLayer() -> CALayer {
        let thread = CAShapeLayer()
        thread.fillColor = nil
        thread.lineCap = .round
        thread.lineJoin = .round
        return thread
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyStrokeColor()
    }

    private func applyStrokeColor() {
        guard let thread else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            thread.strokeColor = NSColor.secondaryLabelColor.cgColor
        }
        CATransaction.commit()
    }

    override func layout() {
        super.layout()
        // A replaced backing layer arrives without a path.
        if let thread, bounds.width > 0, bounds.height > 0,
           thread.path == nil || pathSize != bounds.size {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            thread.path = OttamThreadMark.centerline(in: bounds, tail: Self.tail).cgPath
            thread.lineWidth = OttamThreadMark.lineWidth(in: bounds, units: Self.strokeUnits)
            CATransaction.commit()
            pathSize = bounds.size
            applyStrokeColor()
        }
        updateAnimation()
    }

    override func removeAnimation() {
        layer?.removeAnimation(forKey: Self.animationKey)
    }

    override func updateAnimation() {
        guard animates, canAnimate, let thread, thread.path != nil else {
            removeAnimation()
            return
        }
        guard !isAnimating else { return }

        let period = Self.period
        let travelIn = Self.travel / period
        let pullStart = (Self.travel + Self.hold) / period
        let pullEnd = (2 * Self.travel + Self.hold) / period
        let ease = CAMediaTimingFunction(controlPoints: 0.65, 0, 0.35, 1)
        let linear = CAMediaTimingFunction(name: .linear)

        let end = CAKeyframeAnimation(keyPath: "strokeEnd")
        end.values = [0, 1, 1]
        end.keyTimes = [0, NSNumber(value: travelIn), 1]
        end.timingFunctions = [ease, linear]

        let start = CAKeyframeAnimation(keyPath: "strokeStart")
        start.values = [0, 0, 1, 1]
        start.keyTimes = [0, NSNumber(value: pullStart), NSNumber(value: pullEnd), 1]
        start.timingFunctions = [linear, ease, linear]

        let cycle = CAAnimationGroup()
        cycle.animations = [end, start]
        cycle.duration = period
        cycle.repeatCount = .infinity
        cycle.beginTime = Self.sharedPhaseBeginTime(for: thread, period: period)
        thread.add(cycle, forKey: Self.animationKey)
    }
}
