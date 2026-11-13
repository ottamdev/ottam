import AppKit
import QuartzCore
import SwiftUI

/// Claude Code-style shimmer for in-flight labels: the secondary-colored text
/// carries a brighter copy of itself revealed by a band that sweeps left to right.
///
/// SwiftUI renders both copies, so typography, truncation, baseline, bidi and
/// fallback glyphs match by construction. Only the band moves, and it moves as
/// a Core Animation mask: nothing ticks the hosted transcript's SwiftUI graph.
/// The tested SwiftUI-driven variants repeatedly triggered window constraint
/// and workspace layout work through the nested hosting view. See the transcript
/// performance protocol for measurements and the related report FB13810482.
struct ChatShimmerText: View {
    let text: String
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var label: some View {
        Text(text)
            .appFont(.body)
            .lineLimit(1)
    }

    var body: some View {
        label
            .overlay {
                if !reduceMotion {
                    ShimmerHighlight(text: text) { label.foregroundStyle(.primary) }
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(text)
            .allowsHitTesting(false)
    }
}

/// Hosts the bright copy in a static `NSHostingView` under a wrapper whose layer
/// mask is the moving band. The wrapper fills the overlay exactly, so the hosted
/// label lays out in the same frame as the visible one.
private struct ShimmerHighlight<Content: View>: NSViewRepresentable {
    let text: String
    @ViewBuilder let content: Content

    func makeNSView(context: Context) -> ChatShimmerHighlightView {
        ChatShimmerHighlightView(rootView: root(context), text: text)
    }

    func updateNSView(_ view: ChatShimmerHighlightView, context: Context) {
        // The visible twin crossfades through SwiftUI when the card animates a
        // label change; the hosted copy has no transaction, so fade its layer.
        view.setRootView(root(context), text: text, animated: context.transaction.animation != nil)
    }

    static func dismantleNSView(_ view: ChatShimmerHighlightView, coordinator: ()) {
        view.stopObservingWindow()
    }

    /// Copy the caller's environment across the AppKit boundary explicitly so the
    /// hosted label resolves typography and styles like its visible twin.
    private func root(_ context: Context) -> AnyView {
        AnyView(content.environment(\.self, context.environment))
    }
}

final class ChatShimmerHighlightView: WindowVisibleAnimationView {
    private static let animationKey = "shimmer"
    private static let period: TimeInterval = 1.8

    private let hosting: NSHostingView<AnyView>
    private let band = CAGradientLayer()

    /// Core Animation files every `CATransition` under this key regardless of
    /// the key passed to `add(_:forKey:)`.
    private static let fadeKey = kCATransition
    private static let fadeDuration: TimeInterval = 0.2

    private(set) var text: String

    var rootView: AnyView {
        get { hosting.rootView }
        set { hosting.rootView = newValue }
    }

    init(rootView: AnyView, text: String = "") {
        hosting = NSHostingView(rootView: rootView)
        self.text = text
        super.init(frame: .zero)
        wantsLayer = true
        hosting.wantsLayer = true
        hosting.sizingOptions = []
        hosting.translatesAutoresizingMaskIntoConstraints = true
        hosting.autoresizingMask = [.width, .height]
        addSubview(hosting)
        band.colors = [NSColor.clear.cgColor, NSColor.white.cgColor, NSColor.clear.cgColor]
        band.locations = [0, 0.5, 1]
        band.startPoint = CGPoint(x: 0, y: 0.5)
        band.endPoint = CGPoint(x: 1, y: 0.5)
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { nil }

    /// Replaces the hosted label. When the text changes inside an animated
    /// SwiftUI update, the hosted layer crossfades for the same 0.2 s the card
    /// uses, so the band never lights up new glyphs over a half-faded twin.
    func setRootView(_ root: AnyView, text: String, animated: Bool) {
        let changed = text != self.text
        self.text = text
        if changed, animated, let layer = hosting.layer {
            let fade = CATransition()
            fade.type = .fade
            fade.duration = Self.fadeDuration
            fade.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            layer.add(fade, forKey: Self.fadeKey)
        }
        hosting.rootView = root
    }

    var isFadingText: Bool { hosting.layer?.animation(forKey: Self.fadeKey) != nil }

    /// The visible label already speaks for this text.
    override func accessibilityChildren() -> [Any]? { [] }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        hosting.frame = bounds
        // AppKit may replace the backing layer while the view is detached, so
        // the mask is re-attached here rather than assumed from init.
        guard let layer, bounds.width > 0, bounds.height > 0 else {
            band.removeAnimation(forKey: Self.animationKey)
            return
        }
        let bandFrame = CGRect(x: -bandWidth, y: 0, width: bandWidth, height: bounds.height)
        if layer.mask !== band || band.frame != bandFrame {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            band.frame = bandFrame
            layer.mask = band
            CATransaction.commit()
            band.removeAnimation(forKey: Self.animationKey)
        }
        updateAnimation()
    }

    private var bandWidth: CGFloat { max(bounds.width * 0.45, 40) }

    override func removeAnimation() {
        band.removeAnimation(forKey: Self.animationKey)
    }

    override func updateAnimation() {
        guard canAnimate, layer?.mask === band else {
            removeAnimation()
            return
        }
        guard band.animation(forKey: Self.animationKey) == nil else { return }
        let sweep = CABasicAnimation(keyPath: "transform.translation.x")
        sweep.fromValue = 0
        sweep.toValue = bounds.width + bandWidth
        sweep.duration = Self.period
        sweep.repeatCount = .infinity
        sweep.timingFunction = CAMediaTimingFunction(name: .linear)
        // Preserve the shared sweep phase when labels change or reattach.
        sweep.beginTime = Self.sharedPhaseBeginTime(for: band, period: Self.period)
        band.add(sweep, forKey: Self.animationKey)
    }

    var isSweeping: Bool { band.animation(forKey: Self.animationKey) != nil }
}
