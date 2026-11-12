import OttamBackendClient
import OttamPresentationLogic
import Foundation
import Observation

/// The MCP Servers pane's state, owned by the pane like Storage's: the list
/// is cheap to read and has no other reader, so it dies with the pane.
@MainActor
@Observable
final class McpServersSettingsModel {
    private let client: OttamCoreProcessClient
    private(set) var servers: [McpServerSummary] = []
    /// Where sign-ins return, for registering Ottam with a server by hand.
    private(set) var redirectURI: String?
    private(set) var didLoadOnce = false
    private(set) var errorText: String?
    /// The latest list request; an earlier one's answer is out of date.
    @ObservationIgnored private var loadGeneration = 0
    /// Re-reads asked for while one is under way run as one, after it.
    @ObservationIgnored private var reloading: Task<Void, Never>?
    @ObservationIgnored private var reloadAgain = false

    init(client: OttamCoreProcessClient) {
        self.client = client
    }

    func server(_ serverID: McpServerID) -> McpServerSummary? {
        servers.first { $0.serverID == serverID }
    }

    func load() async {
        loadGeneration += 1
        let generation = loadGeneration
        do {
            let listed = try await client.listMcpServers()
            guard !Task.isCancelled, generation == loadGeneration else { return }
            servers = listed.servers
            redirectURI = listed.redirectURI
            errorText = nil
            didLoadOnce = true
        } catch {
            guard !Task.isCancelled, generation == loadGeneration else { return }
            errorText = error.localizedDescription
        }
    }

    /// Read the list again, folding requests that come while one runs into
    /// a single read after it: a burst of changes reads it twice at most.
    private func reload() {
        guard reloading == nil else {
            reloadAgain = true
            return
        }
        reloading = Task { [weak self] in
            while let self, !Task.isCancelled {
                self.reloadAgain = false
                await self.load()
                guard self.reloadAgain else { break }
            }
            self?.reloading = nil
        }
    }

    private enum Change {
        case state(McpServerStateChangedParams)
        /// The list or a server's sign-in changed: read it again.
        case reload
    }

    /// Scoped to the pane's SwiftUI task, so the subscription ends with it.
    /// A state arriving before the list is dropped; the list carries it.
    /// Another client's changes, and finished sign-ins, read the list again.
    func observeStateChanges() async {
        let (events, continuation) = AsyncStream<Change>.makeStream()
        let token = await client.subscribeToEvents(
            filter: AgentEventFilter(methods: [
                "mcp.serverStateChanged", "mcp.serversChanged", "mcp.signInFinished",
            ])
        ) { notification in
            if notification.method == "mcp.serverStateChanged" {
                if let params = try? JSONDecoder().decode(
                    ProtocolNotification<McpServerStateChangedParams>.self,
                    from: notification.payload
                ).params {
                    continuation.yield(.state(params))
                }
            } else {
                continuation.yield(.reload)
            }
        }
        for await change in events {
            switch change {
            case .state(let change):
                if let index = servers.firstIndex(where: { $0.serverID == change.serverID }) {
                    servers[index].state = change.state
                }
            case .reload:
                reload()
            }
        }
        reloading?.cancel()
        reloading = nil
        await client.unsubscribeFromEvents(token)
    }

    func add(_ config: McpServerConfig) async throws -> McpServerSummary {
        let server = try await client.addMcpServer(config)
        show(server)
        return server
    }

    func importCandidates(_ source: McpImportSource) async throws -> McpImportSelection {
        McpImportSelection(try await client.importMcpServerCandidates(source: source))
    }

    func update(_ serverID: McpServerID, config: McpServerConfig) async throws {
        show(try await client.updateMcpServer(serverID: serverID, config: config))
    }

    func remove(_ serverID: McpServerID) async throws {
        try await client.removeMcpServer(serverID: serverID)
        servers.removeAll { $0.serverID == serverID }
    }

    func signedOut(_ server: McpServerSummary) {
        show(server)
    }

    func setEnabled(_ serverID: McpServerID, workspaceID: WorkspaceID, enabled: Bool) async throws {
        show(try await client.setMcpServerEnabled(serverID: serverID, workspaceID: workspaceID, enabled: enabled))
    }

    /// A reply's summary in place of the listed one, or added at the end.
    private func show(_ server: McpServerSummary) {
        if let index = servers.firstIndex(where: { $0.serverID == server.serverID }) {
            servers[index] = server
        } else {
            servers.append(server)
        }
    }
}
