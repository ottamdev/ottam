import OttamAccessibility
import OttamPresentationLogic
import SwiftUI

/// Quote replies staged in the composer, in the order they will be sent. Each
/// reads like its sent form: the quoted passage under a bar, the comment below.
struct ChatQuoteCommentList: View {
    let comments: [QuoteReply.Comment]
    let onRemove: (QuoteReply.Comment) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(comments) { comment in
                row(comment)
            }
        }
    }

    private func row(_ comment: QuoteReply.Comment) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Capsule()
                .fill(.tertiary)
                .frame(width: 2.5)
            VStack(alignment: .leading, spacing: 2) {
                Text(QuoteReply.headerText(for: comment.quote))
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text(comment.comment)
                    .appFont(.callout)
                    .lineLimit(2)
                    .truncationMode(.tail)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button {
                onRemove(comment)
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .frame(width: 18, height: 18)
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .help("Remove Reply")
            .accessibilityLabel("Remove Reply")
            .accessibilityIdentifier(AccessibilityID.chatQuoteReplyRemove)
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(.vertical, 6)
        .padding(.horizontal, 8)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}
