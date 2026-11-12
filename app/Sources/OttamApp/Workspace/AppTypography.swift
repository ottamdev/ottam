import AppKit
import CoreText
import OSLog
import SwiftUI

private let typographyLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Typography"
)

/// The app's typographic vocabulary.
///
/// Every piece of Ottam-drawn content text resolves through here, so the
/// chosen family is one value rather than a hundred call sites. Two properties of
/// the design drive the shape:
///
/// 1. **Two weights, not nine.** Views ask for `.regular` or `.bold` and nothing
///    between. Hack ships only those, and CoreText does not synthesize the gaps —
///    it snaps to the nearest real face, sending `.medium` *up* to Bold, louder
///    than the system font it replaced. So weight is not a hierarchy channel
///    here: size and color are. That holds for a user-chosen family too, where
///    the bold is derived from the base face by `TypographyPresentation`.
/// 2. **Faces are addressed by PostScript name.** Asking for a family plus a
///    weight trait leaves the resolution to CoreText; naming the face outright
///    means the face that renders is the face that was requested.
///
/// SF Symbols are unaffected — `Image(systemName:)` takes its stroke weight from
/// the symbol's own axis, so icon sites keep `.system(size:weight:)` and keep the
/// full weight range.
///
/// Chrome — the sidebar, inspector, toolbar, Settings, the chooser, the window
/// title — is not part of this vocabulary and stays in the system font. See
/// `appTypographyRoot()`.
enum AppTypography {
    /// The family the app ships, and what the code role defaults to. Text
    /// defaults to the system font instead — see `TypographySettings`.
    static let builtInFamilyName = "Hack Nerd Font Mono"

    /// A role, not a name. Which PostScript face renders it comes from
    /// `Resolution`, and therefore from the user's choice.
    enum Face: Int, CaseIterable {
        case regular
        case bold
        case italic
        case boldItalic

        var isBold: Bool { self == .bold || self == .boldItalic }
        var isItalic: Bool { self == .italic || self == .boldItalic }
    }

    /// Which of the two selectable fonts a scale follows.
    enum Role {
        case ui
        case code
    }

    /// Named sizes, replacing both the semantic styles and the ~40 hand-dialed
    /// point sizes that preceded them.
    ///
    /// The shape follows the macOS ramp: whole-point steps from 10 to 16, a
    /// loose ~1.2 ratio above that. `body` is the system control size, so
    /// anything that sits beside an AppKit control lines up with it; `prose`
    /// is the one deliberate step above it, for sustained reading. Mapping
    /// rules, so the same kind of element lands on the same rung everywhere:
    ///
    /// - `prose`: message bodies and other reading text.
    /// - `body`: primary row text and card titles — sidebar, terminal, file
    ///   rail, activity and permission rows.
    /// - `callout`: the secondary line under a `body` row, card copy,
    ///   arguments, notices. Always one step under its primary.
    /// - `label`: badges, column headers, status pills.
    /// - `caption`: timestamps and counts. The floor — HIG's macOS minimum.
    ///
    /// Chrome keeps the system family but takes its sizes from here too, via
    /// `Font.chrome(_:weight:design:)`, so a retune moves everything at once.
    enum Scale: Int, CaseIterable {
        // Text.
        /// Hero numerals and welcome titles.
        case displayLarge
        /// Empty-state glyphs and large headings.
        case display
        /// Sheet titles and markdown h1.
        case titleLarge
        /// Markdown h2 and the identity-bar workspace name.
        case headline
        /// Empty-state titles, markdown h3.
        case title
        /// Markdown prose and h4. One step over `body`: reading text sits
        /// larger than list rows, and the system font at 13 reads a size
        /// smaller than Hack at 13 did.
        case prose
        /// Default control size — list rows, card titles, composer.
        case body
        /// Secondary rows and supporting copy, one step under `body`.
        case callout
        /// Badges, column headers, status pills.
        case label
        /// Timestamps, counts, footnotes. The floor: HIG's macOS minimum is
        /// 10pt, and 9 is reserved for mini controls.
        case caption

        // Code. These follow the code font rather than the UI font.
        /// Markdown code blocks and tables.
        case code
        /// Diff rows.
        case codeDiff
        /// Diff line-number gutter.
        case codeGutter
        /// Inline identifiers, hashes, paths, change counts.
        case codeMeta
        /// The embedded terminal. Larger than `code` so it keeps the size it has
        /// always had while still following the code size control.
        case terminal

