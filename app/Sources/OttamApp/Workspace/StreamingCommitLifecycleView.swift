import AppKit
import SwiftUI

/// Zero-layout AppKit lifecycle host for the renderer-neutral stream clock.
struct StreamingCommitLifecycleView: NSViewRepresentable {
    let coordinator: StreamingCommitCoordinator

    /// Structural commits per second for streamed Markdown. Fixed, not a
    /// setting: it divides 60 and 120 Hz panels evenly, and the DEBUG override
    /// below is the hook for experiments.
    private static let structuralCommitRate: Float = 30

    private var structuralRate: Float {
        #if DEBUG
        if let rawValue = ProcessInfo.processInfo.environment["OTTAM_DEBUG_STREAMING_TEXT_FPS"],
           let framesPerSecond = Float(rawValue),
           framesPerSecond >= 1 {
            return framesPerSecond
        }
        #endif
        return Self.structuralCommitRate
    }

    func makeNSView(context: Context) -> HostView {
        let view = HostView()
        view.onWindowChange = { [weak view] in
            guard let view, let coordinator = view.commitCoordinator else { return }
            coordinator.startIfNeeded(on: view)
        }
        configure(view)
        return view
    }

    func updateNSView(_ view: HostView, context: Context) {
        configure(view)
    }

    static func dismantleNSView(_ view: HostView, coordinator: ()) {
        view.commitCoordinator?.detach(from: view)
        view.commitCoordinator = nil
        view.onWindowChange = nil
    }

    private func configure(_ view: HostView) {
        // Switching conversations hands the view another store's coordinator;
        // the previous one stops ticking for a store no longer shown.
        if let previous = view.commitCoordinator, previous !== coordinator {
            previous.detach(from: view)
        }
        view.commitCoordinator = coordinator
        coordinator.structuralRate = structuralRate
        coordinator.startIfNeeded(on: view)
    }

    final class HostView: NSView {
        weak var commitCoordinator: StreamingCommitCoordinator?
        var onWindowChange: (() -> Void)?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            onWindowChange?()
        }

        override var intrinsicContentSize: NSSize { .zero }
    }
}
