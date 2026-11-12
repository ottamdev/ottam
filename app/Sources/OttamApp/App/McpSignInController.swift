import AppKit
import OttamBackendClient
import OttamPresentationLogic
import Foundation
import Observation
import OSLog

/// Drives remote MCP servers' sign-ins for the whole app.
///
/// Process-wide, like core's one waiting sign-in: Settings and every chat's
/// Sign in card read the same phase, so a sign-in started in one shows as
/// waiting in the other. Core opens nothing itself; this opens the
/// authorization URL in the default browser.
@MainActor
@Observable
final class McpSignInController {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "McpSignIn"
    )

    private let client: OttamCoreProcessClient
    private let decoder = JSONDecoder()
    @ObservationIgnored private var subscription: AgentEventSubscriptionToken?

    private(set) var state = McpSignInState()

    init(client: OttamCoreProcessClient) {
        self.client = client
    }

    /// Subscribe once, for the app's lifetime.
    func start() async {
        guard subscription == nil else { return }
        subscription = await client.subscribeToEvents(
            filter: AgentEventFilter(methods: ["mcp.signInFinished", "mcp.signInNeeded"])
        ) { [weak self] notification in
            Task { @MainActor in
                self?.handle(notification)
            }
        }
    }

    /// Start signing in to `serverID`, replacing any sign-in still waiting,
    /// and open its page in the browser.
    func signIn(_ serverID: McpServerID, clientID: String? = nil) {
        state.started(serverID)
        Task { [client] in
            do {
                let started = try await client.signInMcpServer(serverID: serverID, clientID: clientID)
                // Cancelled or replaced while core found the server's sign-in.
                guard state.opensBrowser(serverID) else { return }
                guard let url = McpSignInPage.url(started.authorizationURL) else {
                    state.startFailed(serverID, message: "The server's sign-in page is not a web page Ottam can open.")
                    return
                }
                NSWorkspace.shared.open(url)
            } catch AgentConnectionError.rpcError(_, let message, let data?)
                where Self.kind(of: data) == "mcpClientIDRequired" {
                state.needsClientID(serverID, message: message)
            } catch AgentConnectionError.rpcError(_, _, let data?)
                where Self.kind(of: data) == "mcpSignInCancelled" {
                // `mcp.signInFinished` reports it.
            } catch {
                state.startFailed(serverID, message: error.localizedDescription)
                Self.logger.error(
                    "MCP sign-in failed to start: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
    }

    func cancel(_ serverID: McpServerID) {
        state.cancelRequested(serverID)
        Task { [client] in
            try? await client.cancelMcpSignIn(serverID: serverID)
        }
    }

    func signOut(_ serverID: McpServerID) async throws -> McpServerSummary {
        let server = try await client.signOutMcpServer(serverID: serverID)
        state.forget(serverID)
        return server
    }

    func forget(_ serverID: McpServerID) {
        state.forget(serverID)
    }

    /// The user wrote again in the chat, so its Sign in cards go; returns
    /// them for `restoreNeeds`.
    func clearNeeds(_ conversationID: ConversationID) -> [McpSignInState.Need] {
        state.clearNeeds(conversationID)
    }

    /// The message did not go through: its chat's cards come back.
    func restoreNeeds(_ needs: [McpSignInState.Need], for conversationID: ConversationID) {
        state.restoreNeeds(needs, for: conversationID)
    }

    private static func kind(of data: JSONValue) -> String? {
        guard case .object(let values) = data, case .string(let kind)? = values["kind"] else { return nil }
        return kind
    }

    private func handle(_ notification: AgentNotification) {
        do {
            switch notification.method {
            case "mcp.signInFinished":
                state.finished(try decoder.decode(
                    ProtocolNotification<McpSignInFinishedParams>.self, from: notification.payload
                ).params)
            case "mcp.signInNeeded":
                state.needed(try decoder.decode(
                    ProtocolNotification<McpSignInNeededParams>.self, from: notification.payload
                ).params)
            default:
                break
            }
        } catch {
            Self.logger.error(
                "failed to decode an MCP sign-in event: \(error.localizedDescription, privacy: .private)"
            )
        }
    }
}
