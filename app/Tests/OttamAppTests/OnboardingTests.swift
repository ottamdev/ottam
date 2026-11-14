import Foundation
import OttamBackendClient
import Testing
@testable import OttamApp

private func agent(
    _ id: String,
    harness: String? = nil,
    homeKind: AgentHomeKind = .managed,
    canSetLayout: Bool = true,
    installed: Bool = true,
    source: AgentAdapterSource = .npm,
    authState: AgentAuthState = .unknown
) -> AgentSummary {
    AgentSummary(
        canSetLayout: canSetLayout,
        harnessID: harness ?? id,
        homeKind: homeKind,
        agentID: AgentID(id),
        displayName: id,
        vendorName: "Vendor",
        accountLabel: "Account",
        accountKind: .subscription,
        runtime: AgentRuntimeStatus(
            availability: .ready, component: nil, foundMajor: nil, minimumMajor: nil,
            nodeVersion: nil, npxVersion: nil
        ),
        adapter: AgentAdapterStatus(
            lifecycle: .stopped, authState: authState, authMethods: [],
            steeringCapability: .unsupported, supportsImageInput: false, compatibility: [],
            credentialsSeen: false, generation: 0, lastError: nil
        ),
        adapterPackage: AgentAdapterPackage(
            name: "adapter", version: "1.0.0", source: source,
            installState: installed ? .installed : .notInstalled
        )
    )
}

private func engine(_ id: String, defaultModeID: String? = nil) -> EngineSummary {
    EngineSummary(
        engineID: EngineID(id), displayName: id, authState: .ready, installState: .ready,
        capabilities: [], models: [], modes: [], defaultModeID: defaultModeID, thoughtLevels: [],
        steeringCapability: .unsupported, authMethods: []
    )
}

