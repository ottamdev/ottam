import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

private func engine(
    id: String = "claude",
    installState: EngineInstallState = .ready,
    authState: EngineAuthState = .ready,
    runtime: AgentRuntimeStatus? = nil
) -> EngineSummary {
    EngineSummary(
        engineID: EngineID(id),
        displayName: id.capitalized,
        authState: authState,
        installState: installState,
        capabilities: [],
        models: [],
        modes: [],
        thoughtLevels: [],
        steeringCapability: .interruptThenPrompt,
        authMethods: [],
        runtime: runtime
    )
}

private func missingNode() -> AgentRuntimeStatus {
    AgentRuntimeStatus(
        availability: .missing,
        component: "node",
        foundMajor: nil,
        minimumMajor: nil,
        nodeVersion: nil,
        npxVersion: nil
    )
}

private func tooOld(found: UInt64, minimum: UInt64) -> AgentRuntimeStatus {
    AgentRuntimeStatus(
        availability: .unsupported,
        component: nil,
        foundMajor: found,
        minimumMajor: minimum,
        nodeVersion: nil,
        npxVersion: nil
    )
}

@Suite("engineSetupState")
struct EngineSetupStateTests {
    @Test("one usable engine is enough")
    func oneUsableWins() {
        let state = engineSetupState(engines: [
            engine(id: "claude", runtime: missingNode()),
            engine(id: "codex"),
        ])
        #expect(state == .usable)
    }

    @Test("an empty list is its own state")
    func emptyIsNoEngines() {
        #expect(engineSetupState(engines: []) == .noEngines)
    }

    /// The headline case: no Node anywhere, so every engine shares the same
    /// problem and the composer can name it instead of shrugging.
    @Test("a problem shared by every engine becomes the dominant one")
    func sharedProblemDominates() {
        let state = engineSetupState(engines: [
            engine(id: "claude", runtime: missingNode()),
            engine(id: "codex", runtime: missingNode()),
        ])
        #expect(state == .noneUsable(dominant: .missing(component: "node")))
    }

    /// Per-engine floors mean one Node can block engines differently — Node 20
    /// is too old for Claude but fine for Codex. Disagreement yields no dominant
    /// problem rather than arbitrarily promoting one engine's.
    @Test("disagreeing problems yield no dominant one")
    func disagreementHasNoDominant() {
        let state = engineSetupState(engines: [
            engine(id: "claude", runtime: tooOld(found: 20, minimum: 22)),
            engine(id: "codex", runtime: missingNode()),
        ])
        #expect(state == .noneUsable(dominant: nil))
    }

    /// Engines can be unusable for non-runtime reasons (sign-in). That is not
    /// a runtime problem, so there is nothing to name.
    @Test("auth-blocked engines have no dominant runtime problem")
    func authBlockedHasNoDominant() {
        let state = engineSetupState(engines: [
            engine(id: "claude", authState: .missing),
            engine(id: "codex", authState: .missing),
        ])
        #expect(state == .noneUsable(dominant: nil))
    }

    @Test("setup is not offered before the initial load succeeds")
    func unresolvedLoadDoesNotOfferSetup() {
        #expect(!shouldOfferAgentSetup(
            didCompleteInitialLoad: false,
            hasLiveRun: false,
            engines: []
        ))
    }

    @Test("setup is offered after a resolved load finds nothing usable")
    func resolvedUnusableEnginesOfferSetup() {
        #expect(shouldOfferAgentSetup(
            didCompleteInitialLoad: true,
            hasLiveRun: false,
            engines: [engine(authState: .missing)]
        ))
    }

    @Test("an active run suppresses setup even with no usable engines")
    func liveRunSuppressesSetup() {
        #expect(!shouldOfferAgentSetup(
            didCompleteInitialLoad: true,
            hasLiveRun: true,
            engines: []
        ))
    }
}
