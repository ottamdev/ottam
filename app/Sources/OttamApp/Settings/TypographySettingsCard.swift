import OttamPresentationLogic
import SwiftUI

/// The two font choices, in the App pane.
///
/// The card itself is chrome, so nothing in it uses `appFont` — except the
/// preview lines, which are the whole point of the card and deliberately render
/// in the pending selection.
struct TypographySettingsCard: View {
    @Bindable var settings: TypographySettings

    var body: some View {
        SettingsCard {
            TypographyRow(
                title: "UI Font",
                subtitle: "Chat, code blocks and everything else the agent writes.",
                families: FontCatalog.families,
                family: $settings.uiFamily,
                face: $settings.uiFacePostScriptName,
                sizeDelta: $settings.uiSizeDelta,
                faceOptions: settings.faceOptions(forFamily: settings.uiFamily),
                resolvedFace: settings.resolved.ui.regular,
                previewText: "The quick brown fox jumps over the lazy dog",
                // The chat's reading size, since that is what the subtitle
                // promises; controls sit one step under it.
                previewFont: settings.resolved.font(.prose),
                previewSize: settings.resolved.points(for: .prose)
            )

            SettingsCardDivider()

            TypographyRow(
                title: "Code Font",
                subtitle: "Diffs and code blocks. The terminal follows the size only.",
                families: FontCatalog.monospaceFamilies,
                family: $settings.codeFamily,
                face: $settings.codeFacePostScriptName,
                sizeDelta: $settings.codeSizeDelta,
                faceOptions: settings.faceOptions(forFamily: settings.codeFamily),
                resolvedFace: settings.resolved.code.regular,
                // Chosen for the distinctions a code font lives or dies on:
                // zero against capital O, one against lowercase l and capital I.
                previewText: "0O1lI  fn main() -> Result<(), Error>",
                previewFont: settings.resolved.font(.code),
                previewSize: settings.resolved.points(for: .code)
            )
        }
    }
}

private struct TypographyRow: View {
    let title: String
    let subtitle: String
    let families: [String]
    @Binding var family: String?
    @Binding var face: String?
    @Binding var sizeDelta: CGFloat
    let faceOptions: [FontFaceOption]
    let resolvedFace: String?
    let previewText: String
    let previewFont: Font
    let previewSize: CGFloat

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            SettingsCardRow(title, subtitle: subtitle) {
                HStack(spacing: 8) {
                    FontFamilyPicker(families: families, family: $family)

                    Picker("", selection: selectedFace) {
                        // The system font has no member list to pick from, but a
                        // disabled control with nothing in it reads as broken
                        // rather than as inapplicable. Name the face it draws.
                        if faceOptions.isEmpty {
                            Text("Regular").tag(String?.none)
                        }
                        ForEach(faceOptions) { option in
                            Text(option.displayName).tag(String?.some(option.postScriptName))
                        }
                    }
                    .labelsHidden()
                    .frame(width: 110)
                    // A system font has no face list of its own; its weight and
                    // slant come from the scale, not from a picked member.
                    .disabled(faceOptions.isEmpty)

                    Stepper(value: $sizeDelta, in: TypographySettings.sizeDeltaRange, step: 1) {
                        Text(Double(previewSize).formatted(.number.precision(.fractionLength(0...1))) + " pt")
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                            .frame(width: 44, alignment: .trailing)
                    }
                }
            }

            Text(previewText)
                .font(previewFont)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(.quaternary.opacity(0.4))
                )
                .padding(.horizontal, 14)
                .padding(.bottom, 12)
        }
    }

    /// The stored face is `nil` until the user picks one, but the picker still has
    /// to show *something* selected — whichever face the family actually resolved
    /// to, which is not the same as the first one listed.
    private var selectedFace: Binding<String?> {
        Binding(
            get: { face ?? resolvedFace },
            set: { face = $0 }
        )
    }
}
