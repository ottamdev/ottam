import AppKit
import QuartzCore
import Testing
@testable import OttamApp

/// A never-ordered test window reports no occlusion state, so the window
/// counts as visible whenever the view is in one.
private final class VisibleThreadSpinnerView: ThreadSpinnerView {
    override var isWindowVisible: Bool { window != nil }
}

@Suite("Thread spinner")
@MainActor
struct ThreadSpinnerTests {
    private func fixture() -> (NSWindow, ThreadSpinnerView) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 40, height: 40),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let view = VisibleThreadSpinnerView()
        view.frame = NSRect(x: 0, y: 0, width: 16, height: 16)
        window.contentView?.addSubview(view)
        view.layout()
        return (window, view)
    }

    @Test("the backing layer is the flipped thread at the spinner's optical weight")
    func geometry() throws {
        let (window, view) = fixture()
        defer { window.close() }
        let thread = try #require(view.thread)
        #expect(view.layer === thread)
        #expect(view.isFlipped)
        let path = try #require(thread.path)
        #expect(view.bounds.contains(path.boundingBox))
        // The tails sit below the loop in the mark's top-left geometry.
        #expect(path.currentPoint.y > view.bounds.midY)
        #expect(abs(thread.lineWidth - ThreadSpinnerView.strokeUnits * 16 / 100) < 0.001)
        // Without the cycle the model shows the whole, still mark.
        #expect(thread.strokeStart == 0 && thread.strokeEnd == 1)
        #expect(thread.lineCap == .round)
    }

    @Test("the stroke follows the effective appearance")
    func appearance() throws {
        let (window, view) = fixture()
        defer { window.close() }
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let appearance = try #require(NSAppearance(named: name))
            view.appearance = appearance
            var expected: CGColor?
            appearance.performAsCurrentDrawingAppearance {
                expected = NSColor.secondaryLabelColor.cgColor
            }
            #expect(view.thread?.strokeColor == expected)
        }
    }

    @Test("a visible spinner runs one repeating draw-in and pull-out cycle")
    func cycle() throws {
        let (window, view) = fixture()
        defer { window.close() }
        let cycle = try #require(view.layer?.animation(forKey: "thread") as? CAAnimationGroup)
        #expect(cycle.repeatCount == .infinity)
        #expect(abs(cycle.duration - 1.7) < 0.001)
        let keyPaths = (cycle.animations ?? []).compactMap { ($0 as? CAPropertyAnimation)?.keyPath }
        #expect(Set(keyPaths) == ["strokeStart", "strokeEnd"])
    }

    @Test("Reduce Motion, hiding, zero size and detaching each stop the cycle, and undoing them restarts it")
    func lifecycle() {
        let (window, view) = fixture()
        defer { window.close() }
        #expect(view.isAnimating)

        view.animates = false
        #expect(!view.isAnimating)
        view.animates = true
        #expect(view.isAnimating)

        view.isHidden = true
        #expect(!view.isAnimating)
        view.isHidden = false
        #expect(view.isAnimating)

        view.setFrameSize(.zero)
        view.layout()
        #expect(!view.isAnimating)
        view.setFrameSize(NSSize(width: 16, height: 16))
        view.layout()
        #expect(view.isAnimating)

        view.removeFromSuperview()
        #expect(!view.isAnimating)
        #expect(view.thread?.path != nil)
        window.contentView?.addSubview(view)
        #expect(view.isAnimating)
    }

    @Test("the spinner is invisible to accessibility and hit testing")
    func passthrough() {
        let (window, view) = fixture()
        defer { window.close() }
        #expect(view.isAccessibilityElement() == false)
        #expect(view.hitTest(NSPoint(x: 8, y: 8)) == nil)
    }
}
