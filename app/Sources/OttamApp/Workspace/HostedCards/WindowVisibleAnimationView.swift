import AppKit
import QuartzCore

/// Base for a view whose looping Core Animation runs only while it can be
/// seen: its window is visible, it is not hidden, and it has a size. AppKit
/// lifecycle callbacks and window occlusion changes call `updateAnimation()`;
/// subclasses add or remove their animation there.
class WindowVisibleAnimationView: NSView {
    /// Whether the window is on screen. Tests override it: a never-ordered
    /// window reports no occlusion state.
    var isWindowVisible: Bool { window?.occlusionState.contains(.visible) == true }

    var canAnimate: Bool {
        isWindowVisible
            && !isHiddenOrHasHiddenAncestor
            && bounds.width > 0 && bounds.height > 0
    }

    /// Adds the animation when `canAnimate` and the subclass is ready, and
    /// removes it otherwise.
    @objc func updateAnimation() {}

    func removeAnimation() {}

    /// A `beginTime` for `layer` that puts every animation of `period` on one
    /// wall-clock phase, so a rebuilt or reattached view does not restart it.
    static func sharedPhaseBeginTime(for layer: CALayer, period: TimeInterval) -> CFTimeInterval {
        layer.convertTime(CACurrentMediaTime(), from: nil)
            - Date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: period)
    }

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
        removeAnimation()
    }

    override func viewDidHide() {
        super.viewDidHide()
        updateAnimation()
    }

    override func viewDidUnhide() {
        super.viewDidUnhide()
        updateAnimation()
    }
}
