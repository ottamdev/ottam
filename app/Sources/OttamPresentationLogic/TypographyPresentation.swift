import Foundation

/// One installed face, reduced to what font selection needs.
///
/// Plain data on purpose: the rules below decide which face pairs with which,
/// and they are worth testing without depending on what fonts a machine happens
/// to have installed.
public struct FontFaceOption: Equatable, Sendable, Identifiable, Hashable {
    /// The name a face is addressed by. Asking for a family plus a weight trait
    /// leaves resolution to CoreText, which snaps to whatever it considers
    /// nearest; naming the face outright means the face that renders is the face
    /// that was chosen.
    public let postScriptName: String
    /// The family-relative name, as shown in the picker — "Regular", "Bold Italic".
    public let displayName: String
    /// AppKit's 0…15 scale, where 5 is regular and 9 is bold.
    public let weight: Int
    public let isItalic: Bool

    public var id: String { postScriptName }

    public init(postScriptName: String, displayName: String, weight: Int, isItalic: Bool) {
        self.postScriptName = postScriptName
        self.displayName = displayName
        self.weight = weight
        self.isItalic = isItalic
    }
}

/// The four faces the app's two-weight vocabulary needs, resolved from one family.
public struct ResolvedFontFaces: Equatable, Sendable {
    public let regular: FontFaceOption
    public let bold: FontFaceOption
    public let italic: FontFaceOption?
    public let boldItalic: FontFaceOption?

    public init(
        regular: FontFaceOption,
        bold: FontFaceOption,
        italic: FontFaceOption?,
        boldItalic: FontFaceOption?
    ) {
        self.regular = regular
        self.bold = bold
        self.italic = italic
        self.boldItalic = boldItalic
    }
}

public enum TypographyPresentation {
    /// AppKit's regular weight. Verified against `availableMembers(ofFontFamily:)`:
    /// Hack, Menlo and Helvetica Neue all report 5 for Regular and 9 for Bold.
    public static let regularWeight = 5

    /// The upright faces a family offers as a base, lightest first.
    ///
    /// Only upright faces: the user picks a base and the slanted counterpart is
    /// derived, the same way bold is. A family with nothing but slanted faces
    /// falls back to offering those, so it is still selectable.
    public static func baseFaceOptions(members: [FontFaceOption]) -> [FontFaceOption] {
        let upright = members.filter { !$0.isItalic }
        let candidates = upright.isEmpty ? members : upright
        return candidates.sorted { lhs, rhs in
            lhs.weight == rhs.weight
                ? lhs.displayName < rhs.displayName
                : lhs.weight < rhs.weight
        }
    }

    /// Pairs a chosen base face with the bold, italic and bold-italic that go
    /// with it. Returns nil only for an empty family.
    ///
    /// The app asks for regular or bold and nothing between, so the bold here is
    /// derived rather than chosen. It aims one step past base — `weight + 4`, the
    /// distance from regular to bold — and takes the nearest heavier face to that
    /// target, breaking ties lighter. That keeps Regular paired with Bold in a
    /// family that has both, and pairs Light with Medium rather than Black in a
    /// family with nine weights, where taking "the heaviest" would blow the
    /// contrast out.
    public static func resolveFaces(
        members: [FontFaceOption],
        preferredPostScriptName: String?
    ) -> ResolvedFontFaces? {
        guard !members.isEmpty else { return nil }

        let base = self.base(members: members, preferredPostScriptName: preferredPostScriptName)
        let bold = self.bold(members: members, base: base)

        return ResolvedFontFaces(
            regular: base,
            bold: bold,
            italic: counterpart(of: base, in: members),
            boldItalic: counterpart(of: bold, in: members)
        )
    }

    private static func base(
        members: [FontFaceOption],
        preferredPostScriptName: String?
    ) -> FontFaceOption {
        // A stored face that no longer exists — the font was replaced by a
        // version with different faces — falls through to the default rather
        // than failing the whole family.
        if let preferredPostScriptName,
           let match = members.first(where: { $0.postScriptName == preferredPostScriptName }) {
            return match
        }

        let candidates = baseFaceOptions(members: members)
        // Nearest to regular, preferring the lighter of two equidistant faces.
        return candidates.min { lhs, rhs in
            let lhsDistance = abs(lhs.weight - regularWeight)
            let rhsDistance = abs(rhs.weight - regularWeight)
            return lhsDistance == rhsDistance ? lhs.weight < rhs.weight : lhsDistance < rhsDistance
        } ?? members[0]
    }

    private static func bold(members: [FontFaceOption], base: FontFaceOption) -> FontFaceOption {
        let target = base.weight + 4
        let heavier = members.filter { $0.isItalic == base.isItalic && $0.weight > base.weight }
        // Nothing heavier to promote to: reuse the base rather than let CoreText
        // synthesize a smeared bold.
        guard !heavier.isEmpty else { return base }

        return heavier.min { lhs, rhs in
            let lhsDistance = abs(lhs.weight - target)
            let rhsDistance = abs(rhs.weight - target)
            return lhsDistance == rhsDistance ? lhs.weight < rhs.weight : lhsDistance < rhsDistance
        } ?? base
    }

    /// The slanted twin of a face, or nil when the family has none.
    private static func counterpart(
        of face: FontFaceOption,
        in members: [FontFaceOption]
    ) -> FontFaceOption? {
        members
            .filter { $0.isItalic != face.isItalic }
            .min { abs($0.weight - face.weight) < abs($1.weight - face.weight) }
    }
}