private func temporaryHome() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("onboarding-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}

@Suite("Onboarding launch decision")
struct OnboardingStoreTests {
    @Test("a fresh home opens onboarding")
    func freshHome() throws {
        let home = try temporaryHome()
        let store = OnboardingStore(directory: home)
        #expect(store.launchNeedsOnboarding(environment: [:]))
        // Core creates the database once running; a relaunch before any
        // Continue still opens onboarding.
        FileManager.default.createFile(atPath: home.appendingPathComponent("ottam.sqlite").path, contents: Data())
        #expect(store.launchNeedsOnboarding(environment: [:]))
        #expect(store.load()?.status == .inProgress)
    }

    @Test("a home that predates onboarding is recorded complete and skips it")
    func legacyHome() throws {
        let home = try temporaryHome()
        FileManager.default.createFile(atPath: home.appendingPathComponent("ottam.sqlite").path, contents: Data())
        let store = OnboardingStore(directory: home)
        #expect(!store.launchNeedsOnboarding(environment: [:]))
        #expect(store.load() == .complete)
    }

    @Test("an unfinished onboarding resumes even after core created its database")
    func inProgressResumes() throws {
        let home = try temporaryHome()
        let store = OnboardingStore(directory: home)
        try store.save(OnboardingRecord(status: .inProgress, selectedHarnessIDs: ["codex"]))
        FileManager.default.createFile(atPath: store.databaseURL.path, contents: Data())
        #expect(store.launchNeedsOnboarding(environment: [:]))
        #expect(store.load()?.selectedHarnessIDs == ["codex"])
    }

    @Test("UI test runs skip onboarding unless they force it")
    func environmentOverrides() throws {
        let store = OnboardingStore(directory: try temporaryHome())
        #expect(!store.launchNeedsOnboarding(environment: ["OTTAM_UI_TEST_MODE": "1"]))
        #expect(store.launchNeedsOnboarding(environment: ["OTTAM_UI_TEST_MODE": "1", "OTTAM_ONBOARDING": "1"]))
        #expect(!store.launchNeedsOnboarding(environment: ["OTTAM_ONBOARDING": "0"]))
    }
}

@Suite("Onboarding progress")
struct OnboardingProgressTests {
    @Test("no saved choice starts at Choose")
    func noChoice() {
        #expect(OnboardingProgress.resumeStep(record: nil, agents: [agent("codex")]) == .chooseAgents)
    }

    @Test("an uninstalled chosen agent resumes at Install")
    func resumesAtInstall() {
        let record = OnboardingRecord(status: .inProgress, selectedHarnessIDs: ["codex", "claude"])
        let agents = [agent("codex"), agent("claude", installed: false)]
        #expect(OnboardingProgress.resumeStep(record: record, agents: agents) == .install)
    }

    @Test("a CLI setup whose layout is not set yet resumes at Install")
    func layoutPendingResumesAtInstall() {
        let record = OnboardingRecord(status: .inProgress, selectedHarnessIDs: ["codex"], cliSetupHarnessIDs: ["codex"])
        #expect(OnboardingProgress.resumeStep(record: record, agents: [agent("codex")]) == .install)
    }

    @Test("installed but not signed in resumes at Sign In; all signed in goes to the named agent")
    func resumesAfterInstall() {
        let record = OnboardingRecord(status: .inProgress, selectedHarnessIDs: ["codex"])
        #expect(OnboardingProgress.resumeStep(record: record, agents: [agent("codex")]) == .signIn)
        #expect(OnboardingProgress.resumeStep(record: record, agents: [agent("codex", authState: .ready)]) == .namedAgent)
    }

    @Test("a claimed seed keeps its home, so a CLI setup uses the existing-home instance")
    func claimedSeed() {
        let agents = [
            agent("codex", canSetLayout: false),
            agent("codex.cli", harness: "codex", homeKind: .existing),
        ]
        #expect(OnboardingProgress.instance(harnessID: "codex", usesCLISetup: true, agents: agents)?.agentID == AgentID("codex.cli"))
        #expect(OnboardingProgress.instance(harnessID: "codex", usesCLISetup: false, agents: agents)?.agentID == AgentID("codex"))
    }

    @Test("prebuilt binaries need no Node")
    func binaryNeedsNoNode() {
        #expect(!OnboardingProgress.usesNode(agent("opencode", source: .binary)))
        #expect(OnboardingProgress.usesNode(agent("codex")))
    }

    @Test("suggestions go to the first chosen agent, not the engine list's first")
    func defaultEngineFollowsChoice() {
        let chosen = [agent("codex"), agent("claude")]
        let engines = [engine("claude"), engine("codex")]
        #expect(OnboardingProgress.defaultEngine(chosen: chosen, engines: engines)?.engineID == EngineID("codex"))
        #expect(OnboardingProgress.defaultEngine(chosen: chosen, engines: [engine("claude")])?.engineID == EngineID("claude"))
        #expect(OnboardingProgress.defaultEngine(chosen: [], engines: engines) == nil)
    }

    @Test("the implementer waits for the agent's modes; read-only roles do not")
    func writingRoleWaitsForModes() throws {
        let implementer = try #require(OnboardingRolePreset.all.filter(\.writes).first)
        let reviewer = try #require(OnboardingRolePreset.all.filter { !$0.writes }.first)
        #expect(!OnboardingProgress.canAdd(implementer, on: engine("codex")))
        #expect(OnboardingProgress.canAdd(implementer, on: engine("codex", defaultModeID: "auto")))
        #expect(OnboardingProgress.canAdd(reviewer, on: engine("codex")))
        #expect(!OnboardingProgress.canAdd(reviewer, on: nil))
    }

    @Test("role presets use names core accepts and only the implementer writes")
    func presets() {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
        for preset in OnboardingRolePreset.all {
            #expect(preset.name.unicodeScalars.allSatisfy(allowed.contains))
        }
        #expect(OnboardingRolePreset.all.filter(\.writes).map(\.id) == ["implementer"])
    }
}
