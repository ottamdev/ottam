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
