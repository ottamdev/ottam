import AppKit
import Testing
@testable import OttamApp

@Suite("Window placement", .serialized)
@MainActor
struct WindowPlacementTests {
    private func window(_ frame: NSRect) -> NSWindow {
        _ = NSApplication.shared
        let window = NSWindow(
            contentRect: frame,
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.setFrame(frame, display: false)
        return window
    }

    @Test("workspace names are stable and independent")
    func names() {
        #expect(WindowPlacement.Role.workspace("a").autosaveName == "OttamWorkspace.a")
        #expect(WindowPlacement.Role.workspace("a").autosaveName != WindowPlacement.Role.workspace("b").autosaveName)
        #expect(WindowPlacement.Role.welcome.autosaveName != WindowPlacement.Role.workspace("a").autosaveName)
    }

    @Test("test markers disable persistence without requiring a preparation request")
    func exclusion() {
        #expect(WindowPlacement.isEnabled(environment: [:]))
        #expect(!WindowPlacement.isEnabled(environment: ["OTTAM_UI_TEST_MODE": "1"]))
    }

    @Test("disabled placement leaves the window and preferences untouched")
    func disabledAttachment() {
        let role = WindowPlacement.Role.workspace(UUID().uuidString)
        let name = role.autosaveName
        defer { NSWindow.removeFrame(usingName: name) }
        let frame = NSRect(x: 100, y: 100, width: 1000, height: 750)
        let target = window(frame)
        let record = "900 800 1000 750 0 0 2000 1400"
        UserDefaults.standard.set(record, forKey: "NSWindow Frame \(name)")
        WindowPlacement(enabled: false).attach(to: target, role: role)
        NotificationCenter.default.post(name: NSWindow.didResizeNotification, object: target)
        #expect(target.frame == frame)
        #expect(target.frameAutosaveName.isEmpty)
        #expect(UserDefaults.standard.string(forKey: "NSWindow Frame \(name)") == record)
    }

    @Test("a first attachment keeps defaults and subsequent attachment does not restore again")
    func firstAttachment() {
        let role = WindowPlacement.Role.workspace(UUID().uuidString)
        let name = role.autosaveName
        defer { NSWindow.removeFrame(usingName: name) }
        let frame = NSRect(x: 100, y: 100, width: 1000, height: 750)
        let target = window(frame)
        let placement = WindowPlacement(enabled: true)
        placement.attach(to: target, role: role)
        #expect(target.frame == frame)
        #expect(target.frameAutosaveName == name)
        #expect(WindowPlacement.savedFrame(UserDefaults.standard.string(forKey: "NSWindow Frame \(name)")) == frame)
        let moved = frame.offsetBy(dx: 30, dy: 40)
        target.setFrame(moved, display: false)
        // A stale record must not snap an already attached window back.
        UserDefaults.standard.set("100 100 1000 750 0 0 2000 1400", forKey: "NSWindow Frame \(name)")
        placement.attach(to: target, role: role)
        #expect(target.frame == moved)
        #expect(target.frameAutosaveName == name)
    }

    @Test("saving survives SwiftUI assigning its own autosave name")
    func sceneAutosaveName() {
        let role = WindowPlacement.Role.welcome
        let sceneName = "scene-test.\(UUID().uuidString)"
        let key = "NSWindow Frame \(role.autosaveName)"
        let previous = UserDefaults.standard.object(forKey: key)
        defer {
            NSWindow.removeFrame(usingName: role.autosaveName)
            NSWindow.removeFrame(usingName: sceneName)
            if let previous { UserDefaults.standard.set(previous, forKey: key) }
        }
        let target = window(NSRect(x: 100, y: 100, width: 1000, height: 750))
        let placement = WindowPlacement(enabled: true)
        placement.attach(to: target, role: role)
        target.setFrameAutosaveName(sceneName)
        let moved = target.frame.offsetBy(dx: 30, dy: 40)
        target.setFrame(moved, display: false)
        NotificationCenter.default.post(name: NSWindow.didMoveNotification, object: target)
        #expect(WindowPlacement.savedFrame(UserDefaults.standard.string(
            forKey: "NSWindow Frame \(role.autosaveName)"
        )) == moved)
        #expect(target.frameAutosaveName == sceneName)
    }

    @Test("native descriptor persistence bypasses scene-specific save redirection")
    func redirectedSaving() {
        let role = WindowPlacement.Role.workspace(UUID().uuidString)
        defer {
            NSWindow.removeFrame(usingName: role.autosaveName)
            NSWindow.removeFrame(usingName: "redirected.\(role.autosaveName)")
        }
        let target = RedirectingWindow(
            contentRect: NSRect(x: 100, y: 100, width: 1000, height: 750),
            styleMask: [.titled, .resizable], backing: .buffered, defer: false
        )
        let placement = WindowPlacement(enabled: true)
        placement.attach(to: target, role: role)
        #expect(WindowPlacement.savedFrame(UserDefaults.standard.string(
            forKey: "NSWindow Frame \(role.autosaveName)"
        )) == target.frame)
        let resized = NSRect(x: 120, y: 140, width: 1100, height: 850)
        target.setFrame(resized, display: false)
        NotificationCenter.default.post(name: NSWindow.didResizeNotification, object: target)
        #expect(WindowPlacement.savedFrame(UserDefaults.standard.string(
            forKey: "NSWindow Frame \(role.autosaveName)"
        )) == resized)
    }

    @Test("native records restore workspace frames across window instances")
    func restoreWorkspace() {
        let role = WindowPlacement.Role.workspace(UUID().uuidString)
        let name = role.autosaveName
        defer { NSWindow.removeFrame(usingName: name) }
        let source = window(NSRect(x: 100, y: 100, width: 1000, height: 750))
        source.saveFrame(usingName: name)
        let target = window(NSRect(x: 400, y: 300, width: 1200, height: 900))
        WindowPlacement(enabled: true).attach(to: target, role: role)
        #expect(target.frameAutosaveName == name)
        let minimum = target.frameRect(forContentRect: NSRect(
            origin: .zero, size: WindowPlacement.minimumWorkspaceContentSize
        )).size
        #expect(target.frame == WindowPlacement.constrainedFrame(
            source.frame, minimumSize: minimum, visibleFrames: NSScreen.screens.map(\.visibleFrame)
        ))
    }

    @Test("welcome restoration preserves its current content size")
    func welcomeSize() {
        let name = WindowPlacement.Role.welcome.autosaveName
        // Preserve any real preference in the test process's domain.
        let key = "NSWindow Frame \(name)"
        let previous = UserDefaults.standard.object(forKey: key)
        defer {
            NSWindow.removeFrame(usingName: name)
            if let previous { UserDefaults.standard.set(previous, forKey: key) }
        }
        let source = window(NSRect(x: 140, y: 180, width: 730, height: 470))
        source.saveFrame(usingName: name)
        let target = window(NSRect(x: 400, y: 300, width: 520, height: 470))
        WindowPlacement(enabled: true).attach(to: target, role: .welcome)
        #expect(target.frameAutosaveName.isEmpty)
        #expect(target.frame.size == NSSize(width: 520, height: 470))
        #expect(target.frame == WindowPlacement.constrainedFrame(
            NSRect(origin: source.frame.origin, size: target.frame.size),
            minimumSize: target.frame.size, visibleFrames: NSScreen.screens.map(\.visibleFrame)
        ))
    }

    @Test("malformed records are ignored", arguments: [
        "", "garbage", "1 2 3 4", "1 2 0 4 0 0 100 100",
        "1 2 3 -4 0 0 100 100", "nan 2 3 4 0 0 100 100",
        "1 2 3 4 0 0 100 100 junk"
    ])
    func invalidRecords(_ record: String) {
        #expect(WindowPlacement.savedFrame(record) == nil)
        #expect(WindowPlacement.savedFrame(nil) == nil)
    }

    @Test("frame fits the display with the greatest overlap, including negative coordinates")
    func overlappingDisplays() {
        let displays = [NSRect(x: 0, y: 0, width: 1440, height: 900),
                        NSRect(x: -1920, y: 0, width: 1920, height: 1080)]
        let frame = NSRect(x: -900, y: 100, width: 1000, height: 700)
        #expect(WindowPlacement.constrainedFrame(frame, minimumSize: .zero, visibleFrames: displays)
                == NSRect(x: -1000, y: 100, width: 1000, height: 700))
    }

    @Test("a disconnected display falls back to primary and shrinks oversized windows")
    func disconnectedDisplay() {
        #expect(WindowPlacement.constrainedFrame(
            NSRect(x: 3000, y: 200, width: 1800, height: 1200),
            minimumSize: NSSize(width: 920, height: 650),
            visibleFrames: [NSRect(x: 0, y: 30, width: 1440, height: 850),
                            NSRect(x: -1920, y: 0, width: 1920, height: 1080)]
        ) == NSRect(x: 0, y: 30, width: 1440, height: 850))
    }

    @Test("tiny displays preserve minimum size and expose the title bar")
    func tinyDisplay() {
        let display = NSRect(x: 0, y: 30, width: 800, height: 570)
        let result = WindowPlacement.constrainedFrame(
            NSRect(x: 100, y: 100, width: 1200, height: 900),
            minimumSize: NSSize(width: 920, height: 650), visibleFrames: [display]
        )
        #expect(result.size == NSSize(width: 920, height: 650))
        #expect(result.minX == display.minX)
        #expect(result.maxY == display.maxY)
    }

    @Test("no screens leaves the frame unchanged")
    func noDisplays() {
        let frame = NSRect(x: 1, y: 2, width: 1000, height: 750)
        #expect(WindowPlacement.constrainedFrame(frame, minimumSize: .zero, visibleFrames: []) == frame)
    }
}

@MainActor
private final class RedirectingWindow: NSWindow {
    override func saveFrame(usingName name: String) {
        super.saveFrame(usingName: "redirected.\(name)")
    }
}
