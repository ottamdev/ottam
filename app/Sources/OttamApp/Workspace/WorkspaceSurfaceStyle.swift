import AppKit
import SwiftUI

/// Shared surface treatment for ordinary workspace content cards and chips.
///
/// Per the macOS-native styling audit (item 10), blur materials are reserved for
/// system-owned chrome — inspectors, popovers, sheets, sidebars, toolbars. Static
/// content cards instead use these flat semantic fills so they read as opaque
/// surfaces rather than floating glass, and stay legible over the window background.
enum WorkspaceSurfaceStyle {
    static let cornerRadius: CGFloat = 8
}

extension ShapeStyle where Self == Color {
    /// Flat fill for ordinary content cards and dense record rows.
    static var workspaceCardSurface: Color { .workspaceCardSurface }

    /// Flat fill for small chips and capsules (tags, metadata, starters).
    static var workspaceChipSurface: Color { .workspaceChipSurface }
}

extension Color {
    /// Stable opaque canvas shared by Workspace detail routes. Keeping this
    /// app-owned avoids position-dependent wallpaper tint while leaving the
    /// NavigationStack, toolbar, and sidebar system-owned.
    static let workspaceCanvas = Color(nsColor: NSColor(name: nil) { appearance in
        appearance.isWorkspaceDarkMode
            ? NSColor(srgbRed: 0.105, green: 0.105, blue: 0.11, alpha: 1)
            : NSColor(srgbRed: 0.91, green: 0.91, blue: 0.925, alpha: 1)
    })

    static let workspaceCardSurface = Color(nsColor: NSColor(name: nil) { appearance in
        appearance.isWorkspaceDarkMode
            ? NSColor(calibratedWhite: 1.0, alpha: 0.07)
            : NSColor(calibratedWhite: 1.0, alpha: 0.55)
    })

    static let workspaceChipSurface = Color(nsColor: NSColor(name: nil) { appearance in
        appearance.isWorkspaceDarkMode
            ? NSColor(calibratedWhite: 1.0, alpha: 0.06)
            : NSColor(calibratedWhite: 0.0, alpha: 0.04)
    })
}

extension View {
    /// Opaque semantic card surface with a hairline border, replacing content-layer
    /// blur material on static cards and rows.
    func workspaceCardSurface(cornerRadius: CGFloat = WorkspaceSurfaceStyle.cornerRadius) -> some View {
        background(Color.workspaceCardSurface, in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .stroke(.quaternary, lineWidth: 1)
            )
    }
}

private extension NSAppearance {
    var isWorkspaceDarkMode: Bool {
        bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
    }
}

/// Small pasteboard helper so content views can offer Copy context-menu actions without
/// importing AppKit directly.
enum WorkspacePasteboard {
    static func copy(_ string: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(string, forType: .string)
    }
}
