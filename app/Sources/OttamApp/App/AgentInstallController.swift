import OttamBackendClient
import OttamWorkspaceLogic
import Foundation
import Observation
import OSLog

/// Drives `agent.install` — the counterpart of `RuntimeInstallController`, but
/// keyed by agent because each agent installs its own adapter independently.
/// Fires the install, follows `agent.installEvent`, and keeps one
/// `AgentAdapterInstall` per agent for the Adapter section to render.
@MainActor
@Observable
final class AgentInstallController {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "AgentInstall"
    )

    private let client: OttamCoreProcessClient
    private let decoder = JSONDecoder()
    private var subscription: AgentEventSubscriptionToken?
    private var installIDs: [AgentID: AgentInstallID] = [:]

    /// Per-agent install flow as last reported by core.
    private(set) var states: [AgentID: AgentAdapterInstall] = [:]

    /// Called when an agent's adapter lands, so Settings can re-read the agent
    /// list — the install state it is showing is now out of date.
    var onInstalled: ((AgentID) -> Void)?

    init(client: OttamCoreProcessClient) {
        self.client = client
    }

    func state(for agentID: AgentID) -> AgentAdapterInstall {
        states[agentID] ?? AgentAdapterInstall()
    }

    /// Subscribe once, for the app's lifetime. Idempotent so every surface that
    /// might show install state can call it on appear without coordinating.
    func start() async {
        guard subscription == nil else { return }
        subscription = await client.subscribeToEvents(
            filter: AgentEventFilter(methods: ["agent.installEvent"])
        ) { [weak self] notification in
            Task { @MainActor in
                self?.handle(notification)
            }
        }
    }

    /// Begin (or join) the agent's adapter install.
    func install(agentID: AgentID) {
        guard !state(for: agentID).isInFlight else { return }
        states[agentID] = AgentAdapterInstall(phase: .starting)
        Task { [weak self] in
            guard let self else { return }
            await self.start()
            do {
                let accepted = try await self.client.installAgentAdapter(agentID: agentID)
                self.installIDs[agentID] = accepted.installID
            } catch {
                self.states[agentID] = AgentAdapterInstall(
                    phase: .failed,
                    error: Self.message(for: error)
                )
                Self.logger.error(
                    "adapter install failed to start: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
    }

    /// Ask core to abandon the agent's in-flight install, if any.
    func cancel(agentID: AgentID) {
        guard let installID = installIDs[agentID] else {
            states[agentID] = nil
            return
        }
        Task { [client] in
            try? await client.cancelAgentAdapterInstall(installID: installID)
        }
    }

    /// Forget a finished install so the steady `agent.list` state shows again —
    /// *Installed* is news about something the user just did, not a status.
    func forgetOutcome(agentID: AgentID) {
        guard !state(for: agentID).isInFlight else { return }
        states[agentID] = nil
        installIDs[agentID] = nil
    }

    private func handle(_ notification: AgentNotification) {
        let event: AgentInstallEventParams
        do {
            event = try decoder.decode(
                ProtocolNotification<AgentInstallEventParams>.self,
                from: notification.payload
            ).params
        } catch {
            Self.logger.error(
                "failed to decode adapter install event: \(error.localizedDescription, privacy: .private)"
            )
            return
        }
        installIDs[event.agentID] = event.installID
        states[event.agentID] = AgentAdapterInstall(phase: event.phase, error: event.error)
        if event.phase == .ready {
            onInstalled?(event.agentID)
        }
    }

    private static func message(for error: Error) -> String {
        if case .rpcError(_, let message, _) = error as? AgentConnectionError {
            return message
        }
        return error.localizedDescription
    }
}
