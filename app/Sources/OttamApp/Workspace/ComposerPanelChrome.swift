import SwiftUI

/// The glass card every panel above the composer shares: the composer's
/// material and corners, materializing in place as it comes and dissolving
/// as it goes. Focus rules stay with each panel.
struct ComposerPanelChrome: ViewModifier {
    static let cornerRadius: CGFloat = 16

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
        content
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            // The card collapses from its top while its bottom stays on the
            // composer; unclipped, its content would slide out over the composer.
            .clipShape(shape)
            .glassEffect(.regular, in: shape)
            .glassEffectTransition(.materialize)
            .contentShape(shape)
    }
}

extension View {
    func composerPanelChrome() -> some View {
        modifier(ComposerPanelChrome())
    }
}
