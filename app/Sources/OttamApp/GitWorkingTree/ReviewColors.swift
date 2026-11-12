import AppKit
import SwiftUI

/// Color palette for the diff reviewer's review surface.
///
/// Grayscale surfaces/text resolve per appearance (light/dark) via dynamic `NSColor`s so
/// the diff matches the surrounding chrome in both modes. Accent and syntax colors also
/// resolve per appearance so light-mode diff bands stay readable without flattening dark mode.
enum ReviewColors {
    private static func resolvedColor(
        appearance: NSAppearance,
        light: (Double, Double, Double),
        dark: (Double, Double, Double),
        lightAlpha: Double = 1,
        darkAlpha: Double = 1
    ) -> NSColor {
        let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let (r, g, b) = isDark ? dark : light
        let alpha = isDark ? darkAlpha : lightAlpha
        return NSColor(srgbRed: r, green: g, blue: b, alpha: alpha)
    }

    /// An `NSColor` that resolves to `light` in Aqua and `dark` in Dark Aqua.
    /// Exposed as `NSColor` where the markdown engine's `MarkdownTheme` consumes it.
    private static func dynamicNSColor(
        light: (Double, Double, Double),
        dark: (Double, Double, Double),
        lightAlpha: Double = 1,
        darkAlpha: Double = 1
    ) -> NSColor {
        NSColor(name: nil) { appearance in
            resolvedColor(
                appearance: appearance,
                light: light,
                dark: dark,
                lightAlpha: lightAlpha,
                darkAlpha: darkAlpha
            )
        }
    }

    /// A `Color` that resolves to `light` in Aqua and `dark` in Dark Aqua.
    private static func dynamic(
        light: (Double, Double, Double),
        dark: (Double, Double, Double),
        lightAlpha: Double = 1,
        darkAlpha: Double = 1
    ) -> Color {
        Color(nsColor: dynamicNSColor(light: light, dark: dark, lightAlpha: lightAlpha, darkAlpha: darkAlpha))
    }

    // Editor surface (the card the diff rows sit on).
    static let editorBackgroundNS = dynamicNSColor(light: (1.0, 1.0, 1.0), dark: (0.072, 0.072, 0.075))
    static let editorBackground = Color(nsColor: editorBackgroundNS)
    static let fileHeaderBackground = dynamic(light: (0.95, 0.95, 0.96), dark: (0.13, 0.13, 0.135))

    // Hairline that separates each card from the backdrop: subtle on whichever surface.
    static let cardBorderNS = dynamicNSColor(
        light: (0.0, 0.0, 0.0),
        dark: (1.0, 1.0, 1.0),
        lightAlpha: 0.08,
        darkAlpha: 0.08
    )
    static let cardBorder = Color(nsColor: cardBorderNS)

    // Row tints use pale opaque fills in light mode so syntax stays legible, while dark
    // mode keeps the previous translucent washes over the editor background.
    static let additionBackground = dynamic(
        light: (0.90, 0.97, 0.92),
        dark: (0.10, 0.20, 0.13),
        darkAlpha: 0.55
    )
    static let deletionBackground = dynamic(
        light: (1.0, 0.91, 0.90),
        dark: (0.24, 0.10, 0.10),
        darkAlpha: 0.55
    )
    static let hunkBackground = dynamic(
        light: (0.95, 0.96, 0.975),
        dark: (0.12, 0.16, 0.21),
        darkAlpha: 0.6
    )
    static let separatorBackground = dynamic(light: (0.965, 0.965, 0.97), dark: (0.17, 0.17, 0.185))

    // Change markers / gutters
    static let additionAccent = dynamic(light: (0.0, 0.56, 0.23), dark: (0.36, 0.78, 0.45))
    static let deletionAccent = dynamic(light: (0.80, 0.16, 0.16), dark: (0.88, 0.40, 0.40))

    // Text
    static let primaryTextNS = dynamicNSColor(light: (0.13, 0.13, 0.14), dark: (0.86, 0.86, 0.86))
    static let primaryText = Color(nsColor: primaryTextNS)
    static let secondaryTextNS = dynamicNSColor(light: (0.40, 0.40, 0.42), dark: (0.56, 0.56, 0.56))
    static let secondaryText = Color(nsColor: secondaryTextNS)
    static let tertiaryText = dynamic(light: (0.56, 0.56, 0.58), dark: (0.42, 0.42, 0.42))

    // Syntax
    static let keywordNS = dynamicNSColor(light: (0.72, 0.25, 0.0), dark: (0.95, 0.53, 0.22))
    static let keyword = Color(nsColor: keywordNS)
    static let stringNS = dynamicNSColor(light: (0.0, 0.46, 0.10), dark: (0.54, 0.82, 0.53))
    static let string = Color(nsColor: stringNS)
    static let typeNS = dynamicNSColor(light: (0.45, 0.18, 0.86), dark: (0.71, 0.50, 0.98))
    static let type = Color(nsColor: typeNS)
    static let propertyNS = dynamicNSColor(light: (0.0, 0.44, 0.58), dark: (0.42, 0.78, 0.84))
    static let property = Color(nsColor: propertyNS)
    static let numberNS = dynamicNSColor(light: (0.55, 0.38, 0.0), dark: (0.85, 0.70, 0.42))
    static let number = Color(nsColor: numberNS)
    static let commentNS = dynamicNSColor(light: (0.50, 0.50, 0.52), dark: (0.45, 0.45, 0.45))
    static let comment = Color(nsColor: commentNS)
}
