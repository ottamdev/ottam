import OttamAccessibility
import OttamMarkdownEngine
import SwiftUI

/// The selection recovery card, hosted. The replacement is derived from the
/// composer's current pick, so changing the picker is a payload change that
/// retitles the resend.
struct SelectionRecoveryCardPayload: HostedCardPayload {
    struct Actions {
        var onSendAgain: () -> Void
    }

    var recovery: ChatSelectionRecovery
    var isMutating: Bool

    var signature: AnyHashable {
        "\(recovery.title)|\(recovery.explanation)|\(recovery.sendAgainTitle)|\(recovery.canResend)"
    }

    func makeBody(actions: Actions) -> some View {
        ChatSelectionRecoveryCard(recovery: recovery, isMutating: isMutating, sendAgain: actions.onSendAgain)
            .frame(maxWidth: 560, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(identifier: AccessibilityID.chatSelectionRecovery, value: nil)
    }
}

typealias HostedSelectionRecoveryCard = HostedCard<SelectionRecoveryCardPayload>

/// A run refused because its model, mode or effort is no longer offered. The
/// fix is a choice only the user can make, so the card says so plainly and
/// names what Send Again will run with.
struct ChatSelectionRecoveryCard: View {
    let recovery: ChatSelectionRecovery
    let isMutating: Bool
    let sendAgain: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(recovery.title, systemImage: "exclamationmark.triangle")
                .appFont(.body, .bold)
                .accessibilityIdentifier(AccessibilityID.chatSelectionRecovery)
            Text(recovery.explanation)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if recovery.canResend {
                Button(recovery.sendAgainTitle, action: sendAgain)
                    .buttonStyle(.borderedProminent)
                    .disabled(isMutating)
                    .accessibilityIdentifier(AccessibilityID.chatSelectionSendAgain)
            }
        }
        .padding(12)
        .background(Color.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
    }
}

extension ChatSelectionRecovery {
    private var noun: String {
        switch setting {
        case .model: "model"
        case .mode: "permission mode"
        case .thoughtLevel: "effort level"
        case nil: "setting"
        }
    }

    var title: String {
        switch setting {
        case .model: "Model no longer available"
        case .mode: "Permission mode no longer available"
        case .thoughtLevel: "Effort level no longer available"
        case nil: "Setting no longer available"
        }
    }

    var explanation: String {
        guard setting != nil, let staleValue else {
            return detail ?? "\(engine.displayName) no longer offers a setting this message used. "
                + "Choose an available one, then send again."
        }
        return "\(engine.displayName) no longer offers “\(staleValue)”. "
            + "Choose an available \(noun) to continue. Send Again uses \(replacement), "
            + "or pick another \(noun) in the message box first."
    }

    /// Also when nothing was named: a restored session whose mode set failed
    /// or came back different refuses with every value still offered, and a
    /// resend replaying them unchanged can succeed once the adapter recovers.
    /// Where the agent reported no mode selector at all it fails again, and
    /// the card returns; that beats a message that can never be resent.
    var canResend: Bool {
        undeliveredInput != nil
    }

    var sendAgainTitle: String {
        setting == nil ? "Send Again" : "Send Again with \(replacement)"
    }
}
