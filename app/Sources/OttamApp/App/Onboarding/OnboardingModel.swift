import Foundation
import Observation
import OttamBackendClient
import OttamPresentationLogic
import OttamWorkspaceLogic

/// First-launch onboarding's state: which agents the user chose, and driving
/// their layout, install, sign-in, and the first named agent. Install and
/// sign-in state stay with the process-wide controllers and `agent.list`;
/// this only decides what to start next.
@MainActor
@Observable
final class OnboardingModel {
    let runtime: OttamAppRuntime
    let agents: AgentsSettingsModel
    let namedAgents: NamedAgentsSettingsModel
    let signIn = AgentSignInController()

    private(set) var step: OnboardingStep = .intro
    /// Harness IDs in the order the picker lists them.
    private(set) var selected: [String]
    private(set) var cliSetup: Set<String>
    private(set) var errorText: String?
    private(set) var isPreparing = false
    /// Agents whose sign-in recheck is in flight, so their row can show a
    /// spinner while it runs and Check Again once it settles.
    private(set) var rechecking: Set<AgentID> = []
    @ObservationIgnored private var autoStarted: Set<String> = []
    @ObservationIgnored private var nodeAutoStarted = false
    @ObservationIgnored private var probed: Set<AgentID> = []
    /// While `start()` runs, core becoming ready is its own doing, not a
    /// reconnect.
    @ObservationIgnored private var isStarting = false

    init(runtime: OttamAppRuntime) {
        self.runtime = runtime
        agents = AgentsSettingsModel(client: runtime.client)
        namedAgents = NamedAgentsSettingsModel(client: runtime.client)
        let record = runtime.onboardingStore.load()
        selected = record?.selectedHarnessIDs ?? []
        cliSetup = Set(record?.cliSetupHarnessIDs ?? [])
    }

    static let providers = CLIProvider.allCases

    // MARK: Startup

    func start() async {
        isStarting = true
        defer { isStarting = false }
        await runtime.start()
        await runtime.runtimeInstall.start()
        await runtime.agentInstall.start()
        await signIn.configure(workspaceID: nil, backend: runtime.client)
        signIn.onSignedIn = { [weak self] _ in self?.agents.requestReload() }
        await agents.load()
        do { try await agents.detectSetups() } catch { errorText = error.localizedDescription }
    }

    /// After a core restart: re-read what the failed start never got. An
    /// Install step that ran without the agent list sets layouts again.
    func coreBecameReady() async {
        guard !isStarting else { return }
        errorText = nil
        await agents.load()
        do { try await agents.detectSetups() } catch { errorText = error.localizedDescription }
        if step == .install {
            await prepareInstall()
        } else {
            drive()
        }
    }

