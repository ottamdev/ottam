import OttamAccessibility
import OttamMarkdownEngine
import OttamPresentationLogic
import SwiftUI

/// The Sign in card for an MCP server the agent called while it was signed
/// out, hosted. The app's sign-in controller supplies the phase.
struct McpSignInCardPayload: HostedCardPayload {
    struct Actions {
        var onSignIn: () -> Void
        var onCancel: () -> Void
    }

    var signIn: ChatMcpSignIn

    var signature: AnyHashable {
        "\(signIn.serverID.rawValue)|\(signIn.accessibilityPhase)|\(signIn.failure ?? "")"
    }

    func makeBody(actions: Actions) -> some View {
        ChatMcpSignInCard(signIn: signIn, signInAction: actions.onSignIn, cancel: actions.onCancel)
            .frame(maxWidth: 560, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatMcpSignIn,
            value: signIn.accessibilityPhase
        )
    }
}

typealias HostedMcpSignInCard = HostedCard<McpSignInCardPayload>

/// Laid out as the agent sign-in card is: the same arc, from signed out
/// through the browser to signed in, in one card that stays put. Signing in
/// does not repeat the call; the user asks the agent again.
struct ChatMcpSignInCard: View {
    let signIn: ChatMcpSignIn
    let signInAction: () -> Void
    let cancel: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label {
                Text(signIn.title)
            } icon: {
                icon
            }
            .appFont(.body, .bold)
            .foregroundStyle(signIn.isSignedIn ? Color.green : .primary)
            .accessibilityIdentifier(AccessibilityID.chatMcpSignIn)
            .accessibilityValue(signIn.accessibilityPhase)
            Text(signIn.detail)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let failure = signIn.failure {
                Text(failure)
                    .appFont(.callout)
                    .foregroundStyle(.orange)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if signIn.phase == .waiting {
                ProgressView()
                    .progressViewStyle(.linear)
                    .controlSize(.small)
                    .frame(maxWidth: 240)
            }
            controls
        }
        .padding(12)
        .background(tint, in: RoundedRectangle(cornerRadius: 10))
        .animation(.smooth(duration: 0.3), value: signIn.phase)
    }

    @ViewBuilder
    private var icon: some View {
        switch signIn.phase {
        case .waiting:
            Image(systemName: "person.badge.key")
                .symbolEffect(.variableColor.iterative.reversing)
                .symbolEffect(.pulse)
        case .signedIn:
            Image(systemName: "checkmark.seal.fill")
        default:
            Image(systemName: "person.badge.key")
        }
    }

    @ViewBuilder
    private var controls: some View {
        HStack(spacing: 8) {
            switch signIn.action {
            case .signIn:
                Button("Sign In…", action: signInAction)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier(AccessibilityID.chatMcpSignInButton)
            case .tryAgain:
                Button("Try Again", action: signInAction)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier(AccessibilityID.chatMcpSignInButton)
            case .cancel:
                Button("Cancel", action: cancel)
                    .buttonStyle(.bordered)
            case nil:
                EmptyView()
            }
            Spacer()
        }
        .transition(.opacity)
    }

    private var tint: Color {
        signIn.isSignedIn ? .green.opacity(0.12) : .orange.opacity(0.1)
    }
}
