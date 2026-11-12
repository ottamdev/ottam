import OttamGhostty
import Foundation

/// Carries the code font into the embedded terminal.
///
/// The terminal is the one code surface Ottam does not draw itself, so it
/// cannot read the environment. Ghostty matches by *family* name, not by
/// PostScript name, which is why this reads the stored family rather than the
/// resolved faces.
@MainActor
enum TerminalFontBridge {
    private static var observer: NSObjectProtocol?

    static func start() {
        guard observer == nil else { return }
        let settings = TypographySettings.shared
        apply(settings)
        observer = NotificationCenter.default.addObserver(
            forName: TypographySettings.changedNotification,
            object: nil,
            queue: .main
        ) { _ in
            MainActor.assumeIsolated { apply(settings) }
        }
    }

    private static func apply(_ settings: TypographySettings) {
        GhosttyTerminalConfigurationReload.setFont(
            family: settings.codeFamily,
            points: Double(settings.resolved.points(for: .terminal))
        )
    }
}
