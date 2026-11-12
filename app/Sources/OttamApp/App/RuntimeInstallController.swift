import OttamBackendClient
import OttamWorkspaceLogic
import Foundation
import Observation
import OSLog

/// Drives `runtime.install` for the whole app.
///
/// Process-wide rather than per-window, because the runtime is: there is one
/// `~/.ottam/runtime/node`, one install at a time, and three surfaces that
/// offer it — the Agents pane, the engine picker, and the runtime card in chat.
/// A per-window controller would let one of them show *Install* while another
/// showed *Downloading… 60%* for the same work.
@MainActor
@Observable
final class RuntimeInstallController {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "RuntimeInstall"
    )

    private let client: OttamCoreProcessClient
    private let decoder = JSONDecoder()
    private var subscription: AgentEventSubscriptionToken?
    private var installID: RuntimeInstallID?

    private(set) var state = ManagedRuntimeInstall()

    /// Called when a runtime lands, so callers can re-read the engine or agent
    /// list — the runtime status they are showing is now out of date. Core has
    /// already invalidated its own caches by this point, so a plain reload is
    /// enough; no explicit refresh is needed.
    var onInstalled: (() -> Void)?

    init(client: OttamCoreProcessClient) {
        self.client = client
    }

    /// Subscribe once, for the app's lifetime. Idempotent so every surface that
    /// might show install state can call it on appear without coordinating.
    func start() async {
        guard subscription == nil else { return }
        subscription = await client.subscribeToEvents(
            filter: AgentEventFilter(methods: ["runtime.installEvent"])
        ) { [weak self] notification in
            Task { @MainActor in
                self?.handle(notification)
            }
        }
    }

    func install() {
        guard !state.isInFlight else { return }
        state = ManagedRuntimeInstall(phase: .starting)
        Task { [weak self] in
            guard let self else { return }
            await self.start()
            do {
                let accepted = try await self.client.installRuntime()
                // Events can land before the response does, so this only fills
                // in what the response is authoritative about and never walks a
                // phase backwards.
                self.installID = accepted.installID
                if self.state.version == nil {
                    self.state.version = accepted.version
                }
            } catch {
                self.state = ManagedRuntimeInstall(
                    phase: .failed,
                    error: Self.message(for: error)
                )
                Self.logger.error(
                    "runtime install failed to start: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
    }

    func cancel() {
        guard let installID else {
            state = ManagedRuntimeInstall()
            return
        }
        Task { [client] in
            try? await client.cancelRuntimeInstall(installID: installID)
        }
    }

    /// Forget a finished install so a surface reopening later starts clean —
    /// *Installed* is news about something the user just did, not a status.
    func forgetOutcome() {
        guard !state.isInFlight else { return }
        state = ManagedRuntimeInstall()
    }

    private func handle(_ notification: AgentNotification) {
        let event: RuntimeInstallEventParams
        do {
            event = try decoder.decode(
                ProtocolNotification<RuntimeInstallEventParams>.self,
                from: notification.payload
            ).params
        } catch {
            Self.logger.error(
                "failed to decode runtime install event: \(error.localizedDescription, privacy: .private)"
            )
            return
        }
        // A late event from an install this controller already replaced would
        // otherwise resurrect a finished flow.
        if let installID, installID != event.installID, state.isInFlight {
            return
        }
        installID = event.installID
        state = ManagedRuntimeInstall(
            phase: event.phase,
            version: event.version,
            bytesReceived: event.bytesReceived,
            bytesTotal: event.bytesTotal,
            error: event.error
        )
        if event.phase == .ready {
            onInstalled?()
        }
    }

    private static func message(for error: Error) -> String {
        if case .rpcError(_, let message, _) = error as? AgentConnectionError {
            return message
        }
        return error.localizedDescription
    }
}