        var points: CGFloat {
            switch self {
            case .displayLarge: 38
            case .display: 28
            case .titleLarge: 19
            case .headline: 16
            case .title: 15
            case .prose: 14
            case .body: 13
            case .callout: 12
            case .label: 11
            case .caption: 10
            case .code: 12
            case .codeDiff: 11.5
            case .codeGutter: 10
            case .codeMeta: 10
            case .terminal: 13
            }
        }

        var role: Role {
            switch self {
            case .code, .codeDiff, .codeGutter, .codeMeta, .terminal: .code
            default: .ui
            }
        }
    }

    /// The four PostScript names one role renders with. `nil` means the system
    /// font, which has no fixed name — its weight and slant come from the `Face`.
    struct FaceNames: Equatable, Hashable {
        var regular: String?
        var bold: String?
        var italic: String?
        var boldItalic: String?

        func name(for face: Face) -> String? {
            switch face {
            case .regular: regular
            case .bold: bold
            case .italic: italic
            case .boldItalic: boldItalic
            }
        }

        var signature: String {
            Face.allCases.map { name(for: $0) ?? "-" }.joined(separator: ",")
        }

        static let builtIn = FaceNames(
            regular: BundledFace.regular.postScriptName,
            bold: BundledFace.bold.postScriptName,
            italic: BundledFace.italic.postScriptName,
            boldItalic: BundledFace.boldItalic.postScriptName
        )

        static let system = FaceNames()
    }

    /// A resolved pair of fonts — one for text, one for code — plus their sizes.
    ///
    /// Carried in the environment rather than read from a global, so changing a
    /// font invalidates exactly the views that draw with it. Cheap to hand around
    /// and cheap to compare: `==` looks only at the selection, never at the
    /// precomputed table, which is derived from it.
    struct Resolution: Equatable, Hashable {
        let ui: FaceNames
        let code: FaceNames
        let uiSizeDelta: CGFloat
        let codeSizeDelta: CGFloat

        /// Every scale × face, built once per selection change. Views ask for
        /// fonts on every body, so this is a lookup rather than a construction.
        private let table: [Font]

        init(
            ui: FaceNames = .builtIn,
            code: FaceNames = .builtIn,
            uiSizeDelta: CGFloat = 0,
            codeSizeDelta: CGFloat = 0
        ) {
            self.ui = ui
            self.code = code
            self.uiSizeDelta = uiSizeDelta
            self.codeSizeDelta = codeSizeDelta

            var table: [Font] = []
            table.reserveCapacity(Scale.allCases.count * Face.allCases.count)
            for scale in Scale.allCases {
                let names = scale.role == .code ? code : ui
                let points = scale.points + (scale.role == .code ? codeSizeDelta : uiSizeDelta)
                for face in Face.allCases {
                    table.append(
                        AppTypography.makeFont(
                            name: names.name(for: face),
                            points: points,
                            face: face,
                            role: scale.role
                        )
                    )
                }
            }
            self.table = table
        }

        /// The bundled family in both roles, at each scale's own size. A fixed
        /// reference for previews and tests rather than the app's default, which
        /// takes the system font for text — see `TypographySettings`.
        static let builtIn = Resolution()

        func font(_ scale: Scale, _ face: Face = .regular) -> Font {
            table[scale.rawValue * Face.allCases.count + face.rawValue]
        }

        /// A stable spelling of this selection, for caches that store text with the
        /// font already baked in and so must miss rather than serve a stale face.
        var signature: String {
            "\(ui.signature)|\(code.signature)|\(uiSizeDelta)|\(codeSizeDelta)"
        }

        func nsFont(_ scale: Scale, _ face: Face = .regular) -> NSFont {
            let names = scale.role == .code ? code : ui
            return AppTypography.makeNSFont(
                name: names.name(for: face),
                points: points(for: scale),
                face: face,
                role: scale.role
            )
        }

        /// The scale's size with this selection's delta applied.
        func points(for scale: Scale) -> CGFloat {
            scale.points + (scale.role == .code ? codeSizeDelta : uiSizeDelta)
        }

        /// Monospace reads optically larger than a proportional face at the
        /// same point size, so a code span is not a fixed rung but a ratio of
        /// the text around it: 13 inside `prose`, 12 inside `body`. Sized from
        /// the surrounding scale — and so from the UI delta — because it lives
        /// on that line; only the face comes from the code selection.
        static let inlineCodeRatio: CGFloat = 0.93

