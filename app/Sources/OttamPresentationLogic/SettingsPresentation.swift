import OttamBackendClient
import OttamWorkspaceLogic

/// The Settings window's sidebar entries. Case order is sidebar order and is
/// also the canonical visual-smoke page inventory.
public enum SettingsPane: String, CaseIterable, Hashable, Identifiable, Sendable {
    case app
    case agents
    case mcpServers
    case terminal
    case storage

    public var id: Self { self }

    public var title: String {
        switch self {
        case .app: "App"
        case .agents: "Agents"
        case .mcpServers: "MCP Servers"
        case .terminal: "Terminal"
        case .storage: "Storage"
        }
    }

    public var symbolName: String {
        switch self {
        case .app: "gearshape"
        case .agents: "cpu"
        case .mcpServers: "server.rack"
        case .terminal: "terminal"
        case .storage: "internaldrive"
        }
    }

    /// What the sidebar search matches. The Agents pane also answers to each
    /// listed agent's product and vendor name, so a new agent is findable
    /// without a keyword edit here.
    public func searchText(agents: [AgentSummary] = []) -> String {
        switch self {
        case .app: "\(title) theme appearance light dark system font typography typeface family size monospace code ui"
        case .agents:
            (["\(title) agent engine node runtime sign in account"]
                + agents.flatMap { [$0.displayName, $0.vendorName] })
                .joined(separator: " ")
        case .mcpServers: "\(title) model context protocol tools local remote command url environment header secret workspace"
        case .terminal: "\(title) ghostty overlay reload"
        case .storage: "\(title) cache disk space limit clear tool results"
        }
    }
}

public enum AgentPackagePresentation: Equatable, Sendable {
    case builtIn
    case npm(name: String, version: String)
    /// A vendor-built executable Ottam downloads; it needs no Node.
    case binary(name: String, version: String)

    /// Whether running the agent depends on a Node runtime.
    public var needsNode: Bool {
        switch self {
        case .npm: true
        case .builtIn, .binary: false
        }
    }
}

/// The providers whose CLI setup Ottam can use, matching core's
/// `existing_setup` table. A name for before `agent.list` loads.
public enum CLIProvider: String, CaseIterable, Sendable {
    case codex, claude, opencode, omp

    public init?(harnessID: String) { self.init(rawValue: harnessID) }

    /// An agent or engine id's harness: a profile's id is `<harness>.<profile>`.
    public init?(agentID: String) {
        self.init(rawValue: String(agentID.prefix { $0 != "." }))
    }

    public var displayName: String {
        switch self {
        case .codex: "Codex"
        case .claude: "Claude Code"
        case .opencode: "OpenCode"
        case .omp: "Oh-My-Pi"
        }
    }
}

/// Everything here comes from the agent's `agent.list` entry; nothing keys on
/// its id, so a new agent renders without an edit.
public struct AgentSettingsPresentation: Equatable, Sendable {
    public let status: AgentSetupStatus
    public let vendorName: String
    /// What signing in connects ("ChatGPT account"); titles the Account row.
    public let accountLabel: String
    public let package: AgentPackagePresentation
    /// The pinned adapter's install state from `agent.list`; nil for built-in
    /// agents, which have nothing to install.
    public let installState: AgentAdapterInstallState?
    public let nodeVersion: String?
    public let runtimeProblem: EngineRuntimeProblem?
    public let accountFootnote: String?
    public let homeFootnote: String
    /// The terminal sign-in Set Up Providers runs; nil hides the button.
    public let providerSetupMethodID: String?

    public init(agent: AgentSummary) {
        status = AgentSetupStatus(agent: agent)
        vendorName = agent.vendorName
        accountLabel = agent.accountLabel
        var home = "The agent runs against its own fresh home inside Ottam — "
            + "sessions and sign-in never touch the terminal CLI's, "
            + "and none of its customizations carry over."
        if let vendorHome = agent.vendorHome {
            home += " If you miss your old setup, feel free to prompt the agent "
                + "to pull in what it needs from \(vendorHome)."
        }
        homeFootnote = home
        if let adapterPackage = agent.adapterPackage {
            switch adapterPackage.source {
            case .npm:
                package = .npm(name: adapterPackage.name, version: adapterPackage.version)
            case .binary:
                package = .binary(name: adapterPackage.name, version: adapterPackage.version)
            }
            installState = adapterPackage.installState
        } else {
            package = .builtIn
            installState = nil
        }
        nodeVersion = agent.runtime.nodeVersion
        if case .runtimeProblem(let problem) = status {
            runtimeProblem = problem
        } else {
            runtimeProblem = nil
        }
        switch status {
        case .adapterProblem(let detail):
            accountFootnote = detail
                ?? "The agent stopped unexpectedly. Try it again from Chat."
        case .running, .stopped:
            accountFootnote = "Ottam checks this the first time you use the agent; nothing is stored across launches."
        case .signedOut, .expired:
            switch agent.accountKind {
            case .subscription:
                accountFootnote = "Sign-in opens a login terminal here; finish in your browser and press Done."
            case .apiKey, .multiProvider:
                accountFootnote = "Sign-in opens a login terminal here; press Done when it finishes."
            }
        case .providers([]):
            accountFootnote = "Set Up Providers opens the agent’s setup in a terminal here; add a provider, then press Done."
        case .providers(let providers):
            accountFootnote = "Set up: \(providers.joined(separator: ", "))."
        case .runtimeProblem, .ready:
            accountFootnote = nil
        }
        // A multi-provider agent can always take another provider, so its
        // terminal setup stays offered once signed in, in your CLI's own home
        // too. The catalog keeps an agent's methods after Remove, and a broken
        // runtime or adapter cannot start one, so those get no button.
        let canStart: Bool
        switch status {
        case .runtimeProblem, .adapterProblem: canStart = false
        default: canStart = (agent.adapterPackage?.installState ?? .installed) == .installed
        }
        providerSetupMethodID = canStart && agent.accountKind == .multiProvider
            ? agent.adapter.authMethods.first { $0.kind == .terminal }?.methodID
            : nil
    }

