import OttamBackendClient
import OttamMarkdownEngine
import SwiftUI

/// A user turn's image attachments, hosted. The Quick Look controller is
/// owned by the attachment (one per row, so the panel's arrow keys walk that
/// message's images) and injected into the row, which no longer holds it as
/// `@State`.
struct AttachmentsCardPayload: HostedCardPayload {
    struct Actions {
        var quickLook: ChatAttachmentQuickLook
    }

    var attachments: [ConversationAttachment]

    var signature: AnyHashable { attachments.map(\.attachmentID.rawValue) }

    func makeBody(actions: Actions) -> some View {
        HStack {
            Spacer(minLength: 80)
            ChatAttachmentCardRow(
                attachments: attachments,
                size: 108,
                alignment: .trailing,
                scrolls: false,
                quickLook: actions.quickLook
            )
        }
    }
}

typealias HostedAttachmentsCard = HostedCard<AttachmentsCardPayload>
