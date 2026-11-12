import AppKit

@MainActor
final class OcclusionMarkerOverlay {
    private let panel: NSPanel

    init(frame: CGRect) {
        panel = NSPanel(
            contentRect: frame,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.backgroundColor = NSColor(
            calibratedRed: 1,
            green: 0,
            blue: 1,
            alpha: 1
        )
        panel.isOpaque = true
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.level = .screenSaver
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    }

    func show() {
        panel.orderFrontRegardless()
    }

    func close() {
        panel.orderOut(nil)
        panel.close()
    }
}

@MainActor
public final class VirtualCursorOverlay {
    private let panel: NSPanel

    public init(point: CGPoint) {
        panel = NSPanel(
            contentRect: CGRect(x: point.x - 12, y: point.y - 12, width: 24, height: 24),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.contentView = CursorView(
            frame: CGRect(x: 0, y: 0, width: 24, height: 24)
        )
    }

    public var ignoresMouseEvents: Bool {
        panel.ignoresMouseEvents
    }

    public func show() {
        panel.orderFrontRegardless()
    }

    public func close() {
        panel.orderOut(nil)
        panel.close()
    }
}

private final class CursorView: NSView {
    override func draw(_ dirtyRect: NSRect) {
        NSColor.systemBlue.withAlphaComponent(0.85).setStroke()
        let path = NSBezierPath(ovalIn: bounds.insetBy(dx: 3, dy: 3))
        path.lineWidth = 3
        path.stroke()
    }
}
