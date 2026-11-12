import OttamAccessibility
import OttamMarkdownEngine
import OttamPresentationLogic
import SwiftUI

/// The sign-in recovery card, hosted. The sign-in controller's per-agent
/// state (failure, terminal login) is flattened by the snapshot.
struct AuthRecoveryCardPayload: HostedCardPayload {
    struct Actions {
        var onSignIn: () -> Void
        var onCancelSignIn: () -> Void
        var onSendAgain: () -> Void
    }

    var recovery: ChatAuthRecovery
    var failure: String?
    var isTerminalLogin: Bool
    var isMutating: Bool

    var signature: AnyHashable {
        "\(recovery.engine.engineID.rawValue)|\(recovery.phase)|\(recovery.engine.authState)|\(recovery.undeliveredInput != nil)|\(failure ?? "")|\(isTerminalLogin)"
    }

    func makeBody(actions: Actions) -> some View {
        ChatAuthSignInCard(
            recovery: recovery,
            failure: failure,
            isMutating: isMutating,
            terminalLogin: isTerminalLogin,
            signIn: actions.onSignIn,
            cancelSignIn: actions.onCancelSignIn,
            sendAgain: actions.onSendAgain
        )
        .frame(maxWidth: 560, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatAuthRecovery,
            value: ChatAuthSignInCard.accessibilityPhase(recovery.phase)
        )
    }
}

typealias HostedAuthRecoveryCard = HostedCard<AuthRecoveryCardPayload>

/// The recovery card on a run that stopped for want of credentials.
///
/// One card for the whole arc — sign in, wait, send again — because they are one
/// task to the person looking at it. It stays put across its own phases rather
/// than swapping between cards, so the thing the user is reading never jumps.
///
/// Deliberately one sign-in button: whether the login runs in a browser the
/// adapter opens or a terminal Ottam owns is core's business, and the two
/// look the same from here until something goes wrong.
struct ChatAuthSignInCard: View {
    let recovery: ChatAuthRecovery
    let failure: String?
    let isMutating: Bool
    /// The login runs in a terminal window Ottam shows, not a browser.
    let terminalLogin: Bool
    let signIn: () -> Void
    let cancelSignIn: () -> Void
    let sendAgain: () -> Void

    private var presentation: ChatAuthRecoveryPresentation {
        ChatAuthRecoveryPresentation(
            phase: recovery.phase,
            authState: recovery.engine.authState,
            hasUndeliveredInput: recovery.undeliveredInput != nil,
            signInFailure: failure
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label {
                Text(title)
            } icon: {
                icon
            }
            .appFont(.body, .bold)
            .foregroundStyle(recovery.phase == .signedIn ? Color.green : .primary)
            .accessibilityIdentifier(AccessibilityID.chatAuthRecovery)
            .accessibilityValue(Self.accessibilityPhase(recovery.phase))
            Text(explanation)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            // A failure from the login itself, distinct from the run's own
            // error: the sign-in was tried and did not take.
            if let visibleFailureMessage = presentation.visibleFailureMessage {
                Text(visibleFailureMessage)
                    .appFont(.callout)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if recovery.phase == .signingIn {
                ProgressView()
                    .progressViewStyle(.linear)
                    .controlSize(.small)
                    .frame(maxWidth: 240)
            }
            controls
        }
        .padding(12)
        .background(tint, in: RoundedRectangle(cornerRadius: 10))
        .animation(.smooth(duration: 0.3), value: recovery.phase)
    }

    @ViewBuilder
    private var icon: some View {
        switch recovery.phase {
        case .signedOut:
            Image(systemName: "person.badge.key")
        case .signingIn:
            // The glyph keeps moving for as long as the browser owes us an
            // answer; a static card reads as a stalled one.
            Image(systemName: "person.badge.key")
                .symbolEffect(.variableColor.iterative.reversing)
                .symbolEffect(.pulse)
        case .signedIn:
            Image(systemName: "checkmark.seal.fill")
        case .ready:
            Image(systemName: "arrow.trianglehead.counterclockwise")
        }
    }

    @ViewBuilder
    private var controls: some View {
        HStack(spacing: 8) {
            switch presentation.primaryAction {
            case .signIn:
                Button(ChatAuthRecoveryAction.signIn.title, action: signIn)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier(AccessibilityID.chatAuthSignIn)
            case .signInAgain:
                Button(ChatAuthRecoveryAction.signInAgain.title, action: signIn)
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier(AccessibilityID.chatAuthSignIn)
            case .cancelSignIn:
                Text(
                    terminalLogin
                        ? "Finish the login in the terminal window…"
                        : "Finish the login in your browser…"
                )
                .appFont(.callout)
                .foregroundStyle(.secondary)
                Button(ChatAuthRecoveryAction.cancelSignIn.title, action: cancelSignIn)
                    .buttonStyle(.bordered)
            case .sendAgain:
                Button(ChatAuthRecoveryAction.sendAgain.title, action: sendAgain)
                    .buttonStyle(.borderedProminent)
                    .disabled(isMutating)
                    .accessibilityIdentifier(AccessibilityID.chatAuthSendAgain)
            case nil:
                EmptyView()
            }
            Spacer()
        }
        .transition(.opacity)
    }

    private var tint: Color {
        recovery.phase == .signedIn ? .green.opacity(0.12) : .orange.opacity(0.1)
    }

    static func accessibilityPhase(_ phase: ChatAuthRecovery.Phase) -> String {
        switch phase {
        case .signedOut: "signed out"
        case .signingIn: "signing in"
        case .signedIn: "signed in"
        case .ready: "ready"
        }
    }

    private var title: String {
        presentation.title(engineDisplayName: recovery.engine.displayName)
    }

    private var explanation: String {
        presentation.detail(engineDisplayName: recovery.engine.displayName)
    }
}

