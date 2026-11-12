import OttamBackendClient
import SwiftUI

/// The revealed half of a sign-in. For a terminal-first login, core
/// reveals this from the first frame — the terminal IS the sign-in; for a
/// browser-first login it appears only when the command exits badly or goes
/// quiet long enough to look stuck. Either way the flow is still live, so
/// this is a working terminal, not a transcript.
struct AgentSignInTerminalSheet: View {
    let revealed: AgentSignInController.RevealedTerminal
    let signIn: AgentSignInController
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Sign in to \(revealed.engineName)")
                        .font(.headline)
                    Text("When you've finished signing in, press Done to restart the agent.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Stop") {
                    signIn.cancel(revealed.agentID)
                    dismiss()
                }
                .buttonStyle(.bordered)
                Button("Done") {
                    signIn.finish(revealed.agentID)
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
            .padding(16)
            Divider()
            TerminalWorkspaceView(
                surfaceController: revealed.surfaceController,
                isActive: true
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        // Presented from the detail column, so the sheet would otherwise inherit
        // its content font. The header is chrome; the terminal below draws its
        // own font either way.
        .systemTypographyRoot()
        .frame(minWidth: 720, minHeight: 460)
    }
}
