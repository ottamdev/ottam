import AppKit
import QuartzCore
import SwiftUI
import Testing
@testable import OttamApp

@Suite("Shimmer highlight")
@MainActor
struct ChatShimmerHighlightTests {
    private func fixture(width: CGFloat = 200) -> (NSWindow, ChatShimmerHighlightView) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 300, height: 30),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let view = ChatShimmerHighlightView(rootView: AnyView(Text("Run step 12")))
        view.frame = NSRect(x: 0, y: 0, width: width, height: 20)
        window.contentView?.addSubview(view)
        view.layout()
        return (window, view)
    }

    // Occlusion is not reported for a never-ordered test window, so the sweep
    // is seeded directly; the tests cover removal and mask geometry.
    private func seedSweep(_ view: ChatShimmerHighlightView) {
        view.layer!.mask!.add(CABasicAnimation(keyPath: "transform.translation.x"), forKey: "shimmer")
    }

    @Test("the band masks the hosted label and fills the view's height")
    func maskGeometry() {
        let (window, view) = fixture()
        defer { window.close() }
        let mask = view.layer!.mask!
        #expect(mask is CAGradientLayer)
        #expect(mask.frame == CGRect(x: -90, y: 0, width: 90, height: 20))
        #expect(view.subviews.count == 1)
        #expect(view.subviews[0].frame == view.bounds)
        view.setFrameSize(NSSize(width: 60, height: 20))
        view.layout()
        #expect(mask.frame == CGRect(x: -40, y: 0, width: 40, height: 20))
        #expect(view.subviews[0].frame == view.bounds)
    }

    @Test("hiding, detaching and resizing remove the sweep; the label survives")
    func lifecycle() {
        let (window, view) = fixture()
        defer { window.close() }
        seedSweep(view)
        #expect(view.isSweeping)
        view.isHidden = true
        #expect(!view.isSweeping)
        view.isHidden = false
        seedSweep(view)
        view.setFrameSize(NSSize(width: 120, height: 20))
        view.layout()
        #expect(!view.isSweeping)
        seedSweep(view)
        view.removeFromSuperview()
        #expect(!view.isSweeping)
        window.contentView?.addSubview(view)
        view.rootView = AnyView(Text("Build"))
        view.layout()
        #expect(view.layer?.mask != nil)
        #expect(view.subviews.count == 1)
    }

    @Test("an animated text change crossfades the hosted copy; unanimated or same-text updates do not")
    func textFade() {
        let (window, view) = fixture()
        defer { window.close() }
        view.setRootView(AnyView(Text("Run step 12")), text: "Run step 12", animated: false)
        #expect(!view.isFadingText)
        view.setRootView(AnyView(Text("Run step 12")), text: "Run step 12", animated: true)
        #expect(!view.isFadingText)
        view.setRootView(AnyView(Text("Run step 13")), text: "Run step 13", animated: false)
        #expect(!view.isFadingText)
        view.setRootView(AnyView(Text("Build")), text: "Build", animated: true)
        #expect(view.isFadingText)
        #expect(view.text == "Build")
    }

    @Test("the highlight is invisible to accessibility and hit testing")
    func passthrough() {
        let (window, view) = fixture()
        defer { window.close() }
        #expect(view.isAccessibilityElement() == false)
        #expect(view.accessibilityChildren()?.isEmpty == true)
        #expect(view.hitTest(NSPoint(x: 10, y: 10)) == nil)
    }
}
