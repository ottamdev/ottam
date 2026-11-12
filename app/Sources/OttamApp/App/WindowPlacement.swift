import AppKit
import OSLog

/// Frame persistence is independent of SwiftUI's decision to reopen a scene.
@MainActor
final class WindowPlacement {
    enum Role {
        case workspace(String)
        case welcome

        var autosaveName: String {
            switch self {
            case .workspace(let id): "OttamWorkspace.\(id)"
            case .welcome: "Ottam.welcome"
            }
        }

        var positionOnly: Bool {
            if case .welcome = self { return true }
            return false
        }
    }

    static let shared = WindowPlacement()
    static var minimumWorkspaceContentSize: NSSize { WorkspaceShellViewController.minimumSize }
    static var isEnabledForCurrentLaunch: Bool {
        isEnabled(environment: ProcessInfo.processInfo.environment)
    }

    private let enabled: Bool
    private let attachedWindows = NSMapTable<NSWindow, FrameAutosaver>.weakToStrongObjects()
    private let logger = Logger(subsystem: "dev.ottam.Ottam", category: "WindowPlacement")

    init(enabled: Bool? = nil) {
        self.enabled = enabled ?? Self.isEnabledForCurrentLaunch
    }

    static func isEnabled(environment: [String: String]) -> Bool {
        environment["OTTAM_UI_TEST_MODE"] != "1"
    }

    @discardableResult
    func attach(to window: NSWindow, role: Role) -> Bool {
        guard enabled else { return false }
        if let previous = attachedWindows.object(forKey: window) { return previous.restored }

        let name = role.autosaveName
        let currentFrame = window.frame
        let minimumContentSize = NSSize(
            width: max(window.contentMinSize.width, Self.minimumWorkspaceContentSize.width),
            height: max(window.contentMinSize.height, Self.minimumWorkspaceContentSize.height)
        )
        let minimumFrameSize = window.frameRect(
            forContentRect: NSRect(origin: .zero, size: minimumContentSize)
        ).size
        // Read the original rectangle before AppKit remaps a disconnected
        // screen. That remapping must not choose the display under the mouse.
        let savedFrame = Self.savedFrame(
            UserDefaults.standard.string(forKey: "NSWindow Frame \(name)")
        )
        if savedFrame != nil {
            window.setFrameUsingName(name, force: true)
        }

        if var frame = savedFrame {
            if role.positionOnly { frame.size = currentFrame.size }
            frame = Self.constrainedFrame(
                frame,
                minimumSize: role.positionOnly ? currentFrame.size : NSSize(
                    width: max(window.minSize.width, minimumFrameSize.width),
                    height: max(window.minSize.height, minimumFrameSize.height)
                ),
                visibleFrames: NSScreen.screens.map(\.visibleFrame)
            )
            window.setFrame(frame, display: true)
        } else if window.frame != currentFrame {
            window.setFrame(currentFrame, display: true)
        }
        // AppKit-owned workspace windows need their unique native registration
        // for state restoration, including placement on macOS Spaces. SwiftUI
        // owns the welcome window's registration and may replace its name.
        Self.saveFrame(window, name: name)
        if !role.positionOnly {
            // Registration can restore again. Publish the adjusted descriptor
            // first and keep AppKit's remapping from undoing our display policy.
            let adjustedFrame = window.frame
            window.setFrameAutosaveName(name)
            if window.frame != adjustedFrame {
                window.setFrame(adjustedFrame, display: true)
                Self.saveFrame(window, name: name)
            }
        }
        attachedWindows.setObject(FrameAutosaver(window: window, name: name, restored: savedFrame != nil), forKey: window)
        logger.info("attached window=\(window.windowNumber) restored=\(savedFrame != nil)")
        return savedFrame != nil
    }

    private static func saveFrame(_ window: NSWindow, name: String) {
        // The welcome window's SwiftUI subclass redirects saveFrame(usingName:)
        // to its scene key. Use AppKit's own descriptor for both kinds of
        // window, without replacing SwiftUI's autosave behavior.
        UserDefaults.standard.set(window.frameDescriptor, forKey: "NSWindow Frame \(name)")
    }

    /// Also save under our stable key when SwiftUI assigns its own autosave
    /// name to the welcome window after the representable attaches.
    @MainActor
    private final class FrameAutosaver: NSObject {
        private weak var window: NSWindow?
        private let name: String
        let restored: Bool

        init(window: NSWindow, name: String, restored: Bool) {
            self.window = window
            self.name = name
            self.restored = restored
            super.init()
            for notification in [NSWindow.didMoveNotification, NSWindow.didResizeNotification,
                                 NSWindow.willCloseNotification] {
                NotificationCenter.default.addObserver(
                    self, selector: #selector(saveFrame), name: notification, object: window
                )
            }
        }

        @objc private func saveFrame(_ notification: Notification) {
            if let window { WindowPlacement.saveFrame(window, name: name) }
        }

        deinit {
            NotificationCenter.default.removeObserver(self)
        }
    }

    /// AppKit's native record contains window x/y/width/height followed by
    /// screen x/y/width/height. We only inspect it; AppKit owns serialization.
    static func savedFrame(_ record: String?) -> NSRect? {
        guard let record else { return nil }
        let components = record.split(whereSeparator: \.isWhitespace)
        let values = components.compactMap { Double($0) }
        guard components.count == 8, values.count == 8, values.allSatisfy(\.isFinite),
              values[2] > 0, values[3] > 0 else { return nil }
        return NSRect(x: values[0], y: values[1], width: values[2], height: values[3])
    }

    /// NSScreen.screens starts with the primary display, unlike NSScreen.main,
    /// which follows the key window. Never use pointer position for fallback.
    static func constrainedFrame(
        _ frame: NSRect,
        minimumSize: NSSize,
        visibleFrames: [NSRect]
    ) -> NSRect {
        guard var display = visibleFrames.first else { return frame }
        var largestOverlap: CGFloat = 0
        for candidate in visibleFrames {
            let intersection = frame.intersection(candidate)
            let area = intersection.isNull ? 0 : intersection.width * intersection.height
            if area > largestOverlap {
                largestOverlap = area
                display = candidate
            }
        }

        var result = frame
        result.size.width = max(minimumSize.width, min(frame.width, display.width))
        result.size.height = max(minimumSize.height, min(frame.height, display.height))
        result.origin.x = max(display.minX, min(frame.minX, display.maxX - result.width))
        // Pin the top edge when the window cannot fit, keeping its title bar reachable.
        result.origin.y = min(
            display.maxY - result.height,
            max(display.minY, frame.minY)
        )
        return result
    }
}
