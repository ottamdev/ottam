import AppKit
import QuartzCore
import Testing
@testable import OttamApp

@Suite("Thread spinner")
@MainActor
struct ThreadSpinnerTests {
    private func fixture() -> (NSWindow, ThreadSpinnerView) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 40, height: 40),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let view = ThreadSpinnerView()
        view.frame = NSRect(x: 0, y: 0, width: 16, height: 16)
        window.contentView?.addSubview(view)
        view.layout()
        return (window, view)
    }

    // Occlusion is not reported for a never-ordered test window, so the cycle
    // is seeded directly; the tests cover removal and geometry.
    private func seedCycle(_ view: ThreadSpinnerView) {
        view.layer!.add(CABasicAnimation(keyPath: "strokeEnd"), forKey: "thread")
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

    @Test("Reduce Motion, hiding, zero size and detaching remove the cycle")
    func lifecycle() {
        let (window, view) = fixture()
        defer { window.close() }
        seedCycle(view)
        #expect(view.isAnimating)
        view.animates = false
        #expect(!view.isAnimating)
        view.animates = true
        seedCycle(view)
        view.isHidden = true
        #expect(!view.isAnimating)
        view.isHidden = false
        seedCycle(view)
        view.setFrameSize(.zero)
        view.layout()
        #expect(!view.isAnimating)
        view.setFrameSize(NSSize(width: 16, height: 16))
        view.layout()
        seedCycle(view)
        view.removeFromSuperview()
        #expect(!view.isAnimating)
        #expect(view.thread?.path != nil)
    }

    @Test("the spinner is invisible to accessibility and hit testing")
    func passthrough() {
        let (window, view) = fixture()
        defer { window.close() }
        #expect(view.isAccessibilityElement() == false)
        #expect(view.hitTest(NSPoint(x: 8, y: 8)) == nil)
    }
}
