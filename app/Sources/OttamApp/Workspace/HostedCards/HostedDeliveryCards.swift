import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import OttamPresentationLogic
import SwiftUI

/// The recovery card under a user turn whose delivery is in doubt, hosted in
/// the native transcript. The card sits on the user's side: trailing, like the
/// bubble above it.
struct DeliveryRecoveryCardPayload: HostedCardPayload {
    struct Actions {
        var onResolve: (Bool) -> Void
    }

    var presentation: ChatDeliveryRecoveryPresentation
    var isMutating: Bool

    var signature: AnyHashable { "\(presentation.title)|\(presentation.detail)|\(presentation.secondaryAction != nil)" }

    func makeBody(actions: Actions) -> some View {
        HStack {
            Spacer(minLength: 80)
            ChatDeliveryRecoveryCard(presentation: presentation, isMutating: isMutating, resolve: actions.onResolve)
                .frame(maxWidth: 560, alignment: .leading)
        }
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatDeliveryRecovery,
            value: presentation.accessibilityValue
        )
    }
}

typealias HostedDeliveryRecoveryCard = HostedCard<DeliveryRecoveryCardPayload>

struct ChatDeliveryRecoveryCard: View {
    let presentation: ChatDeliveryRecoveryPresentation
    let isMutating: Bool
    let resolve: (Bool) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(
                presentation.title,
                systemImage: "exclamationmark.arrow.triangle.2.circlepath"
            )
                .appFont(.body, .bold)
                .accessibilityIdentifier(AccessibilityID.chatDeliveryRecovery)
                .accessibilityValue(presentation.accessibilityValue)
            Text(presentation.detail)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Button(presentation.primaryAction.title) {
                    resolve(true)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(
                    presentation.primaryAction == .startRetry
                        ? AccessibilityID.chatPendingRetryStart
                        : AccessibilityID.chatDeliveryRetry
                )
                if let secondaryAction = presentation.secondaryAction {
                    Button(secondaryAction.title) {
                        resolve(false)
                    }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier(
                        AccessibilityID.chatDeliveryMarkDelivered
                    )
                }
                Spacer()
            }
        }
        .padding(12)
        .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
        .disabled(isMutating)
    }
}

// The one-line delivery state inside a user message's trailer. Routine states —
// pending, in flight, adapter-confirmed delivered — are tracked internally and
// never labeled; only outcomes worth acting on surface here.
struct ChatRunInputStateLabel: View {
    let status: ChatDeliveryStatusPresentation
    /// Offered on a failed input, which never reached the agent and so can be
    /// re-sent without duplicating anything. Absent when a card above already
    /// owns the resend.
    var resend: (() -> Void)?
    var isMutating = false

    var body: some View {
        HStack(spacing: 8) {
            if status.isUserMarkedDelivered {
                Text(status.label)
                    .appFont(.callout)
                    .foregroundStyle(color)
                    .accessibilityIdentifier(AccessibilityID.chatMarkedDelivered)
            } else {
                Text(status.label)
                    .appFont(.callout)
                    .foregroundStyle(color)
            }
            if let action = status.action, let resend {
                Button(action.title, action: resend)
                    .buttonStyle(.link)
                    .appFont(.callout)
                    .disabled(isMutating)
            }
        }
        .padding(.trailing, 4)
    }

    private var color: Color {
        status.isWarning ? .orange : .secondary.opacity(0.7)
    }
}
