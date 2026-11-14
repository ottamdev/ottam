import OttamBackendClient
import OttamWorkspaceLogic

enum OnboardingStep: Int, Comparable, CaseIterable {
    case intro
    case chooseAgents
    case install
    case signIn
    case namedAgent

    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

/// Onboarding's reading of `agent.list`, kept free of views and controllers
/// so resume and readiness are testable.
enum OnboardingProgress {
    /// The instance onboarding sets up for a harness. A CLI setup prefers the
    /// harness's existing-home instance; otherwise the seeded instance (whose
    /// ID is the harness ID) is the one to use while its layout can still
    /// change, then any managed instance of that harness.
    static func instance(harnessID: String, usesCLISetup: Bool, agents: [AgentSummary]) -> AgentSummary? {
        let ofHarness = agents.filter { ($0.harnessID ?? $0.agentID.rawValue) == harnessID }
        let seed = ofHarness.first { $0.agentID.rawValue == harnessID }
        if usesCLISetup {
            return ofHarness.first { $0.homeKind == .existing } ?? seed
        }
        if let seed, seed.homeKind != .existing || seed.canSetLayout == true { return seed }
        return ofHarness.first { $0.homeKind != .existing }
    }

    /// Whether the instance's home layout still has to be set for this choice.
    static func needsLayout(_ agent: AgentSummary, usesCLISetup: Bool) -> Bool {
        let wanted: AgentHomeKind = usesCLISetup ? .existing : .managed
        return (agent.homeKind ?? .managed) != wanted
    }

    static func isInstalled(_ agent: AgentSummary) -> Bool {
        (agent.adapterPackage?.installState ?? .installed) == .installed
    }

    /// Whether the agent needs the Node runtime: anything but a prebuilt binary.
    static func usesNode(_ agent: AgentSummary) -> Bool {
        agent.adapterPackage?.source != .binary
    }

    /// Signed in, or for a multi-provider agent, at least one provider set up.
    static func isSignedIn(_ agent: AgentSummary) -> Bool {
        switch AgentSetupStatus(agent: agent) {
        case .ready: true
        case .providers(let providers): !providers.isEmpty
        default: false
        }
    }

    /// The engine a new named agent runs on: the first chosen agent, in the
    /// picker's order, that the engine list reports.
    static func defaultEngine(chosen: [AgentSummary], engines: [EngineSummary]) -> EngineSummary? {
        chosen.lazy.compactMap { agent in
            engines.first { $0.engineID.rawValue == agent.agentID.rawValue }
        }.first
    }

    /// Whether a suggestion can be added as it promises. A writing role runs
    /// in the agent's default mode, which arrives with its model catalog;
    /// saved before that, core would store it read-only.
    static func canAdd(_ preset: OnboardingRolePreset, on engine: EngineSummary?) -> Bool {
        guard let engine else { return false }
        return !preset.writes || engine.defaultModeID != nil
    }

    /// Where a resumed onboarding picks up: the first step whose work is not
    /// yet done for every chosen agent.
    static func resumeStep(record: OnboardingRecord?, agents: [AgentSummary]) -> OnboardingStep {
        guard let record, !record.selectedHarnessIDs.isEmpty else { return .chooseAgents }
        let chosen = record.selectedHarnessIDs.map { harnessID in
            instance(
                harnessID: harnessID,
                usesCLISetup: record.cliSetupHarnessIDs.contains(harnessID),
                agents: agents
            )
        }
        guard chosen.allSatisfy({ $0 != nil }) else { return .install }
        let instances = chosen.compactMap(\.self)
        // Install is where Node installs, so a missing runtime resumes there.
        if instances.contains(where: {
            !isInstalled($0)
                || needsLayout($0, usesCLISetup: record.cliSetupHarnessIDs.contains($0.harnessID ?? $0.agentID.rawValue))
                || (usesNode($0) && EngineRuntimeProblem(agent: $0) != nil)
        }) {
            return .install
        }
        if instances.contains(where: { !isSignedIn($0) }) { return .signIn }
        return .namedAgent
    }
}

/// The named agents the last step suggests, each added in one click and
/// editable afterwards like any other.
struct OnboardingRolePreset: Identifiable, Equatable {
    let name: String
    /// One line on what the role is for, shown on its suggestion row.
    let summary: String
    let instructions: String
    /// Whether the role writes code, so it starts in the agent's default mode
    /// rather than read-only.
    let writes: Bool

    var id: String { name }

    /// The tile a name draws by default, so the suggestion shows the tile
    /// the added agent keeps.
    var avatar: AgentAvatar { AgentAvatar(seed: AgentAvatar.seed(forName: name)) }

    static let all: [OnboardingRolePreset] = [
        OnboardingRolePreset(
            name: "reviewer",
            summary: "Reviews changes for bugs and missing tests. Doesn’t edit files.",
            instructions: "Review the change you are given for correctness bugs, regressions, and missing tests. Lead with the most severe finding, cite file and line, and say how to reproduce it. Do not edit files.",
            writes: false
        ),
        OnboardingRolePreset(
            name: "implementer",
            summary: "Builds a task in small steps and runs the tests.",
            instructions: "Implement the task you are given in small, verifiable steps. Follow the repository's conventions, run the relevant tests, and report what changed and what you could not verify.",
            writes: true
        ),
        OnboardingRolePreset(
            name: "planner",
            summary: "Reads the code and writes a short plan. Doesn’t edit files.",
            instructions: "Read the relevant code and produce a short, ordered plan for the task: the files to change, the approach, the risks, and how to verify it. Do not edit files.",
            writes: false
        ),
        OnboardingRolePreset(
            name: "debugger",
            summary: "Finds the root cause of a problem. Doesn’t edit files.",
            instructions: "Find the root cause of the problem you are given. Reproduce it, narrow it down with evidence, and explain the mechanism. Propose a fix but do not edit files.",
            writes: false
        ),
    ]
}
