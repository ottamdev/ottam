import OttamAccessibility
import SwiftUI

/// A file's frontmatter as a property list above its body: muted labels,
/// values in the body face, and a rule where the body starts.
struct FrontmatterCardPayload: HostedCardPayload {
    struct Actions {}

    var frontmatter: MarkdownFrontmatter

    var signature: AnyHashable {
        frontmatter.fields.map { "\($0.key)\u{1F}\($0.value)" }
    }

    func makeBody(actions: Actions) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 24, verticalSpacing: 10) {
                ForEach(Array(frontmatter.fields.enumerated()), id: \.offset) { _, field in
                    GridRow {
                        Text(field.label)
                            .appFont(.callout)
                            .foregroundStyle(.secondary)
                            .gridColumnAlignment(.leading)
                        Text(field.value)
                            .appFont(.body)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            Divider()
        }
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatFileFrontmatter)
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatFileFrontmatter,
            value: frontmatter.fields.map { "\($0.label): \($0.value)" }.joined(separator: ", ")
        )
    }
}

typealias HostedFrontmatterCard = HostedCard<FrontmatterCardPayload>
