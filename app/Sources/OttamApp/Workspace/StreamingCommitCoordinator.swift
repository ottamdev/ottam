import AppKit
import OttamInstrumentation
import QuartzCore

/// One native display-link source for bounded structural Markdown commits.
/// The selected display-link rate is the commit clock; it stays paused while
/// no buffered text is waiting.
@MainActor
final class StreamingCommitCoordinator: NSObject {
    weak var view: NSView?
    var structuralRate: Float = 30 {
        didSet {
            guard structuralRate != oldValue else { return }
            updateFrameRateRange()
        }
    }
    var drainStructural: (() -> Void)?
    var hasStructuralWork: (() -> Bool)?
    var pendingStructuralMetrics: (() -> (bytes: Int, deltas: Int))?

    private(set) var displayLink: CADisplayLink?
    private(set) var screenObserver: NSObjectProtocol?
    private weak var observedWindow: NSWindow?
    private var previousTickTimestamp: TimeInterval?

    func startIfNeeded(on view: NSView) {
        if let current = self.view, current !== view {
            invalidateLink()
        }
        self.view = view
        observeScreenChanges(for: view.window)
        guard displayLink == nil, let screen = view.window?.screen else { return }
        let link = view.displayLink(target: self, selector: #selector(frameTick(_:)))
        applyFrameRateRange(to: link, maximumScreenRate: screen.maximumFramesPerSecond)
        link.add(to: .main, forMode: .common)
        link.isPaused = !(hasStructuralWork?() ?? false)
        displayLink = link
    }

    func wake() {
        guard let view else { return }
        if displayLink == nil { startIfNeeded(on: view) }
        displayLink?.isPaused = false
    }

    @objc private func frameTick(_ link: CADisplayLink) {
        let pending = pendingStructuralMetrics?() ?? (bytes: 0, deltas: 0)
        let payload = TranscriptSignpost.DisplayTickPayload.calculate(
            configuredHz: Int(structuralRate),
            physicalHz: view?.window?.screen?.maximumFramesPerSecond ?? Int(structuralRate),
            timestamp: link.timestamp,
            previousTimestamp: previousTickTimestamp,
            pendingBytes: pending.bytes,
            pendingDeltas: pending.deltas
        )
        previousTickTimestamp = link.timestamp
        TranscriptSignpost.displayTick(payload)
        guard view != nil else {
            link.isPaused = true
            return
        }
        guard hasStructuralWork?() == true else {
            link.isPaused = true
            return
        }
        drainStructural?()
        if hasStructuralWork?() != true {
            link.isPaused = true
        }
    }

    func invalidate() {
        invalidateLink()
        view = nil
    }

    func detach(from view: NSView) {
        guard self.view === view else { return }
        invalidate()
    }

    deinit {
        displayLink?.invalidate()
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
    }

    private func updateFrameRateRange() {
        guard let link = displayLink else { return }
        applyFrameRateRange(
            to: link,
            maximumScreenRate: view?.window?.screen?.maximumFramesPerSecond ?? Int(structuralRate)
        )
    }

    private func applyFrameRateRange(to link: CADisplayLink, maximumScreenRate: Int) {
        let rate = Float(max(1, min(maximumScreenRate, Int(structuralRate))))
        link.preferredFrameRateRange = CAFrameRateRange(
            minimum: rate,
            maximum: rate,
            preferred: rate
        )
    }

    private func observeScreenChanges(for window: NSWindow?) {
        if observedWindow === window,
           (window == nil) == (screenObserver == nil) {
            return
        }
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
        observedWindow = window
        guard let window else {
            screenObserver = nil
            return
        }
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeScreenNotification,
            object: window,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let view = self.view else { return }
                if self.displayLink == nil {
                    self.startIfNeeded(on: view)
                } else {
                    self.updateFrameRateRange()
                }
            }
        }
    }

    private func invalidateLink() {
        displayLink?.invalidate()
        displayLink = nil
        previousTickTimestamp = nil
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
        screenObserver = nil
        observedWindow = nil
    }
}
