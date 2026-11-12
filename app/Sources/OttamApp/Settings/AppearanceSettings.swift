import AppKit

enum AppearanceMode: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .system:
            return "System"
        case .light:
            return "Light"
        case .dark:
            return "Dark"
        }
    }
}

enum AppearanceSettings {
    static let appearanceModeKey = "dev.ottam.Ottam.appearanceMode"
    static let changedNotification = Notification.Name("dev.ottam.Ottam.appearanceModeChanged")

    static func current(defaults: UserDefaults = .standard) -> AppearanceMode {
        guard
            let rawValue = defaults.string(forKey: appearanceModeKey),
            let mode = AppearanceMode(rawValue: rawValue)
        else {
            return .system
        }
        return mode
    }

    @discardableResult
    static func select(_ mode: AppearanceMode, defaults: UserDefaults = .standard) -> AppearanceMode {
        defaults.set(mode.rawValue, forKey: appearanceModeKey)
        apply(mode)
        NotificationCenter.default.post(name: changedNotification, object: mode)
        return mode
    }

    @discardableResult
    static func applyStored(defaults: UserDefaults = .standard) -> AppearanceMode {
        let mode = current(defaults: defaults)
        apply(mode)
        return mode
    }

    /// `NSApplication.shared`, not `NSApp`: this now runs from the App's
    /// initializer, which can precede the first access that populates `NSApp`.
    private static func apply(_ mode: AppearanceMode) {
        let app = NSApplication.shared
        switch mode {
        case .system:
            app.appearance = nil
        case .light:
            app.appearance = NSAppearance(named: .aqua)
        case .dark:
            app.appearance = NSAppearance(named: .darkAqua)
        }
    }
}
