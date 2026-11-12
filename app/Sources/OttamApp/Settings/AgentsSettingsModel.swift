import OttamBackendClient
import OttamWorkspaceLogic
import Observation
import Foundation

/// The Agents pane's window-lifetime state. Owned as `@State` by the Settings
/// window, whose sidebar search also reads it, rather than parked on
/// `OttamAppRuntime`: the runtime lives for the process, and pane state
/// (loading, error, last answer) should die with the Settings window instead
/// of probing from views that never asked.
@MainActor
@Observable
final class AgentsSettingsModel {
    private let client: OttamCoreProcessClient
    private(set) var agents: [AgentSummary] = []
    private(set) var detectedSetups: [AgentDetectedSetup] = []
    var activeProfileID: AgentID?
    let removalPreparation = AgentShutdownPreparation()
    let installPreparation = AgentShutdownPreparation()

    func monitorInstallRefresh() async {
        await installPreparation.monitor(client: client)
    }

    func detectSetups() async throws {
        detectedSetups = try await client.detectAgentSetups().setups
    }

    func addExistingSetup(harnessID: String) async throws -> AgentID {
        let result = try await client.createAgent(harnessID: harnessID, layout: .existing)
        _ = try? await client.agentAuthState(agentID: result.agentID, refresh: true)
        await load()
        return result.agentID
    }

    func createProfile(harnessID: String, label: String) async throws -> AgentID {
        let result = try await client.createAgent(harnessID: harnessID, layout: .managed, label: label)
        await load()
        return result.agentID
    }

    func renameProfile(agentID: AgentID, label: String) async throws {
        _ = try await client.renameAgent(agentID: agentID, label: label)
        await load()
    }

    func probeProfile(agentID: AgentID) async {
        _ = try? await client.agentAuthState(agentID: agentID, refresh: true)
        await load()
    }

    func deleteProfile(agentID: AgentID, purge: Bool) async throws {
        let operationKey = UUID().uuidString
        let monitor = Task { await removalPreparation.monitor(client: client) }
        defer { monitor.cancel() }
        try await withTaskCancellationHandler {
            try await client.deleteAgent(agentID: agentID, purge: purge, operationKey: operationKey)
        } onCancel: { [client] in
            Task { try? await client.cancelAgentShutdown(operationKey: operationKey) }
        }
        await load()
    }

    func useLayout(agentID: AgentID, layout: AgentHomeKind) async throws -> AgentID {
        let result = try await client.setAgentLayout(agentID: agentID, layout: layout)
        if layout == .existing {
            _ = try? await client.agentAuthState(agentID: result.agentID, refresh: true)
        }
        await load()
        return result.agentID
    }
    /// Loads overlap — the pane, sidebar search, and removal each start one —
    /// so loading lasts until the last of them finishes.
    var isLoading: Bool { loadsInFlight > 0 }
    private var loadsInFlight = 0
    /// Issue order of loads, and the newest one whose answer is on screen: a
    /// reply older than what is shown never replaces it.
    @ObservationIgnored private var loadSequence = 0
    @ObservationIgnored private var shownSequence = 0
    @ObservationIgnored private var searchLoad: Task<Void, Never>?
    private(set) var didLoadOnce = false
    private(set) var errorText: String?
    /// Bumped by Check Again; keyed into the pane's `.task(id:)` so a manual
    /// refresh cancels an in-flight load instead of racing it.
    private(set) var refreshGeneration = 0
    /// Whether the pending load should invalidate the resolver cache. Only
    /// Check Again sets it: an appear-triggered load serves the cache.
    private(set) var wantsRuntimeRefresh = false
    private var reloadPending = false

    init(client: OttamCoreProcessClient) {
        self.client = client
    }

    func requestRuntimeRefresh() {
        wantsRuntimeRefresh = true
        refreshGeneration += 1
    }

    func requestActivationRefresh() {
        if let id = activeProfileID { Task { await probeProfile(agentID: id) } }
        requestReload()
    }

    /// A soft reload: serves the resolver's caches, which expire on their own
    /// for the unresolved case. Used on app re-activation.
    func requestReload() {
        if isLoading {
            reloadPending = true
            return
        }
        guard didLoadOnce else { return }
        refreshGeneration += 1
    }

