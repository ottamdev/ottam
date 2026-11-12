import AppKit
import OttamPresentationLogic

/// What the font pickers can offer, read from the installed font collection.
@MainActor
enum FontCatalog {
    /// Every family a person could reasonably pick, for the UI font list.
    ///
    /// Families whose name starts with a dot are internal — `.AppleSystemUIFont`
    /// and friends — and are not selectable in any Mac font menu.
    ///
    /// Latin coverage is the second filter. Roughly a third of an untouched Mac's
    /// families are script-specific (Diwan Kufi, Devanagari MT, Kannada Sangam MN)
    /// or symbol-only (Zapf Dingbats, Apple Color Emoji); picking one as the UI
    /// font renders Ottam in fallback glyphs, so they are not offered.
    static var families: [String] {
        if let cachedFamilies { return cachedFamilies }
        let families = NSFontManager.shared.availableFontFamilies
            .filter { !$0.hasPrefix(".") && hasLatinCoverage($0) }
            .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
        cachedFamilies = families
        return families
    }

    /// The face a family should be previewed in — its own regular member, so the
    /// list shows each name written in the font it names.
    static func previewFont(forFamily family: String, size: CGFloat) -> NSFont? {
        NSFontManager.shared.font(
            withFamily: family,
            traits: [],
            weight: 5,
            size: size
        )
    }

    /// The subset that can hold code, for the code font list.
    ///
    /// Measured rather than read off the `.monoSpace` symbolic trait, which a
    /// good number of fixed-pitch families never set: if `i` and `W` advance by
    /// the same amount, the family is fixed-pitch.
    static var monospaceFamilies: [String] {
        if let cachedMonospaceFamilies { return cachedMonospaceFamilies }
        let families = families.filter(isMonospace)
        cachedMonospaceFamilies = families
        return families
    }

    private static var cachedFamilies: [String]?
    private static var cachedMonospaceFamilies: [String]?

    /// The probe characters: three Latin letters of obviously different natural
    /// width. Requiring all three to *exist* is what keeps symbol and script
    /// families out — they answer .notdef for every one, and .notdef advances
    /// uniformly, which reads as fixed-pitch. Measured on this Mac's 201
    /// families, that check is the difference between 75 matches and 27.
    private static let monospaceProbe = "iWM"

    private static func isMonospace(_ family: String) -> Bool {
        guard let font = probeFont(forFamily: family),
              let glyphs = glyphs(for: monospaceProbe, in: font)
        else { return false }

        let widths = glyphs.map { font.advancement(forCGGlyph: $0).width }
        guard let first = widths.first, first > 0 else { return false }
        return widths.allSatisfy { abs($0 - first) < 0.01 }
    }

    /// Enough of the Latin set to catch a family that cannot write English:
    /// both cases, digits, and the punctuation that shows up in code and paths.
    private static let latinProbe = "AQXaqxg0139{}<>@#/_"

    private static func hasLatinCoverage(_ family: String) -> Bool {
        guard let font = probeFont(forFamily: family) else { return false }
        return glyphs(for: latinProbe, in: font) != nil
    }

    private static func probeFont(forFamily family: String) -> NSFont? {
        guard let member = members(ofFamily: family)?.first else { return nil }
        return NSFont(name: member.postScriptName, size: 12)
    }

    /// The glyphs for `text`, or nil if the font is missing any of them.
    ///
    /// `CTFontGetGlyphsForCharacters` returns false the moment one character maps
    /// to `.notdef`, which is exactly the "this family cannot write this" signal
    /// both probes want — and the reason the monospace check cannot skip it, since
    /// `.notdef` advances uniformly and would otherwise read as fixed-pitch.
    private static func glyphs(for text: String, in font: NSFont) -> [CGGlyph]? {
        var characters = Array(text.utf16)
        var glyphs = [CGGlyph](repeating: 0, count: characters.count)
        guard CTFontGetGlyphsForCharacters(font, &characters, &glyphs, characters.count) else {
            return nil
        }
        return glyphs
    }

    /// The faces of one family, as `TypographyPresentation` wants them.
    ///
    /// `availableMembers` reports `[postScriptName, faceName, weight, traits]` on
    /// AppKit's 0…15 weight scale. Returns nil when the family is not installed —
    /// the caller treats that as a stored selection gone stale.
    static func members(ofFamily family: String) -> [FontFaceOption]? {
        guard let members = NSFontManager.shared.availableMembers(ofFontFamily: family) else {
            return nil
        }
        let options = members.compactMap { member -> FontFaceOption? in
            guard member.count >= 4,
                  let postScriptName = member[0] as? String,
                  let displayName = member[1] as? String,
                  let weight = member[2] as? Int,
                  let traits = member[3] as? UInt
            else { return nil }
            return FontFaceOption(
                postScriptName: postScriptName,
                displayName: displayName,
                weight: weight,
                isItalic: traits & UInt(NSFontTraitMask.italicFontMask.rawValue) != 0
            )
        }
        return options.isEmpty ? nil : options
    }
}
