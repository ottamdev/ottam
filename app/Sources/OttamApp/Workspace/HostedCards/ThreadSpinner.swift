import AppKit
import QuartzCore
import SwiftUI

/// The thread mark as a small indeterminate progress indicator: the thread
/// draws in, pulls out the far side, and repeats.
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

final class ThreadSpinnerView: NSView {
    private static let animationKey = "thread"
    private static let travel: TimeInterval = 0.7
    private static let hold: TimeInterval = 0.1
    private static let gap: TimeInterval = 0.2
    private static let period = 2 * travel + hold + gap
    /// Optical sizing at 16pt: a translucent 8-unit stroke antialiases thin
    /// next to text, so the spinner draws heavier than mark.svg, with tails a
    /// little longer to keep the thread's proportions.
    private static let strokeUnits: CGFloat = 9.5
    private static let tail: CGFloat = 38

    private let thread = CAShapeLayer()

    var animates = true {
        didSet { if animates != oldValue { restartAnimation() } }
    }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        thread.fillColor = nil
        thread.lineCap = .round
        thread.lineJoin = .round
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { nil }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        stopObservingWindow()
        if let window {
            NotificationCenter.default.addObserver(self, selector: #selector(updateAnimation),
                name: NSWindow.didChangeOcclusionStateNotification, object: window)
        }
        needsLayout = true
        updateAnimation()
    }

    func stopObservingWindow() {
        NotificationCenter.default.removeObserver(self, name: NSWindow.didChangeOcclusionStateNotification, object: nil)
        thread.removeAnimation(forKey: Self.animationKey)
    }

    override func viewDidHide() {
        super.viewDidHide()
        updateAnimation()
    }

    override func viewDidUnhide() {
        super.viewDidUnhide()
        updateAnimation()
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsLayout = true
    }

    override func layout() {
        super.layout()
        guard let layer, bounds.width > 0, bounds.height > 0 else {
            thread.removeAnimation(forKey: Self.animationKey)
            return
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        // AppKit may replace the backing layer while the view is detached.
        if thread.superlayer !== layer { layer.addSublayer(thread) }
        thread.frame = bounds
        // The mark's geometry is top-left origin; the backing layer is not.
        var flip = CGAffineTransform(a: 1, b: 0, c: 0, d: -1, tx: 0, ty: bounds.height)
        thread.path = OttamThreadMark.centerline(in: bounds, tail: Self.tail).cgPath.copy(using: &flip)
        thread.lineWidth = OttamThreadMark.lineWidth(in: bounds, units: Self.strokeUnits)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            thread.strokeColor = NSColor.secondaryLabelColor.cgColor
        }
        CATransaction.commit()
        updateAnimation()
    }

    private func restartAnimation() {
        thread.removeAnimation(forKey: Self.animationKey)
        updateAnimation()
    }

    @objc private func updateAnimation() {
        let visible = window?.occlusionState.contains(.visible) == true && !isHiddenOrHasHiddenAncestor
        guard animates, visible, thread.superlayer != nil, thread.path != nil else {
            thread.removeAnimation(forKey: Self.animationKey)
            return
        }
        guard thread.animation(forKey: Self.animationKey) == nil else { return }

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
        // Every spinner shares one phase, so a rebuilt row does not restart it.
        cycle.beginTime = thread.convertTime(CACurrentMediaTime(), from: nil)
            - Date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: period)
        thread.add(cycle, forKey: Self.animationKey)
    }

    var isAnimating: Bool { thread.animation(forKey: Self.animationKey) != nil }
}
