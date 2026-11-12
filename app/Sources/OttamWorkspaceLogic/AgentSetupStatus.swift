import OttamBackendClient

/// One agent's answer to "is this ready to use?", as the Settings › Agents
/// pane presents it.
///
/// Unlike `EngineSummary.isUsable`, this works from an `AgentSummary`, which
/// has no install/auth-state derivation — `agent.list` is the app-scoped RPC,
/// reachable without a workspace, and it reports the adapter raw.
public enum AgentSetupStatus: Equatable, Sendable {
    /// The Node runtime behind the adapter is the blocker; auth is moot.
    case runtimeProblem(EngineRuntimeProblem)
    /// The adapter started and then failed. Core sanitizes the optional detail
    /// before it crosses the protocol boundary, so it is safe to show here.
    case adapterProblem(String?)
    /// The adapter process is up but has not reported a sign-in verdict yet.
    case running
    /// Nothing is running and nothing is known: the adapter has not started
    /// this app session, and nothing persists sign-in state across launches.
    /// Presented as no status at all — the normal cold state, not a problem.
    case stopped
    case signedOut
    /// Credentials were seen earlier this adapter generation and are now
    /// gone — "sign in again", not "sign in".
    case expired
    case ready
    /// A multi-provider agent, by the providers set up rather than whether a
    /// session opened: omp opens one with only Apple's on-device model, so
    /// "signed in" would say nothing. Empty means none set up.
    case providers([String])

    public init(agent: AgentSummary) {
        // Runtime first, matching the engine picker's ordering: an agent whose
        // Node is broken also reports unusable auth, and the runtime is the
        // actionable half.
        if let problem = EngineRuntimeProblem(agent: agent) {
            self = .runtimeProblem(problem)
            return
        }
        if agent.adapter.lifecycle == .dead {
            self = .adapterProblem(agent.adapter.lastError)
            return
        }
        // A missing or in-flight sign-in still reads as one; otherwise the
        // providers last seen are the better answer, even while stopped.
        if let providers = agent.modelProviders,
           agent.adapter.authState == .ready || agent.adapter.authState == .unknown
        {
            self = .providers(providers)
            return
        }
        switch agent.adapter.authState {
        case .ready:
            self = .ready
        // Same table as the server's engine.list derivation
        // (backend/crates/ottam-core/src/server/engines.rs): ACP cannot
        // tell "never signed in" from "lapsed"; credentialsSeen is the only
        // evidence separating them. AgentSummary carries the raw state, so the
        // derivation is repeated here — keep the two in step.
        case .missing where agent.adapter.credentialsSeen:
            self = .expired
        case .missing:
            self = .signedOut
        // No auth verdict: the process state is the only honest thing to say.
        case .unknown, .authenticating:
            switch agent.adapter.lifecycle {
            case .starting, .ready:
                self = .running
            case .stopped, .stopping, .dead:
                self = .stopped
            }
        }
    }

    /// The trailing status text, or `nil` for a stopped adapter — the normal
    /// cold state shows nothing rather than a verdict it does not have.
    public var label: String? {
        switch self {
        case .runtimeProblem(let problem):
            problem.menuSuffix
        case .adapterProblem:
            "Failed"
        case .running:
            "Running"
        case .stopped:
            nil
        case .signedOut:
            "Signed out"
        case .expired:
            "Sign-in expired"
        case .ready:
            "Signed in"
        case .providers(let providers):
            switch providers.count {
            case 0: "No providers"
            case 1: "1 provider"
            case let count: "\(count) providers"
            }
        }
    }

    public var symbolName: String? {
        switch self {
        case .runtimeProblem, .adapterProblem:
            "exclamationmark.triangle"
        case .running:
            "circle.inset.filled"
        case .stopped:
            nil
        case .signedOut, .expired, .providers([]):
            "person.crop.circle.badge.xmark"
        case .ready, .providers:
            "checkmark.circle"
        }
    }

    /// Whether the row should offer a way to sign in.
    public var wantsSignIn: Bool {
        switch self {
        case .signedOut, .expired, .providers([]):
            true
        case .runtimeProblem, .adapterProblem, .running, .stopped, .ready, .providers:
            false
        }
    }
}
