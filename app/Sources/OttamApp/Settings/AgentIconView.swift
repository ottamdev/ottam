import AppKit
import SwiftUI

/// The rounded-tile brand icon for an agent, Xcode Intelligence-style: a
/// brand-colored tile carrying the vendor's glyph. Glyphs ship as vector PDFs
/// in AppResources (Claude starburst from Anthropic's mark, OpenAI blossom,
/// OpenCode's mark, omp's pi) and render as templates so the tile owns the
/// colors, except a multi-color mark, which keeps its own. Tile treatments follow each
/// vendor's own: Claude terracotta, OpenAI white, OpenCode near-black, omp
/// near-black violet.
///
/// Keyed by the `brand` core ships on the agent, not by agent id, so every
/// agent from a known vendor shares its tile; no brand, or one this catalog
/// lacks, draws the name's monogram.
struct AgentIconView: View {
    let brand: String?
    let name: String
    var size: CGFloat = 36
    /// A circle instead of the tile, as in the agents pill's stack. A mark
    /// drawn to fill its tile shrinks to sit inside the circle.
    var round = false

    private var catalogEntry: AgentBrand? {
        brand.flatMap { AgentBrand.catalog[$0] }
    }

    private var tileShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: round ? size / 2 : size * 0.24, style: round ? .circular : .continuous)
    }

    var body: some View {
        ZStack {
            tileShape
                .fill(catalogEntry?.background ?? Color(nsColor: .quaternarySystemFill))

            if let entry = catalogEntry, let glyph = AgentBrand.glyph(named: entry.resource, template: entry.glyphColor != nil) {
                let scale = round ? min(entry.glyphScale, 0.7) : entry.glyphScale
                Image(nsImage: glyph)
                    .renderingMode(entry.glyphColor == nil ? .original : .template)
                    .resizable()
                    .scaledToFit()
                    .foregroundStyle(entry.glyphColor ?? .primary)
                    .frame(width: size * scale, height: size * scale)
            } else {
                Text(name.prefix(1).uppercased())
                    .font(.system(size: size * 0.45, weight: .semibold, design: .rounded))
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: size, height: size)
        // The hairline keeps the white OpenAI tile from dissolving into light
        // backgrounds.
        .overlay(tileShape.stroke(.quaternary, lineWidth: 1))
        .accessibilityHidden(true)
    }
}

private struct AgentBrand {
    let resource: String
    let background: Color
    /// `nil` keeps the glyph's own colors.
    let glyphColor: Color?
    let glyphScale: CGFloat

    static let catalog: [String: AgentBrand] = [
        "anthropic": AgentBrand(
            resource: "agent-claude",
            background: Color(red: 0.85, green: 0.47, blue: 0.34),
            glyphColor: .white,
            glyphScale: 0.62
        ),
        "openai": AgentBrand(
            resource: "agent-openai",
            background: .white,
            glyphColor: .black,
            glyphScale: 0.62
        ),
        // OpenCode's mark from `packages/identity/mark.svg` (MIT): a white
        // frame over a gray square, drawn on a 512-point page at its own
        // proportions, so it fills the tile as the vendor's icon does.
        "anomaly": AgentBrand(
            resource: "agent-opencode",
            background: Color(red: 0.075, green: 0.063, blue: 0.063),
            glyphColor: nil,
            glyphScale: 1
        ),
        // omp.sh's favicon (oh-my-pi, MIT): the gradient pi on a 64-point
        // page at its own inset, so it sits in the tile as on the site.
        "omp": AgentBrand(
            resource: "agent-omp",
            background: Color(red: 0.059, green: 0.039, blue: 0.078),
            glyphColor: nil,
            glyphScale: 1
        ),
    ]

    /// Loaded once per resource. Bundle.main, not Bundle.module: the app
    /// bundle copies AppResources into Contents/Resources directly, and the
    /// SPM resource bundle is not part of the packaged app (Bundle.module
    /// would fatalError there). Under `swift test` these resolve to nil and
    /// the monogram fallback renders instead.
    private static var cache: [String: NSImage] = [:]

    @MainActor
    static func glyph(named resource: String, template: Bool) -> NSImage? {
        if let cached = cache[resource] {
            return cached
        }
        guard let url = Bundle.main.url(forResource: resource, withExtension: "pdf"),
              let image = NSImage(contentsOf: url) else {
            return nil
        }
        image.isTemplate = template
        cache[resource] = image
        return image
    }
}
