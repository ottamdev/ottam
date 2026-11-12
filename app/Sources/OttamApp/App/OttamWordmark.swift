import SwiftUI

/// "ottam" drawn in the thread's own stroke rather than set in a font, so the
/// name matches `OttamThreadMark` wherever the two sit together. Geometry
/// matches the wordmark in `assets/brand/lockup-light.svg`.
///
/// The view keeps the wordmark's aspect ratio and takes the current foreground
/// style, so it follows light and dark mode.
struct OttamWordmark: View {
    var body: some View {
        WordmarkShape()
            .fill(.foreground)
            .aspectRatio(WordmarkShape.size, contentMode: .fit)
            .accessibilityElement()
            .accessibilityLabel("Ottam")
            .accessibilityAddTraits(.isStaticText)
    }
}

/// The stroked outline of the five letters. Units are the lockup's: the o and
/// a share the mark's 24-unit loop radius and the stroke is the thread's 8.
private struct WordmarkShape: Shape {
    private static let radius: CGFloat = 24
    private static let stroke: CGFloat = 8
    private static let xHeight = 2 * radius
    private static let ascent = xHeight + 0.8 * radius
    /// Centreline gaps, tuned by eye: a round o beside a crossbar reads
    /// looser than two stems do.
    private static let gaps: [CGFloat] = [11, 12, 14, 18]
    private static let advances: [CGFloat] = [48, 24, 24, 48, 57.6]

    /// Ink bounds, which is the centreline box grown by half the stroke.
    static let size = CGSize(
        width: advances.reduce(0, +) + gaps.reduce(0, +) + stroke,
        height: ascent + stroke
    )

    func path(in rect: CGRect) -> Path {
        let unit = min(rect.width / Self.size.width, rect.height / Self.size.height)
        let originX = rect.midX - Self.size.width * unit / 2 + Self.stroke * unit / 2
        let baseline = rect.midY + Self.size.height * unit / 2 - Self.stroke * unit / 2
        func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            // `y` counts up from the baseline.
            CGPoint(x: originX + x * unit, y: baseline - y * unit)
        }

        let r = Self.radius
        var letters = Path()
        var x: CGFloat = 0

        func bowl() {
            letters.addEllipse(in: CGRect(
                x: originX + x * unit, y: baseline - 2 * r * unit,
                width: 2 * r * unit, height: 2 * r * unit))
        }
        func footedT() {
            let stem = x + 0.42 * r
            let foot = 0.5 * r
            letters.move(to: point(stem, Self.ascent))
            letters.addLine(to: point(stem, foot))
            // Down the stem and out to the right. `clockwise` is mirrored in
            // SwiftUI's flipped coordinate space.
            letters.addArc(
                center: point(stem + foot, foot), radius: foot * unit,
                startAngle: .degrees(180), endAngle: .degrees(90), clockwise: true)
            letters.addLine(to: point(x + r, 0))
            letters.move(to: point(x, Self.xHeight))
            letters.addLine(to: point(x + r, Self.xHeight))
        }
        func stem(at stemX: CGFloat) {
            letters.move(to: point(stemX, Self.xHeight))
            letters.addLine(to: point(stemX, 0))
        }
        func arch(from archX: CGFloat) {
            let archRadius = 0.6 * r
            letters.move(to: point(archX, Self.xHeight - archRadius))
            letters.addArc(
                center: point(archX + archRadius, Self.xHeight - archRadius), radius: archRadius * unit,
                startAngle: .degrees(180), endAngle: .degrees(360), clockwise: false)
            letters.addLine(to: point(archX + 2 * archRadius, 0))
        }

        bowl()                                  // o
        x += Self.advances[0] + Self.gaps[0]
        footedT()                               // t
        x += Self.advances[1] + Self.gaps[1]
        footedT()                               // t
        x += Self.advances[2] + Self.gaps[2]
        bowl()                                  // a
        stem(at: x + 2 * r)
        x += Self.advances[3] + Self.gaps[3]
        stem(at: x)                             // m
        arch(from: x)
        arch(from: x + 1.2 * r)

        // Normalizing merges the overlapping strokes (the a's bowl and stem,
        // the m's arches) into one outline.
        return letters
            .strokedPath(StrokeStyle(lineWidth: Self.stroke * unit, lineCap: .round, lineJoin: .round))
            .normalized(eoFill: false)
    }
}