    /// Scoped to the pane's SwiftUI task. AsyncStream ends on cancellation,
    /// allowing the subscription to be removed when the pane disappears.
    func observeEngineChanges() async {
        let (events, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let token = await client.subscribeToEvents(
            filter: AgentEventFilter(methods: ["engine.changed"])
        ) { _ in
            continuation.yield(())
        }
        for await _ in events {
            requestReload()
        }
        await client.unsubscribeFromEvents(token)
    }

    /// The one Node the whole app resolved to. Every adapter runs on the same
    /// toolchain, so the first agent that reports a version speaks for all of
    /// them; a disagreement would mean core resolved twice, which it does not.
    var nodeVersion: String? {
        agents.lazy.compactMap(\.runtime.nodeVersion).first
    }

    var runtimeSource: AgentRuntimeSource? {
        agents.lazy.compactMap(\.runtime.source).first
    }

    /// An app update replacing the managed runtime the agents run on, while
    /// they keep running on the older one.
    var managedRuntimeUpdate: AgentUpdateStatus? {
        agents.lazy.compactMap(\.runtime.managedUpdate).first
    }

    /// Whether any agent is held back by the runtime rather than by sign-in or
    /// a crashed adapter — the only condition the managed install can fix.
    var hasRuntimeProblem: Bool {
        agents.contains { EngineRuntimeProblem(agent: $0) != nil }
    }

    /// Home path + rough size per agent, cached for the window's lifetime so
    /// a revisited detail page shows the last answer immediately while the
    /// fresh walk runs. Errors leave the cache untouched: an agent with no
    /// managed home (the test agent) simply never gets an entry, and the
    /// detail page hides its Home section.
    private(set) var homeInfo: [AgentID: AgentHomeInfoResult] = [:]

    func loadHomeInfo(agentID: AgentID) async {
        guard let info = try? await client.agentHomeInfo(agentID: agentID) else { return }
        guard !Task.isCancelled else { return }
        homeInfo[agentID] = info
    }

    /// Remove the agent's adapter (and with `removeHome`, its sessions and
    /// sign-in). Reloads the list and the home figures afterwards so the page
    /// shows *Get* and a fresh size without waiting for the next appear.
    func remove(agentID: AgentID, removeHome: Bool) async throws {
        try await removalPreparation.wait(client: client)
        try Task.checkCancellation()
        let operationKey = UUID().uuidString
        let monitor = Task { await removalPreparation.monitor(client: client) }
        defer { monitor.cancel() }
        try await withTaskCancellationHandler {
            try await client.uninstallAgentAdapter(agentID: agentID, removeHome: removeHome, operationKey: operationKey)
        } onCancel: { [client] in
            Task { try? await client.cancelAgentShutdown(operationKey: operationKey) }
        }
        await load()
        await loadHomeInfo(agentID: agentID)
    }

    /// The sidebar search's one load. Unstructured, so clearing the query or
    /// leaving the Agents pane does not cancel it; skipped once a list has
    /// arrived or while any load runs, so a failed attempt retries on the
    /// next keystroke rather than never.
    func loadForSearch() {
        guard !didLoadOnce, !isLoading, searchLoad == nil else { return }
        searchLoad = Task {
            await load()
            searchLoad = nil
        }
    }

    func load() async {
        let refreshRuntime = wantsRuntimeRefresh
        wantsRuntimeRefresh = false
        loadSequence += 1
        let sequence = loadSequence
        loadsInFlight += 1
        defer {
            loadsInFlight -= 1
            if loadsInFlight == 0, reloadPending {
                reloadPending = false
                refreshGeneration += 1
            }
        }
        do {
            let listed = try await client.listAgents(refreshRuntime: refreshRuntime)
            guard !Task.isCancelled, sequence > shownSequence else { return }
            shownSequence = sequence
            agents = listed.agents
            errorText = nil
            didLoadOnce = true
        } catch {
            guard !Task.isCancelled, sequence > shownSequence else { return }
            shownSequence = sequence
            errorText = error.localizedDescription
        }
    }
}
