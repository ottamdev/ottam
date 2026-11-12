import AppKit
import OttamAccessibility
import OttamGhostty
import SwiftUI

struct TerminalWorkspaceView: NSViewRepresentable {
    let surfaceController: TerminalSurfaceController
    let isActive: Bool

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> NSView {
        let view = TerminalSurfaceContainerView()
        view.setAccessibilityElement(true)
        view.setAccessibilityIdentifier(AccessibilityID.terminalSurface)
        view.setAccessibilityRole(.group)
        view.setAccessibilityLabel("Terminal")
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        guard let container = nsView as? TerminalSurfaceContainerView else {
            return
        }
        let controllerChanged = context.coordinator.attachedController !== surfaceController
        if context.coordinator.attachedController !== surfaceController {
            context.coordinator.attachedController?.detach(from: container)
            surfaceController.attach(to: container)
            context.coordinator.attachedController = surfaceController
        } else {
            surfaceController.attach(to: container)
        }

        if isActive && (controllerChanged || context.coordinator.isActive != true) {
            surfaceController.activateFocus()
        } else if !isActive && context.coordinator.isActive != false {
            surfaceController.deactivateFocus()
        }
        context.coordinator.isActive = isActive
    }

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        if let container = nsView as? TerminalSurfaceContainerView {
            coordinator.attachedController?.detach(from: container)
        }
        coordinator.attachedController = nil
    }

    final class Coordinator {
        var attachedController: TerminalSurfaceController?
        var isActive: Bool?
    }
}

/// What a Ghostty surface needs from whatever is driving it: bytes out, bytes
/// in, and a grid size. The workspace's own terminals are backed by
/// `TerminalSessionModel`; a revealed sign-in terminal is backed by its own,
/// much smaller model, and the surface cannot tell the difference.
@MainActor
protocol TerminalSurfaceBacking: AnyObject {
    func attachOutputSink(_ sink: @escaping (Data) -> Void) -> TerminalOutputSinkToken
    func detachOutputSink(token: TerminalOutputSinkToken)
    func writeInput(_ data: Data)
    func resize(rows: UInt16, cols: UInt16)
}

@MainActor
final class TerminalSurfaceController {
    private let model: any TerminalSurfaceBacking
    private let hostView: GhosttyTerminalHostView
    private var outputSinkToken: TerminalOutputSinkToken?
    private weak var visibleContainer: TerminalSurfaceContainerView?
    private var isDestroyed = false

    init(
        model: any TerminalSurfaceBacking,
        workingDirectory: String
    ) {
        self.model = model
        let hostView = GhosttyTerminalHostView(
            configuration: GhosttyTerminalConfiguration(
                title: "Terminal",
                workingDirectory: workingDirectory,
                onManualInput: { [weak model] data in
                    Task { @MainActor in
                        model?.writeInput(data)
                    }
                },
                onResize: { [weak model] rows, cols in
                    Task { @MainActor in
                        model?.resize(rows: rows, cols: cols)
                    }
                }
            )
        )
        self.hostView = hostView
        outputSinkToken = model.attachOutputSink { [weak hostView] data in
            hostView?.processOutput(data)
        }
    }

    deinit {
        MainActor.assumeIsolated {
            destroy()
        }
    }

    func attach(to container: TerminalSurfaceContainerView) {
        guard !isDestroyed else { return }
        if visibleContainer !== container {
            detachFromVisibleContainer()
        }
        container.install(hostView)
        visibleContainer = container
        hostView.setTerminalResizeReportingEnabled(true)
    }

    /// Detaches only when `container` still shows this surface. The same
    /// terminal can move between hosts (a route and a conversation's tools
    /// card); SwiftUI may dismantle the old host after the new one attached,
    /// and that late dismantle must not pull the surface out of the new host.
    func detach(from container: TerminalSurfaceContainerView) {
        guard visibleContainer === container else { return }
        detachFromVisibleContainer()
    }

    func detachFromVisibleContainer() {
        guard !isDestroyed else { return }
        deactivateFocus()
        hostView.setTerminalResizeReportingEnabled(false)
        if hostView.superview === visibleContainer {
            hostView.removeFromSuperview()
        } else if hostView.superview != nil {
            hostView.removeFromSuperview()
        }
        visibleContainer = nil
    }

    func activateFocus() {
        guard !isDestroyed else { return }
        hostView.activateTerminalFocus()
    }

    func deactivateFocus() {
        hostView.deactivateTerminalFocus()
    }

    func destroy() {
        guard !isDestroyed else { return }
        detachFromVisibleContainer()
        if let outputSinkToken {
            model.detachOutputSink(token: outputSinkToken)
            self.outputSinkToken = nil
        }
        hostView.teardownTerminalSurface()
        isDestroyed = true
    }
}

final class TerminalSurfaceContainerView: NSView {
    override var acceptsFirstResponder: Bool { false }

    func install(_ hostView: GhosttyTerminalHostView) {
        if hostView.superview !== self {
            subviews
                .filter { $0 !== hostView }
                .forEach { $0.removeFromSuperview() }
            hostView.removeFromSuperview()
            hostView.translatesAutoresizingMaskIntoConstraints = false
            addSubview(hostView)
            NSLayoutConstraint.activate([
                hostView.leadingAnchor.constraint(equalTo: leadingAnchor),
                hostView.trailingAnchor.constraint(equalTo: trailingAnchor),
                hostView.topAnchor.constraint(equalTo: topAnchor),
                hostView.bottomAnchor.constraint(equalTo: bottomAnchor)
            ])
        }
    }
}