    /// Leaves the intro for wherever a resumed onboarding should pick up.
    func finishIntro() async {
        while !agents.didLoadOnce, agents.errorText == nil, !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(100))
        }
        let resume = OnboardingProgress.resumeStep(record: record, agents: agents.agents)
        enter(resume)
    }

    // MARK: Choose

    func isSelected(_ harnessID: String) -> Bool { selected.contains(harnessID) }

    func setSelected(_ harnessID: String, _ isOn: Bool) {
        if isOn, !selected.contains(harnessID) {
            selected = Self.providers.map(\.rawValue).filter { $0 == harnessID || selected.contains($0) }
        } else if !isOn {
            selected.removeAll { $0 == harnessID }
            cliSetup.remove(harnessID)
        }
    }

    func usesCLISetup(_ harnessID: String) -> Bool { cliSetup.contains(harnessID) }

    func setUsesCLISetup(_ harnessID: String, _ isOn: Bool) {
        if isOn { cliSetup.insert(harnessID) } else { cliSetup.remove(harnessID) }
    }

    /// The harness's icon key from any of its instances, since the one
    /// onboarding will set up may not exist yet.
    func brand(_ harnessID: String) -> String? {
        agents.agents.first { ($0.harnessID ?? $0.agentID.rawValue) == harnessID }?.brand
    }

    func detected(_ harnessID: String) -> AgentDetectedSetup? {
        agents.detectedSetups.first { $0.harnessID == harnessID && $0.available }
    }

    var canContinueFromChoose: Bool { !selected.isEmpty && agents.didLoadOnce && !isPreparing }

    func continueFromChoose() {
        save()
        enter(.install)
    }

    // MARK: Install

    func instance(_ harnessID: String) -> AgentSummary? {
        OnboardingProgress.instance(harnessID: harnessID, usesCLISetup: cliSetup.contains(harnessID), agents: agents.agents)
    }

    var chosenInstances: [AgentSummary] { selected.compactMap(instance) }

    /// Node is in the plan while any chosen agent needs it and it is not ready,
    /// and stays listed (as done) once installed here.
    var needsNode: Bool {
        selected.compactMap(instance).contains { OnboardingProgress.usesNode($0) && EngineRuntimeProblem(agent: $0) != nil }
    }

    var showsNodeRow: Bool { needsNode || nodeAutoStarted }

    var installComplete: Bool {
        !selected.isEmpty && !isPreparing
            && selected.allSatisfy { harnessID in
                guard let agent = instance(harnessID) else { return false }
                return OnboardingProgress.isInstalled(agent)
                    && !OnboardingProgress.needsLayout(agent, usesCLISetup: cliSetup.contains(harnessID))
                    && !(OnboardingProgress.usesNode(agent) && EngineRuntimeProblem(agent: agent) != nil)
            }
    }

    /// Sets each chosen agent's home layout, then starts installs. Seeded
    /// instances switch layout while unclaimed; a claimed one keeps its home,
    /// and the other layout becomes a new instance beside it.
    private func prepareInstall() async {
        isPreparing = true
        errorText = nil
        if !agents.didLoadOnce { await agents.load() }
        // Without the list every choice looks like it has no instance, and
        // this would create profiles that already exist. A reconnect retries.
        guard agents.didLoadOnce else {
            errorText = agents.errorText
            isPreparing = false
            return
        }
        for harnessID in selected {
            let wantsCLI = cliSetup.contains(harnessID)
            guard let agent = instance(harnessID) else {
                // The seed is claimed by the other layout and no instance of
                // this one exists yet.
                do {
                    if wantsCLI {
                        _ = try await agents.addExistingSetup(harnessID: harnessID)
                    } else {
                        _ = try await agents.createProfile(harnessID: harnessID, label: "Ottam profile")
                    }
                } catch {
                    errorText = error.localizedDescription
                }
                continue
            }
            guard OnboardingProgress.needsLayout(agent, usesCLISetup: wantsCLI) else { continue }
            do {
                if agent.canSetLayout == true {
                    _ = try await agents.useLayout(agentID: agent.agentID, layout: wantsCLI ? .existing : .managed)
                } else if wantsCLI {
                    _ = try await agents.addExistingSetup(harnessID: harnessID)
                }
            } catch {
                errorText = error.localizedDescription
            }
        }
        await agents.load()
        // Before driving: drive() waits out preparation.
        isPreparing = false
        drive()
    }

    /// Starts whatever can start now: Node first when a chosen agent needs it,
    /// then every adapter whose runtime is ready. Each starts automatically
    /// once; after a failure or cancel the row's Retry starts it again.
    func drive() {
        guard step == .install, !isPreparing else { return }
        let nodeState = runtime.runtimeInstall.state
        if needsNode, !nodeAutoStarted, !nodeState.isInFlight {
            nodeAutoStarted = true
            runtime.runtimeInstall.install()
        }
        for harnessID in selected where !autoStarted.contains(harnessID) {
            guard let agent = instance(harnessID), !OnboardingProgress.isInstalled(agent) else { continue }
            if OnboardingProgress.usesNode(agent), EngineRuntimeProblem(agent: agent) != nil { continue }
            autoStarted.insert(harnessID)
            runtime.agentInstall.install(agentID: AgentID(harnessID))
        }
    }

    func retryNode() { runtime.runtimeInstall.install() }

    func retry(_ harnessID: String) {
        runtime.agentInstall.forgetOutcome(agentID: AgentID(harnessID))
        runtime.agentInstall.install(agentID: AgentID(harnessID))
    }

    func cancel(_ harnessID: String) { runtime.agentInstall.cancel(agentID: AgentID(harnessID)) }

    /// Drops a chosen agent, so one that will not install never blocks the rest.
    func remove(_ harnessID: String) {
        runtime.agentInstall.cancel(agentID: AgentID(harnessID))
        selected.removeAll { $0 == harnessID }
        cliSetup.remove(harnessID)
        save()
        if selected.isEmpty { enter(.chooseAgents) }
    }

    /// A finished Node install changes every agent's runtime status; reload
    /// with a fresh resolve before starting the adapters that waited on it.
    func runtimePhaseChanged(_ phase: RuntimeInstallPhase?) {
        if phase == .ready { agents.requestRuntimeRefresh() }
    }

    // MARK: Sign in

    var signInComplete: Bool {
        !chosenInstances.isEmpty && chosenInstances.count == selected.count
            && chosenInstances.allSatisfy(OnboardingProgress.isSignedIn)
    }

    /// Nothing persists sign-in across launches, so an agent nobody has
    /// started this session reads as unknown until core asks it once. A CLI
    /// setup is always asked: one already signed in never signs in here, and
    /// its recheck is what fills the model list the named-agent step offers.
    func probeUnknownSignIns() {
        for agent in chosenInstances where !probed.contains(agent.agentID) {
            let stopped = if case .stopped = AgentSetupStatus(agent: agent) { true } else { false }
            if stopped || agent.homeKind == .existing {
                probed.insert(agent.agentID)
                recheck(agent.agentID)
            }
        }
    }

    /// Asks core for the agent's sign-in once more. A row left stopped after
    /// it offers Check Again rather than a spinner that never resolves.
    func recheck(_ agentID: AgentID) {
        guard !rechecking.contains(agentID) else { return }
        rechecking.insert(agentID)
        Task {
            await agents.probeProfile(agentID: agentID)
            rechecking.remove(agentID)
        }
    }

    func startSignIn(_ agent: AgentSummary) {
        let presentation = AgentSettingsPresentation(agent: agent)
        signIn.signIn(agentID: agent.agentID, displayName: agent.displayName, methodID: presentation.providerSetupMethodID)
    }

    // MARK: Named agent

    /// The agent a new named agent runs on: the first one chosen here.
    var defaultEngine: EngineSummary? {
        OnboardingProgress.defaultEngine(chosen: chosenInstances, engines: namedAgents.engines)
    }

    func canAdd(_ preset: OnboardingRolePreset) -> Bool {
        addingPreset == nil && OnboardingProgress.canAdd(preset, on: defaultEngine)
    }

    func reloadNamedAgents() async { await namedAgents.load(workspaceID: nil) }

    /// Suggestions not yet added, by name.
    var suggestions: [OnboardingRolePreset] {
        let taken = Set(namedAgents.namedAgents.map { $0.name.lowercased() })
        return OnboardingRolePreset.all.filter { !taken.contains($0.name) }
    }

    private(set) var addingPreset: String?

    /// Adds a suggestion as is, on the default agent and model. A writing role
    /// starts in the agent's own default mode; the rest stay read-only, which
    /// an empty mode means.
    func add(_ preset: OnboardingRolePreset) async {
        guard let engine = defaultEngine, OnboardingProgress.canAdd(preset, on: engine) else { return }
        addingPreset = preset.name
        defer { addingPreset = nil }
        errorText = nil
        let config = NamedAgentConfig(
            name: preset.name,
            engineID: engine.engineID,
            modelID: nil,
            modeID: preset.writes ? engine.defaultModeID : nil,
            instructions: preset.instructions,
            avatarSeed: Int(preset.avatar.seed)
        )
        do { try await namedAgents.save(nil, config: config) } catch { errorText = error.localizedDescription }
    }

    var canFinish: Bool { !namedAgents.namedAgents.isEmpty }

    func finish() -> Bool {
        errorText = nil
        do {
            try runtime.completeOnboarding()
            return true
        } catch {
            errorText = error.localizedDescription
            return false
        }
    }

    // MARK: Steps

    private var record: OnboardingRecord {
        OnboardingRecord(status: .inProgress, selectedHarnessIDs: selected, cliSetupHarnessIDs: selected.filter(cliSetup.contains))
    }

    private func save() {
        do { try runtime.onboardingStore.save(record) } catch { errorText = error.localizedDescription }
    }

    private func enter(_ next: OnboardingStep) {
        step = next
        errorText = nil
        switch next {
        case .install:
            Task { await prepareInstall() }
        case .signIn:
            probeUnknownSignIns()
        case .namedAgent:
            Task { await reloadNamedAgents() }
        case .intro, .chooseAgents:
            break
        }
    }

    func continueFromInstall() { enter(.signIn) }
    func continueFromSignIn() { enter(.namedAgent) }

    /// Choose has no Back: onboarding has no way out but finishing.
    var canGoBack: Bool { step > .chooseAgents && !isPreparing }

    /// Steps back one page. Installs and sign-ins already started carry on,
    /// and returning to Install starts only what has not run yet.
    func goBack() {
        guard canGoBack, let previous = OnboardingStep(rawValue: step.rawValue - 1) else { return }
        enter(previous)
    }
}
