import AppKit
import OttamBackendClient
import Observation
import SwiftUI

@MainActor
@Observable
final class AgentShutdownPreparation {
    private(set) var waiting = false

    func monitor(client: OttamCoreProcessClient) async {
        defer { waiting = false }
        while !Task.isCancelled {
            if let readiness = try? await client.agentShutdownReadiness(), !Task.isCancelled { waiting = !readiness.ready }
            try? await Task.sleep(for: .milliseconds(100))
        }
    }

    func wait(client: OttamCoreProcessClient) async throws {
        defer { waiting = false }
        while true {
            try Task.checkCancellation()
            let readiness = try await client.agentShutdownReadiness()
            try Task.checkCancellation()
            if readiness.ready { return }
            waiting = true
            try await Task.sleep(for: .milliseconds(100))
        }
    }
}

/// What quitting asks while agents are working: mid-turn, or on a
/// background task or subagent that outlived its turn. Quitting stops it.
struct WorkingAgentQuitWarning: Equatable {
    let message: String
    let detail: String

    /// Nil when nothing is working, or when a signal asked to quit: nobody
    /// is there to answer.
    init?(workingConversations count: Int, signalled: Bool) {
        guard count > 0, !signalled else { return nil }
        if count == 1 {
            message = "Quit Ottam while an agent is working?"
            detail = "Quitting stops its work. You can continue the chat later."
        } else {
            message = "Quit Ottam while \(count) agents are working?"
            detail = "Quitting stops their work. You can continue the chats later."
        }
    }

    /// Cancel is the default button. NSAlert binds a first button titled
    /// Cancel to Escape rather than Return, so Return is set explicitly.
    @MainActor
    func makeAlert() -> NSAlert {
        let alert = NSAlert()
        alert.messageText = message
        alert.informativeText = detail
        alert.addButton(withTitle: "Cancel").keyEquivalent = "\r"
        alert.addButton(withTitle: "Quit").hasDestructiveAction = true
        return alert
    }
}

struct AgentShutdownWaitingView: View {
    var refreshing = true
    let cancel: @MainActor () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                ProgressView().controlSize(.small)
                Text(refreshing ? "Waiting for Claude to finish refreshing sign-in…" : "Preparing to quit Ottam…")
            }
            Text(refreshing ? "Ottam will keep the agent running until the refresh finishes." : "Ottam is waiting for its agents to stop safely.")
                .foregroundStyle(.secondary)
            HStack { Spacer(); Button("Cancel", action: cancel).keyboardShortcut(.cancelAction) }
        }.padding(24).frame(width: 440)
    }
}
