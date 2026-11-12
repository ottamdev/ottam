import OttamAccessibility
import OttamMarkdownEngine
import SwiftUI

struct RunFailureCardPayload: HostedCardPayload {
    struct Actions {
        var onRetry: () -> Void
        var onToggleDetails: () -> Void = {}
    }

    var presentation: ChatRunFailurePresentation
    var showsDetails = false

    var signature: AnyHashable {
        "\(presentation.explanation)|\(presentation.canRetry)|\(showsDetails)"
    }

    func makeBody(actions: Actions) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Response failed", systemImage: "exclamationmark.triangle")
                .appFont(.body, .bold)
            Text(presentation.explanation)
                .appFont(.callout)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .accessibilityIdentifier(AccessibilityID.chatRunFailure)
            if let message = presentation.providerMessage {
                DisclosureGroup(isExpanded: Binding(
                    get: { showsDetails },
                    set: { _ in actions.onToggleDetails() }
                )) {
                    Text(message)
                        .appFont(.callout)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                } label: {
                    Text("Details").appFont(.callout)
                }
                .accessibilityIdentifier(AccessibilityID.chatRunFailureDetails)
            }
            if presentation.canRetry {
                Text("Retry asks the same agent to continue with its previous settings. The provider may reject another attempt.")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Retry", action: actions.onRetry)
                    .buttonStyle(.bordered)
                    .disabled(presentation.isMutating)
                    .accessibilityIdentifier(AccessibilityID.chatRunFailureRetry)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
    }

    mutating func adopt(_ fresh: Self) {
        let showsDetails = showsDetails
        self = fresh
        self.showsDetails = showsDetails
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        actions.onToggleDetails = { [weak card] in
            card?.updatePayload { $0.showsDetails.toggle() }
        }
    }
}

typealias HostedRunFailureCard = HostedCard<RunFailureCardPayload>