    public var wantsSignIn: Bool { status.wantsSignIn }

    /// Whether the user has this agent: its adapter is installed or on its
    /// way. Core reinstalls in the background when an app update moves the
    /// pin, and seeds a profile for every agent it knows, installed or not.
    public static func hasAdapter(_ agent: AgentSummary) -> Bool {
        switch agent.adapterPackage?.installState ?? .installed {
        case .installed, .installing: true
        case .notInstalled: false
        }
    }
}

/// The compatibility matrix as the Settings sheet shows it: one row per ACP
/// feature, grouped by which side of the protocol it lives on. Text and
/// symbols are decided here so the view stays a plain `Table`.
public struct AgentCompatibilityPresentation: Equatable, Sendable {
    public struct Row: Equatable, Identifiable, Sendable {
        public let feature: String
        public let sdkText: String
        public let agentText: String
        /// Why the agent column is empty; the cell's tooltip.
        public let agentDetail: String?
        public let clientText: String
        /// Which part a `Partial` row handles; the cell's tooltip.
        public let clientDetail: String?
        public let verdictSymbol: String
        /// The verdict word; the table's accessibility label for the symbol.
        public let verdictLabel: String

        public var id: String { feature }
    }

    public struct Section: Equatable, Identifiable, Sendable {
        public let title: String
        public let rows: [Row]

        public var id: String { title }
    }

    /// "41 ACP features supported"; `Partial` counts as supported, the
    /// row's Ottam column says which part. No denominator: the matrix also
    /// lists SDK features no agent uses, so "41 of 58" read as a failing score.
    public let summary: String
    public let sections: [Section]
    /// True until the agent has been probed once, when every agent column
    /// is empty.
    public let isUnprobed: Bool

    public init(compatibility: [AgentFeatureCompatibility]) {
        let supported = compatibility.filter { $0.clientSupport != .unsupported }.count
        summary = "\(supported) ACP \(supported == 1 ? "feature" : "features") supported"
        let unprobed = compatibility.allSatisfy { $0.agentAdvertises == nil }
        isUnprobed = unprobed
        sections = Self.groupOrder.compactMap { group in
            let rows = compatibility
                .filter { $0.group == group }
                .map { Row(row: $0, unprobed: unprobed) }
            return rows.isEmpty ? nil : Section(title: Self.title(for: group), rows: rows)
        }
    }

    private static let groupOrder: [AgentFeatureGroup] = [
        .agentCapability, .agentMethod, .updateKind, .clientRequest, .clientCapability, .extension,
    ]

    private static func title(for group: AgentFeatureGroup) -> String {
        switch group {
        case .agentCapability: "Agent capabilities"
        case .agentMethod: "Agent methods"
        case .updateKind: "Session updates"
        case .clientRequest: "Client requests"
        case .clientCapability: "Client capabilities"
        case .extension: "Extensions"
        }
    }
}

extension AgentCompatibilityPresentation.Row {
    init(row: AgentFeatureCompatibility, unprobed: Bool) {
        feature = row.feature
        sdkText = switch row.sdkStatus {
        case .stable: "Stable"
        case .unstableOn: "Unstable, on"
        case .unstableOff: "Unstable, off"
        case .notInBuild: "Not in this build"
        case .extension: row.sdkDetail ?? "Extension"
        }
        switch row.agentAdvertises {
        case .some(true):
            agentText = "Yes"
            agentDetail = nil
        case .some(false):
            agentText = "No"
            agentDetail = nil
        case .none:
            agentText = "—"
            agentDetail = unprobed
                ? "Not known yet: the agent has not been started, so it has not answered initialize."
                : "Not reported by initialize; only seen once a session runs."
        }
        switch row.clientSupport {
        case .supported:
            clientText = "Yes"
            clientDetail = nil
        case .partial:
            clientText = "Partial"
            clientDetail = row.clientDetail
        case .unsupported:
            clientText = "No"
            clientDetail = nil
        }
        (verdictSymbol, verdictLabel) = switch row.verdict {
        case .works: ("checkmark.circle", "Works")
        case .agentOnly: ("arrow.down.circle", "Agent only")
        case .clientOnly: ("arrow.up.circle", "Ottam only")
        case .neither: ("circle", "Neither")
        case .unknown: ("questionmark.circle", "Unknown")
        }
    }
}
