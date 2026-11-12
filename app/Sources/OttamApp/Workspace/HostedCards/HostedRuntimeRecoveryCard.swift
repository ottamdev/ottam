import OttamAccessibility
import OttamMarkdownEngine
import OttamWorkspaceLogic
import SwiftUI

/// The runtime recovery card, hosted. The process-wide install controller is
/// flattened to its `ManagedRuntimeInstall` value by the snapshot, so a
/// progress tick is a payload change like any other.
struct RuntimeRecoveryCardPayload: HostedCardPayload {
    struct Actions {
        var onInstall: () -> Void
        var onCheckAgain: () -> Void
    }

    var recovery: ChatRuntimeRecovery
    /// Nil when no install controller is reachable: Check Again alone.
    var install: ManagedRuntimeInstall?
    var isChecking: Bool

    var signature: AnyHashable {
        "\(recovery.problem.title)|\(recovery.problem.explanation)|\(recovery.detail ?? "")|\(install?.detail ?? "")|\(install != nil)|\(install?.fractionCompleted != nil)|\(install?.isInFlight ?? false)"
    }

    func makeBody(actions: Actions) -> some View {
        ChatRuntimeSetupCard(
            recovery: recovery,
            isChecking: isChecking,
            install: install,
            installAction: actions.onInstall,
            checkAgain: actions.onCheckAgain
        )
        .frame(maxWidth: 560, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(identifier: AccessibilityID.chatRuntimeRecovery, value: nil)
    }
}

typealias HostedRuntimeRecoveryCard = HostedCard<RuntimeRecoveryCardPayload>

/// The runtime counterpart of `ChatAuthSignInCard`: a run that failed because
/// Node is missing, broken, or too old, with the fix and a way to re-check.
///
/// Install is the prominent action and lives right here — a card that sent the
/// user to a browser was a dead end, since nothing about a manual install
/// brought them back. Check Again stays for the user who fixed it themselves.
struct ChatRuntimeSetupCard: View {
    let recovery: ChatRuntimeRecovery
    let isChecking: Bool
    let install: ManagedRuntimeInstall?
    let installAction: () -> Void
    let checkAgain: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label {
                Text(recovery.problem.title)
            } icon: {
                Image(systemName: "shippingbox.and.arrow.backward")
            }
            .appFont(.body, .bold)
            .accessibilityIdentifier(AccessibilityID.chatRuntimeRecovery)
            Text(recovery.problem.explanation)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            // The wire message names the agent and component exactly; the
            // explanation above generalizes, so both earn their place.
            if let detail = recovery.detail {
                Text(detail)
                    .appFont(.callout)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let installDetail = install?.detail {
                Text(installDetail)
                    .appFont(.callout)
                    .foregroundStyle(install?.phase == .failed ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                if let install {
                    Button(install.actionTitle, action: installAction)
                        .buttonStyle(.borderedProminent)
                        .disabled(install.isInFlight || install.phase == .ready)
                    if let fraction = install.fractionCompleted {
                        ProgressView(value: fraction)
                            .controlSize(.small)
                            .frame(width: 90)
                    } else if install.isInFlight {
                        ProgressView()
                            .controlSize(.small)
                    }
                }
                Button("Check Again", action: checkAgain)
                    .buttonStyle(.bordered)
                    .disabled(isChecking)
                    .accessibilityIdentifier(AccessibilityID.chatRuntimeCheckAgain)
                Spacer()
            }
        }
        .padding(12)
        .background(Color.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
    }
}