        func inlineCodeFont(within surrounding: Scale, _ face: Face = .regular) -> NSFont {
            AppTypography.makeNSFont(
                name: code.name(for: face),
                points: (points(for: surrounding) * Self.inlineCodeRatio).rounded(),
                face: face,
                role: .code
            )
        }

        // The table is derived from the four stored values, so comparing it would
        // only cost time. Views compare resolutions on every environment write.
        static func == (lhs: Resolution, rhs: Resolution) -> Bool {
            lhs.ui == rhs.ui
                && lhs.code == rhs.code
                && lhs.uiSizeDelta == rhs.uiSizeDelta
                && lhs.codeSizeDelta == rhs.codeSizeDelta
        }

        func hash(into hasher: inout Hasher) {
            hasher.combine(ui)
            hasher.combine(code)
            hasher.combine(uiSizeDelta)
            hasher.combine(codeSizeDelta)
        }
    }

    private static func makeFont(
        name: String?,
        points: CGFloat,
        face: Face,
        role: Role
    ) -> Font {
        guard let name else {
            // No PostScript name to aim at, so the face has to be spelled as
            // traits instead. "System font" in the code role means the system's
            // *monospaced* face — proportional code would be a bug, not a choice.
            let font = Font.system(
                size: points,
                weight: face.isBold ? .bold : .regular,
                design: role == .code ? .monospaced : .default
            )
            return face.isItalic ? font.italic() : font
        }
        return .custom(name, fixedSize: points)
    }

    private static func makeNSFont(
        name: String?,
        points: CGFloat,
        face: Face,
        role: Role
    ) -> NSFont {
        // The system font is a real fallback, not a formality: a missing or
        // uninstalled face should degrade to legible text rather than blank rows.
        let weight: NSFont.Weight = face.isBold ? .bold : .regular
        let systemFont: NSFont = role == .code
            ? .monospacedSystemFont(ofSize: points, weight: weight)
            : .systemFont(ofSize: points, weight: weight)
        let base = name.flatMap { NSFont(name: $0, size: points) } ?? systemFont
        guard name == nil, face.isItalic else { return base }
        return NSFontManager.shared.convert(base, toHaveTrait: .italicFontMask)
    }

    // MARK: - Bundled faces

    /// The four faces Hack ships, which are also what gets registered at launch.
    enum BundledFace: CaseIterable {
        case regular
        case bold
        case italic
        case boldItalic

        var postScriptName: String {
            switch self {
            case .regular: "HackNFM-Regular"
            case .bold: "HackNFM-Bold"
            case .italic: "HackNFM-Italic"
            case .boldItalic: "HackNFM-BoldItalic"
            }
        }

        /// Filename of the vendored TTF backing this face.
        var resourceName: String {
            switch self {
            case .regular: "HackNerdFontMono-Regular"
            case .bold: "HackNerdFontMono-Bold"
            case .italic: "HackNerdFontMono-Italic"
            case .boldItalic: "HackNerdFontMono-BoldItalic"
            }
        }
    }

    private static var didRegister = false

    /// Makes the bundled faces available to CoreText.
    ///
    /// In an assembled bundle `ATSApplicationFontsPath` has already done this by
    /// the time any code runs. The dev path (`swift run`, the UI-test harness)
    /// has no Info.plist doing that work, so the same TTFs are registered from
    /// the SPM resource bundle instead. Call once, before the first scene body.
    @MainActor
    static func registerBundledFonts() {
        guard !didRegister else { return }
        didRegister = true

        // Already resolvable — bundle path took care of it, or the developer has
        // Hack installed. Registering again would only log a duplicate.
        if isBuiltInFamilyAvailable {
            return
        }

        // Registered one URL at a time: the plural form is deprecated, and its
        // replacement may report results asynchronously — no good when the first
        // window is about to draw. This overload stays synchronous.
        for face in BundledFace.allCases {
            guard let url = resourceURL(for: face) else {
                typographyLogger.error(
                    "Missing bundled face \(face.resourceName, privacy: .public)"
                )
                continue
            }
            var error: Unmanaged<CFError>?
            if !CTFontManagerRegisterFontsForURL(url as CFURL, .process, &error) {
                let description = error?.takeRetainedValue().localizedDescription ?? "unknown"
                typographyLogger.error(
                    "Font registration failed face=\(face.resourceName, privacy: .public) error=\(description, privacy: .public)"
                )
            }
        }

        if !isBuiltInFamilyAvailable {
            typographyLogger.error(
                "\(builtInFamilyName, privacy: .public) unavailable after registration; content falls back to the system font."
            )
        }
    }

