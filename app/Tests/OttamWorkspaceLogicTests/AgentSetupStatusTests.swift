import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

private func agent(
    runtime: AgentRuntimeAvailability = .ready,
    component: String? = nil,
    lifecycle: AgentLifecycleState = .stopped,
    authState: AgentAuthState = .unknown,
    credentialsSeen: Bool = false,
    lastError: String? = nil,
    modelProviders: [String]? = nil
) -> AgentSummary {
    AgentSummary(
        agentID: AgentID("claude"),
        displayName: "Claude Code",
        vendorName: "Anthropic",
        accountLabel: "Claude account",
        accountKind: modelProviders == nil ? .subscription : .multiProvider,
        modelProviders: modelProviders,
        runtime: AgentRuntimeStatus(
            availability: runtime,
            component: component,
            foundMajor: nil,
            minimumMajor: nil,
            nodeVersion: nil,
            npxVersion: nil
        ),
        adapter: AgentAdapterStatus(
            lifecycle: lifecycle,
            authState: authState,
            authMethods: [],
            steeringCapability: .unsupported,
            supportsImageInput: false,
            compatibility: [],
            credentialsSeen: credentialsSeen,
            generation: 0,
            lastError: lastError
        )
    )
}

@Suite("AgentSetupStatus")
struct AgentSetupStatusTests {
    /// The cold-launch shape: adapter never started, nothing persisted. The
    /// pane shows no status at all rather than invent a sign-in verdict —
    /// cold is the normal state, not a problem to report.
    @Test("a stopped adapter with unknown auth shows no status")
    func coldShowsNothing() {
        let status = AgentSetupStatus(agent: agent())
        #expect(status == .stopped)
        #expect(status.label == nil)
        #expect(status.symbolName == nil)
        #expect(!status.wantsSignIn)
    }

    /// A live adapter that has not reported auth yet is at least *running*,
    /// and that is what the row says.
    @Test("a live adapter with unknown auth reads as running")
    func liveIsRunning() {
        let status = AgentSetupStatus(agent: agent(lifecycle: .ready))
        #expect(status == .running)
        #expect(status.label == "Running")
        #expect(!status.wantsSignIn)
    }

    @Test("a starting adapter already reads as running")
    func startingIsRunning() {
        #expect(AgentSetupStatus(agent: agent(lifecycle: .starting)) == .running)
    }

    // The same table engine.list derives server-side (engines.rs): missing
    // alone is "never signed in"; missing after credentials were seen is a
    // lapse. If these cases change, change both places.
    @Test("missing credentials read as signed out")
    func missingIsSignedOut() {
        let status = AgentSetupStatus(agent: agent(authState: .missing))
        #expect(status == .signedOut)
        #expect(status.wantsSignIn)
    }

    @Test("missing credentials that were once seen read as expired")
    func missingAfterSeenIsExpired() {
        let status = AgentSetupStatus(
            agent: agent(authState: .missing, credentialsSeen: true)
        )
        #expect(status == .expired)
        #expect(status.wantsSignIn)
    }

    @Test("ready auth reads as signed in")
    func readyIsSignedIn() {
        #expect(AgentSetupStatus(agent: agent(authState: .ready)) == .ready)
    }

    @Test("authenticating is still not a verdict")
    func authenticatingIsNotAVerdict() {
        #expect(AgentSetupStatus(agent: agent(authState: .authenticating)) == .stopped)
        #expect(
            AgentSetupStatus(agent: agent(lifecycle: .ready, authState: .authenticating))
                == .running
        )
    }

    @Test("a dead adapter surfaces its recorded failure")
    func deadAdapterIsUnavailable() {
        let status = AgentSetupStatus(agent: agent(
            lifecycle: .dead,
            authState: .unknown,
            lastError: "adapter handshake failed"
        ))
        #expect(status == .adapterProblem("adapter handshake failed"))
        #expect(status.label == "Failed")
        #expect(!status.wantsSignIn)
    }

    /// Runtime first, matching the engine picker: a broken runtime makes the
    /// auth column moot, and telling the user to sign in when Node is missing
    /// sends them down the wrong path.
    @Test("a runtime problem outranks any auth state")
    func runtimeBeatsAuth() {
        let status = AgentSetupStatus(
            agent: agent(runtime: .missing, component: "node", authState: .ready)
        )
        #expect(status == .runtimeProblem(.missing(component: "node")))
        #expect(!status.wantsSignIn)
    }

    /// omp opens a session on Apple's on-device model alone, so "ready" says
    /// nothing; core leaves keyless providers out of the list.
    @Test("a multi-provider agent reads by its providers, not its session")
    func multiProviderReadsByProviders() {
        let none = AgentSetupStatus(agent: agent(authState: .ready, modelProviders: []))
        #expect(none == .providers([]))
        #expect(none.label == "No providers")
        #expect(none.wantsSignIn)

        let one = AgentSetupStatus(agent: agent(modelProviders: ["opencode-go"]))
        #expect(one.label == "1 provider")
        #expect(!one.wantsSignIn)
        #expect(
            AgentSetupStatus(agent: agent(authState: .ready, modelProviders: ["anthropic", "opencode-go"])).label
                == "2 providers"
        )

        // A sign-in verdict of its own still wins.
        #expect(AgentSetupStatus(agent: agent(authState: .missing, modelProviders: [])) == .signedOut)
    }
}
