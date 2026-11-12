import SwiftUI

extension View {
    /// Standard scroll-edge treatment for scrollable content in the SwiftUI shell.
    ///
    /// Defaults to `.soft` — the Notes-style progressive frost, validated on macOS 26.5.2
    /// (earlier 26.x builds never rendered it, which is why call sites used to force
    /// `.hard`). `.soft` only renders at system-toolbar edges or under `safeAreaBar`
    /// mounts — a `safeAreaInset` bar gets no edge effect at all.
    func toolbarScrollEdge(style: ScrollEdgeEffectStyle = .soft) -> some View {
        scrollContentBackground(.hidden)
            .scrollEdgeEffectStyle(style, for: .top)
    }

    /// Opaque backing + scroll-edge treatment for the middle source-list (subnav) column.
    /// Replaces the translucent `.sidebar` vibrancy with a flat, lightly elevated fill so
    /// the column reads as a solid panel rather than glass (user preference, 2026-06-15).
    func subnavColumnSurface() -> some View {
        scrollContentBackground(.hidden)
            .background(ReviewColors.fileHeaderBackground)
            .scrollEdgeEffectStyle(.soft, for: .top)
    }
}
