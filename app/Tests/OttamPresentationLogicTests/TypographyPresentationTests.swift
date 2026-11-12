import Testing

@testable import OttamPresentationLogic

private func face(
    _ postScriptName: String,
    _ displayName: String,
    weight: Int,
    italic: Bool = false
) -> FontFaceOption {
    FontFaceOption(
        postScriptName: postScriptName,
        displayName: displayName,
        weight: weight,
        isItalic: italic
    )
}

/// Real weights, read off `NSFontManager.availableMembers(ofFontFamily:)`.
private let hack = [
    face("HackNFM-Regular", "Regular", weight: 5),
    face("HackNFM-Italic", "Italic", weight: 5, italic: true),
    face("HackNFM-Bold", "Bold", weight: 9),
    face("HackNFM-BoldItalic", "Bold Italic", weight: 9, italic: true)
]

private let helveticaNeue = [
    face("HelveticaNeue", "Regular", weight: 5),
    face("HelveticaNeue-Italic", "Italic", weight: 5, italic: true),
    face("HelveticaNeue-UltraLight", "UltraLight", weight: 2),
    face("HelveticaNeue-Thin", "Thin", weight: 3),
    face("HelveticaNeue-Light", "Light", weight: 3),
    face("HelveticaNeue-LightItalic", "Light Italic", weight: 3, italic: true),
    face("HelveticaNeue-Medium", "Medium", weight: 6),
    face("HelveticaNeue-MediumItalic", "Medium Italic", weight: 6, italic: true),
    face("HelveticaNeue-Bold", "Bold", weight: 9),
    face("HelveticaNeue-BoldItalic", "Bold Italic", weight: 9, italic: true),
    face("HelveticaNeue-CondensedBlack", "Condensed Black", weight: 11)
]

@Suite("font face resolution")
struct TypographyPresentationTests {
    @Test("a four-face family pairs exactly")
    func fourFaceFamily() throws {
        let resolved = try #require(
            TypographyPresentation.resolveFaces(members: hack, preferredPostScriptName: nil)
        )
        #expect(resolved.regular.postScriptName == "HackNFM-Regular")
        #expect(resolved.bold.postScriptName == "HackNFM-Bold")
        #expect(resolved.italic?.postScriptName == "HackNFM-Italic")
        #expect(resolved.boldItalic?.postScriptName == "HackNFM-BoldItalic")
    }

    @Test("a many-weight family pairs regular with bold, not with the heaviest")
    func manyWeightFamilyFromRegular() throws {
        let resolved = try #require(
            TypographyPresentation.resolveFaces(members: helveticaNeue, preferredPostScriptName: nil)
        )
        #expect(resolved.regular.postScriptName == "HelveticaNeue")
        #expect(resolved.bold.postScriptName == "HelveticaNeue-Bold")
    }

    /// The case that makes "just take the heaviest face" wrong: Light against
    /// Black is not a weight step, it is two different typefaces on one line.
    @Test("a light base pairs with a middle weight, not with black")
    func manyWeightFamilyFromLight() throws {
        let resolved = try #require(
            TypographyPresentation.resolveFaces(
                members: helveticaNeue,
                preferredPostScriptName: "HelveticaNeue-Light"
            )
        )
        #expect(resolved.regular.postScriptName == "HelveticaNeue-Light")
        #expect(resolved.bold.postScriptName == "HelveticaNeue-Medium")
        #expect(resolved.italic?.postScriptName == "HelveticaNeue-LightItalic")
        #expect(resolved.boldItalic?.postScriptName == "HelveticaNeue-MediumItalic")
    }

    @Test("a single-face family reuses the base rather than synthesizing")
    func singleFaceFamily() throws {
        let members = [face("Chalkduster", "Regular", weight: 5)]
        let resolved = try #require(
            TypographyPresentation.resolveFaces(members: members, preferredPostScriptName: nil)
        )
        #expect(resolved.regular.postScriptName == "Chalkduster")
        #expect(resolved.bold.postScriptName == "Chalkduster")
        #expect(resolved.italic == nil)
        #expect(resolved.boldItalic == nil)
    }

    /// A stored face survives in user defaults after the font is replaced by a
    /// version that no longer ships it.
    @Test("a missing stored face falls back to the family default")
    func missingStoredFace() throws {
        let resolved = try #require(
            TypographyPresentation.resolveFaces(
                members: hack,
                preferredPostScriptName: "HackNFM-SemiBoldWhichNeverExisted"
            )
        )
        #expect(resolved.regular.postScriptName == "HackNFM-Regular")
        #expect(resolved.bold.postScriptName == "HackNFM-Bold")
    }

    @Test("an empty family resolves to nothing")
    func emptyFamily() {
        #expect(TypographyPresentation.resolveFaces(members: [], preferredPostScriptName: nil) == nil)
    }

    @Test("base options are upright only, lightest first")
    func baseOptionsExcludeItalics() {
        let options = TypographyPresentation.baseFaceOptions(members: helveticaNeue)
        #expect(options.allSatisfy { !$0.isItalic })
        #expect(options.map(\.weight) == options.map(\.weight).sorted())
        #expect(options.first?.displayName == "UltraLight")
        #expect(options.last?.displayName == "Condensed Black")
    }

    /// A family of nothing but slanted faces would otherwise offer no base at
    /// all, making it unselectable.
    @Test("an all-italic family still offers its faces as bases")
    func allItalicFamily() {
        let members = [
            face("Fake-Italic", "Italic", weight: 5, italic: true),
            face("Fake-BoldItalic", "Bold Italic", weight: 9, italic: true)
        ]
        let options = TypographyPresentation.baseFaceOptions(members: members)
        #expect(options.count == 2)
    }
}