    static var isBuiltInFamilyAvailable: Bool {
        NSFont(name: BundledFace.regular.postScriptName, size: 12) != nil
    }

    private static func resourceURL(for face: BundledFace) -> URL? {
        // The two bundle layouts differ: `build-app.sh` flattens AppResources
        // into `Contents/Resources/`, so the assembled app has `Fonts/`, while
        // SPM's `.process` rule keeps the source tree and yields
        // `AppResources/Fonts/`. Try both, plus a flat fallback.
        let subdirectories = ["Fonts", "AppResources/Fonts", nil]
        for bundle in [Bundle.main, Bundle.module] {
            for subdirectory in subdirectories {
                if let url = bundle.url(
                    forResource: face.resourceName,
                    withExtension: "ttf",
                    subdirectory: subdirectory
                ) {
                    return url
                }
            }
        }
        return nil
    }
}

// MARK: - Environment

/// Spelled out rather than via `@Entry`: the macro's plugin is not available to
/// this SwiftPM build.
private struct AppTypographyKey: EnvironmentKey {
    static let defaultValue = AppTypography.Resolution.builtIn
}

extension EnvironmentValues {
    /// The fonts a content surface draws with. Defaults to the bundled pair, so
    /// anything rendered outside a surface — previews, `ImageRenderer` — still
    /// gets a real font rather than nothing. Every surface in the app is under
    /// `appTypographyRoot(typography.resolved)` and so follows the user's choice.
    var appTypography: AppTypography.Resolution {
        get { self[AppTypographyKey.self] }
        set { self[AppTypographyKey.self] = newValue }
    }
}

private struct AppFontModifier: ViewModifier {
    @Environment(\.appTypography) private var typography
    let scale: AppTypography.Scale
    let face: AppTypography.Face

    func body(content: Content) -> some View {
        content.font(typography.font(scale, face))
    }
}

extension View {
    /// `.appFont(.body)`, `.appFont(.title, .bold)`.
    ///
    /// Reads the resolution from the environment rather than a global, so a font
    /// change invalidates exactly the views that draw with it — no change of view
    /// identity, which is what would tear down terminal surfaces and scroll
    /// positions.
    func appFont(
        _ scale: AppTypography.Scale,
        _ face: AppTypography.Face = .regular
    ) -> some View {
        modifier(AppFontModifier(scale: scale, face: face))
    }

    /// Marks a subtree as a content surface: agent output, code, diffs, the
    /// composer.
    ///
    /// Font follows role, not location. Text the agent produced, text from the
    /// repo and text the user types render in the chosen fonts; navigation,
    /// controls and the app's own labels stay in the system font. That line lands
    /// at the detail column — the sidebar, inspector, toolbar, Settings, the
    /// chooser and the window title are chrome and never see this modifier.
    ///
    /// Sets the inherited default as well as publishing the resolution, because
    /// migrating explicit `.font(…)` call sites only covers text that asked for a
    /// font; everything that took the default would otherwise still resolve to
    /// the system font and leave the surface half-converted.
    func appTypographyRoot(_ resolution: AppTypography.Resolution = .builtIn) -> some View {
        environment(\.appTypography, resolution)
            .font(resolution.font(.body))
    }

    /// Returns a subtree to the system font vocabulary. Used by sheets presented
    /// from the detail column, which would otherwise inherit the content font.
    ///
    /// For chrome that hangs off a content surface — a sheet presented from the
    /// detail column — and would otherwise inherit the content font through the
    /// environment. `nil` restores the default rather than pinning one style, so
    /// controls keep resolving their own fonts.
    func systemTypographyRoot() -> some View {
        font(nil)
    }
}

extension Font {
    /// The system font at a scale's size, for chrome.
    ///
    /// Chrome — sidebar, toolbar, Settings, the chooser — stays in the system
    /// family and ignores the user's font choice, but its sizes come from the
    /// same ladder as content so the two never drift apart. The full weight
    /// range is available here: the two-weight doctrine is about Hack and
    /// user-chosen families, not about SF.
    static func chrome(
        _ scale: AppTypography.Scale,
        weight: Font.Weight = .regular,
        design: Font.Design = .default
    ) -> Font {
        .system(size: scale.points, weight: weight, design: design)
    }
}
