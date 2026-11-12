import AppKit
import OttamPresentationLogic
import OSLog
import SwiftUI

private let typographySettingsLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Typography"
)

/// The user's two font choices — one for content text, one for code — and the
/// `AppTypography.Resolution` they resolve to.
///
/// Shaped like `AppearanceSettings`: UserDefaults-backed, `UserDefaults`
/// injectable. It differs in publishing an observable value rather than only
/// firing a notification, because the resolution is what the environment
/// carries. The notification stays for the consumers that cannot observe —
/// AppKit text views and the Ghostty runtime.
@MainActor
@Observable
final class TypographySettings {
    static let shared = TypographySettings()

    static let changedNotification = Notification.Name("dev.ottam.Ottam.typographyChanged")

    private enum Key {
        static let uiFamily = "dev.ottam.Ottam.typography.uiFamily"
        static let uiFace = "dev.ottam.Ottam.typography.uiFace"
        static let uiSizeDelta = "dev.ottam.Ottam.typography.uiSizeDelta"
        static let codeFamily = "dev.ottam.Ottam.typography.codeFamily"
        static let codeFace = "dev.ottam.Ottam.typography.codeFace"
        static let codeSizeDelta = "dev.ottam.Ottam.typography.codeSizeDelta"
    }

    /// The size control's range, in points off each scale's own size. Wider than
    /// this and the named scale stops meaning anything.
    static let sizeDeltaRange: ClosedRange<CGFloat> = -2...4

    /// `nil` is the system font, which is a deliberate choice rather than an
    /// absent one — hence the sentinel below rather than a missing default.
    private static let systemFamilySentinel = "__system__"

    @ObservationIgnored private let defaults: UserDefaults

    private(set) var resolved: AppTypography.Resolution = .builtIn

    // A face belongs to a family, so changing the family clears the face:
    // carrying "Bold Italic" across to a family that never had it would resolve
    // to nothing.
    var uiFamily: String? {
        didSet {
            guard uiFamily != oldValue else { return }
            writeFamily(uiFamily, forKey: Key.uiFamily)
            uiFacePostScriptName = nil
            recompute()
        }
    }

    var uiFacePostScriptName: String? {
        didSet {
            guard uiFacePostScriptName != oldValue else { return }
            write(uiFacePostScriptName, forKey: Key.uiFace)
            recompute()
        }
    }

    var uiSizeDelta: CGFloat {
        didSet {
            guard uiSizeDelta != oldValue else { return }
            defaults.set(Double(uiSizeDelta), forKey: Key.uiSizeDelta)
            recompute()
        }
    }

    var codeFamily: String? {
        didSet {
            guard codeFamily != oldValue else { return }
            writeFamily(codeFamily, forKey: Key.codeFamily)
            codeFacePostScriptName = nil
            recompute()
        }
    }

    var codeFacePostScriptName: String? {
        didSet {
            guard codeFacePostScriptName != oldValue else { return }
            write(codeFacePostScriptName, forKey: Key.codeFace)
            recompute()
        }
    }

    var codeSizeDelta: CGFloat {
        didSet {
            guard codeSizeDelta != oldValue else { return }
            defaults.set(Double(codeSizeDelta), forKey: Key.codeSizeDelta)
            recompute()
        }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        // Text defaults to the system font — chat prose should read like the
        // rest of macOS — while code keeps the family the app ships.
        uiFamily = Self.storedFamily(defaults, Key.uiFamily, unset: nil)
        uiFacePostScriptName = defaults.string(forKey: Key.uiFace)
        uiSizeDelta = Self.storedDelta(defaults, Key.uiSizeDelta)
        codeFamily = Self.storedFamily(
            defaults,
            Key.codeFamily,
            unset: AppTypography.builtInFamilyName
        )
        codeFacePostScriptName = defaults.string(forKey: Key.codeFace)
        codeSizeDelta = Self.storedDelta(defaults, Key.codeSizeDelta)
        recompute()
    }

    /// The faces the given family offers as a base, for the face picker.
    func faceOptions(forFamily family: String?) -> [FontFaceOption] {
        guard let family, let members = FontCatalog.members(ofFamily: family) else { return [] }
        return TypographyPresentation.baseFaceOptions(members: members)
    }

    // MARK: - Resolution

    private func recompute() {
        let next = AppTypography.Resolution(
            ui: faceNames(family: uiFamily, preferredFace: uiFacePostScriptName, role: "ui"),
            code: faceNames(family: codeFamily, preferredFace: codeFacePostScriptName, role: "code"),
            uiSizeDelta: uiSizeDelta,
            codeSizeDelta: codeSizeDelta
        )
        guard next != resolved else { return }
        resolved = next
        NotificationCenter.default.post(name: Self.changedNotification, object: next)
    }

    /// Resolves a stored selection to four PostScript names, degrading rather
    /// than failing: an uninstalled family falls back to the shipped one, and a
    /// missing shipped font falls back to the system font. Either way text stays
    /// legible instead of disappearing.
    private func faceNames(
        family: String?,
        preferredFace: String?,
        role: String
    ) -> AppTypography.FaceNames {
        guard let family else { return .system }

        guard let members = FontCatalog.members(ofFamily: family),
              let faces = TypographyPresentation.resolveFaces(
                  members: members,
                  preferredPostScriptName: preferredFace
              )
        else {
            typographySettingsLogger.error(
                "\(role, privacy: .public) font family \(family, privacy: .public) is unavailable; falling back"
            )
            return AppTypography.isBuiltInFamilyAvailable ? .builtIn : .system
        }

        return AppTypography.FaceNames(
            regular: faces.regular.postScriptName,
            bold: faces.bold.postScriptName,
            italic: faces.italic?.postScriptName ?? faces.regular.postScriptName,
            boldItalic: faces.boldItalic?.postScriptName ?? faces.bold.postScriptName
        )
    }

    // MARK: - Persistence

    /// `nil` here means the system font — a real choice — so it is stored as the
    /// sentinel rather than as an absent key, which means "never chosen".
    private func writeFamily(_ family: String?, forKey key: String) {
        defaults.set(family ?? Self.systemFamilySentinel, forKey: key)
    }

    private func write(_ value: String?, forKey key: String) {
        if let value {
            defaults.set(value, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }

    /// `unset` is what the role falls back to when the key was never written —
    /// which is not the same as the sentinel, where the user chose the system
    /// font outright. The two roles ship with different answers.
    private static func storedFamily(
        _ defaults: UserDefaults,
        _ key: String,
        unset fallback: String?
    ) -> String? {
        guard let stored = defaults.string(forKey: key) else { return fallback }
        return stored == systemFamilySentinel ? nil : stored
    }

    private static func storedDelta(_ defaults: UserDefaults, _ key: String) -> CGFloat {
        guard defaults.object(forKey: key) != nil else { return 0 }
        let stored = CGFloat(defaults.double(forKey: key))
        return min(max(stored, sizeDeltaRange.lowerBound), sizeDeltaRange.upperBound)
    }
}
