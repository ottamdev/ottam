import OttamBackendClient
import OttamMarkdownEngine
import SwiftUI

/// `ChatPermissionCard` as an engine-hosted card: the first stage-3.2
/// conformer. The disclosure toggle is payload state (height purity); the
/// pending permission rides along for the view, with its identity and tool
/// call in the signature.
struct PermissionCardPayload: HostedCardPayload {
    struct Actions {
        var onResolve: (String?) -> Void
        /// Wired by `HostedCard.permission(...)` to flip
        /// `showsDetails` on the owning card.
        var onToggleDetails: () -> Void = {}
    }

    let permission: ChatPendingPermission
    var showsDetails = false

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.permission === rhs.permission && lhs.showsDetails == rhs.showsDetails
    }

    var signature: AnyHashable {
        "\(permission.id)|\(showsDetails)|\(chatPermissionToolDetails(permission.request.toolCall))"
    }

    func makeBody(actions: Actions) -> some View {
        ChatPermissionCard(
            permission: permission,
            showsDetails: showsDetails,
            onToggleDetails: actions.onToggleDetails,
            onResolve: actions.onResolve
        )
    }

    /// A fresh payload for the same permission never collapses an open
    /// disclosure.
    mutating func adopt(_ fresh: Self) {
        let showsDetails = self.showsDetails
        self = fresh
        self.showsDetails = showsDetails
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        actions.onToggleDetails = { [weak card] in
            card?.updatePayload { $0.showsDetails.toggle() }
        }
    }
}

typealias HostedPermissionCard = HostedCard<PermissionCardPayload>

extension HostedCard where Payload == PermissionCardPayload {
    /// Builds the card with its disclosure toggle wired back to this
    /// card's payload.
    static func permission(
        _ permission: ChatPendingPermission,
        theme: MarkdownTheme,
        environment: HostedCardEnvironment,
        onResolve: @escaping (String?) -> Void
    ) -> HostedPermissionCard {
        let card = HostedPermissionCard(
            payload: PermissionCardPayload(permission: permission),
            actions: PermissionCardPayload.Actions(onResolve: onResolve),
            theme: theme,
            environment: environment
        )
        return card
    }
}
