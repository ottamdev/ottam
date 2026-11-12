import AppKit
import OttamWorkspaceLogic
import SwiftUI

/// A named agent's or delegated child's tile: one shape on one color,
/// both picked by the avatar's seed. It sits beside `AgentIconView`, whose
/// brand tiles stand for agent products, so it keeps their corner radius
/// and hairline.
///
/// The palette is the system colors, which already adapt to light and dark
/// appearance. The shape is white, or dark on the light hues where white
/// would wash out.
struct AgentAvatarView: View {
    let avatar: AgentAvatar
    var size: CGFloat = 36
    /// What VoiceOver reads. Nil hides the tile, for a place whose text
    /// already names the agent.
    var label: String?
    /// A circle instead of the tile, as in the agents pill's stack.
    var round = false

    private var tileShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: round ? size / 2 : size * 0.24, style: round ? .circular : .continuous)
    }

    /// Hues far enough apart to tell at 16 points, with no near-pairs such
    /// as red and pink or blue and indigo. `AgentAvatar.paletteCount`
    /// matches its length.
    private static let palette: [(color: Color, darkShape: Bool)] = [
        (.red, false), (.orange, false), (.yellow, true), (.green, false),
        (.teal, false), (.blue, false), (.purple, false), (.brown, false),
    ]

    var body: some View {
        let entry = Self.palette[avatar.paletteIndex % Self.palette.count]
        ZStack {
            // Flat, not a gradient: the shape's edge stays crisp at menu
            // and pill sizes.
            tileShape
                .fill(entry.color)
            AgentAvatarShape(shape: avatar.shape, lineWidth: size * 0.14)
                // Even-odd, so the ring's inner circle is a hole.
                .fill(entry.darkShape ? Color.black.opacity(0.65) : Color.white, style: FillStyle(eoFill: true))
                // Large enough to read at 16 points, and still inside the
                // pill's circle.
                .frame(width: size * 0.68, height: size * 0.68)
        }
        .frame(width: size, height: size)
        .overlay(tileShape.stroke(.quaternary, lineWidth: 1))
        .accessibilityElement()
        .accessibilityHidden(label == nil)
        .accessibilityLabel(label ?? "")
        .accessibilityAddTraits(.isImage)
    }
}

/// The tile's shapes, each filling its frame and upright. The triangle and
/// half-moon sit on their visual centers, not their bounding boxes, so they
/// don't look dropped or pushed aside.
private struct AgentAvatarShape: Shape {
    let shape: AgentAvatar.Shape
    let lineWidth: CGFloat

    func path(in rect: CGRect) -> Path {
        switch shape {
        case .circle:
            return Path(ellipseIn: rect)
        case .ring:
            var path = Path(ellipseIn: rect)
            path.addEllipse(in: rect.insetBy(dx: lineWidth, dy: lineWidth))
            return path
        case .square:
            return Path(roundedRect: rect.insetBy(dx: rect.width * 0.06, dy: rect.height * 0.06),
                        cornerRadius: rect.width * 0.12)
        case .diamond:
            return Path { path in
                path.move(to: CGPoint(x: rect.midX, y: rect.minY))
                path.addLine(to: CGPoint(x: rect.maxX, y: rect.midY))
                path.addLine(to: CGPoint(x: rect.midX, y: rect.maxY))
                path.addLine(to: CGPoint(x: rect.minX, y: rect.midY))
                path.closeSubpath()
            }
        case .triangle:
            // Near equilateral, its centroid a little below the middle,
            // where a triangle looks centered.
            return Path { path in
                path.move(to: CGPoint(x: rect.midX, y: rect.minY + rect.height * 0.02))
                path.addLine(to: CGPoint(x: rect.maxX - rect.width * 0.03, y: rect.minY + rect.height * 0.83))
                path.addLine(to: CGPoint(x: rect.minX + rect.width * 0.03, y: rect.minY + rect.height * 0.83))
                path.closeSubpath()
            }
        case .halfMoon:
            return Path { path in
                // The flat side right of the middle, so the half's
                // centroid (4r/3π from it) sits on the tile's center.
                path.addArc(
                    center: CGPoint(x: rect.midX + rect.width * 0.21, y: rect.midY), radius: rect.width / 2,
                    startAngle: .degrees(90), endAngle: .degrees(270), clockwise: false
                )
                path.closeSubpath()
            }
        }
    }
}

/// A view as an image for AppKit (a menu item, a toolbar item), drawn each
/// time AppKit draws it, so it takes the appearance and scale of wherever
/// it is shown. `ImageRenderer` alone renders in light appearance at one
/// fixed scale.
@MainActor
func appKitImage(size: CGSize, description: String? = nil, _ content: @escaping @MainActor () -> some View) -> NSImage {
    let image = NSImage(size: size, flipped: false) { rect in
        MainActor.assumeIsolated {
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            let dark = NSAppearance.currentDrawing().bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            let renderer = ImageRenderer(content: content().environment(\.colorScheme, dark ? .dark : .light))
            let scale = abs(context.userSpaceToDeviceSpaceTransform.a)
            renderer.scale = scale > 0 ? scale : 2
            guard let rendered = renderer.cgImage else { return false }
            context.draw(rendered, in: rect)
            return true
        }
    }
    image.accessibilityDescription = description
    return image
}

extension AgentAvatarView {
    /// The tile as an image, for a menu item.
    @MainActor
    static func image(_ avatar: AgentAvatar, size: CGFloat) -> NSImage {
        appKitImage(size: CGSize(width: size, height: size)) { AgentAvatarView(avatar: avatar, size: size) }
    }
}

extension AgentIconView {
    /// The brand tile as an image, for a menu item.
    @MainActor
    static func image(brand: String?, name: String, size: CGFloat) -> NSImage {
        appKitImage(size: CGSize(width: size, height: size)) { AgentIconView(brand: brand, name: name, size: size) }
    }
}
