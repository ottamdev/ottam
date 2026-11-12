import AppKit
import OttamAccessibility
import OttamMarkdownEngine
import SwiftUI

/// The Plan tab: the latest plan an agent asked to approve, read-only. The
/// decision itself is taken in the panel above the composer.
struct ConversationPlanTool: View {
    let plan: ChatPlan?
    /// The conversation's root: relative links in the plan resolve here.
    let resourceRoot: URL?
    let openLink: (URL) -> Void

    var body: some View {
        if let plan {
            VStack(spacing: 0) {
                HStack(spacing: 6) {
                    Text(plan.title)
                        .appFont(.body)
                        .fontWeight(.semibold)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                Divider()
                MarkdownDocumentView(
                    id: "plan",
                    markdown: plan.markdown,
                    accessibilityIdentifier: AccessibilityID.chatPlanTab,
                    resourceRoot: resourceRoot,
                    openLink: openLink
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            ContentUnavailableView(
                "No Plan",
                systemImage: "list.bullet.clipboard",
                description: Text("A plan the agent asks you to approve shows here.")
            )
        }
    }
}
