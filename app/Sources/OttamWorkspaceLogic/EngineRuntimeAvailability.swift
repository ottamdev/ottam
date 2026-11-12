import OttamBackendClient

/// Why an engine's Node runtime cannot start its adapter.
///
/// The agent adapters are npm packages, so an engine is only as available as
/// the Node install behind it. `EngineInstallState` says whether the engine is
/// usable at all; this says what the user would have to fix, which is what any
/// affordance offering a fix needs to know.
public enum EngineRuntimeProblem: Equatable, Sendable {
    /// `node` or `npx` was not found anywhere the app looks.
    case missing(component: String)
    /// Found, but it answered `--version` with something unparseable.
    case invalid(component: String)
    case unsupported(found: UInt64, minimum: UInt64)

    /// The problem behind an engine, if any. Engines with no runtime of their
    /// own — and summaries from a server predating the field — report none,
    /// which leaves them judged on `installState` alone as before.
    public init?(engine: EngineSummary) {
        guard let runtime = engine.runtime else { return nil }
        self.init(runtime: runtime)
    }

    /// The agent-scoped view of the same status: `agent.list` reports every
    /// agent's runtime unconditionally, so there is no absent-field case.
    public init?(agent: AgentSummary) {
        self.init(runtime: agent.runtime)
    }

    public init?(runtime: AgentRuntimeStatus) {
        switch runtime.availability {
        case .ready, .checking:
            // A probe still running has found no problem. Treating it as one
            // would disable the engine and move a cold launch off it.
            return nil
        case .missing:
            self = .missing(component: runtime.component ?? "node")
        case .invalid:
            self = .invalid(component: runtime.component ?? "node")
        case .unsupported:
            // Engines declare their own floor (Claude 22, Codex 18), so
            // a status that omits one falls back to the strictest rather than
            // the most permissive: understating it sends the user off to
            // install a version that still will not run their engine.
            self = .unsupported(
                found: runtime.foundMajor ?? 0,
                minimum: runtime.minimumMajor ?? 22
            )
        }
    }

    /// The suffix an engine picker appends to an unusable engine's name.
    public var menuSuffix: String {
        switch self {
        case .missing(let component):
            "\(component.capitalized) required"
        case .invalid(let component):
            "\(component.capitalized) not working"
        case .unsupported(let found, let minimum):
            "Node \(found) too old (needs \(minimum)+)"
        }
    }

    public var title: String {
        switch self {
        case .missing:
            "Node.js is required"
        case .invalid(let component):
            "\(component) is not working"
        case .unsupported:
            "Node.js is out of date"
        }
    }

    public var explanation: String {
        switch self {
        case .missing(let component):
            // No version named here on purpose: a missing runtime reports only
            // which component is absent, and engines disagree on the floor, so
            // any number this picked would be wrong for some of them.
            """
            Agents run as npm packages, so Ottam needs Node.js to start \
            them — but \(component) was not found. Ottam can install its \
            own copy, which leaves anything already on your Mac alone.
            """
        case .invalid(let component):
            """
            \(component) was found but did not report a usable version. \
            Installing Ottam's own copy sidesteps it.
            """
        case .unsupported(let found, let minimum):
            """
            This agent needs Node.js \(minimum) or newer, and Node \(found) is \
            installed. Ottam can install its own copy and use that instead, \
            leaving your Node alone.
            """
        }
    }
}

/// What the managed-runtime install is doing right now, and how to say it.
///
/// One install exists per app launch, so this is a single value rather than
/// anything keyed. Presentation lives here rather than in the views because
/// three surfaces offer the same action — the Agents pane, the engine picker,
/// and the runtime card in chat — and they must not word it three ways.
public struct ManagedRuntimeInstall: Equatable, Sendable {
    public var phase: RuntimeInstallPhase?
    public var version: String?
    public var bytesReceived: UInt64?
    public var bytesTotal: UInt64?
    public var error: String?

    public init(
        phase: RuntimeInstallPhase? = nil,
        version: String? = nil,
        bytesReceived: UInt64? = nil,
        bytesTotal: UInt64? = nil,
        error: String? = nil
    ) {
        self.phase = phase
        self.version = version
        self.bytesReceived = bytesReceived
        self.bytesTotal = bytesTotal
        self.error = error
    }

    /// Work is under way and the button should not be pressable again.
    public var isInFlight: Bool {
        switch phase {
        case .starting, .downloading, .verifying, .extracting:
            true
        case .ready, .failed, .cancelled, nil:
            false
        }
    }

    /// Download progress, when it is known well enough to draw a bar. A server
    /// that sends no `Content-Length` leaves this nil, and the caller falls
    /// back to an indeterminate spinner rather than a bar that lies.
    public var fractionCompleted: Double? {
        guard phase == .downloading,
              let bytesTotal, bytesTotal > 0,
              let bytesReceived
        else { return nil }
        return min(1, Double(bytesReceived) / Double(bytesTotal))
    }

    /// The action's title. Deliberately reads as one continuous thing —
    /// *Install Node.js* → *Downloading… 43%* → *Verifying…* → *Installed* —
    /// rather than a button that swaps for a spinner and back.
    public var actionTitle: String {
        switch phase {
        case nil, .cancelled:
            "Install Node.js"
        case .starting:
            "Starting…"
        case .downloading:
            if let fractionCompleted {
                "Downloading… \(Int(fractionCompleted * 100))%"
            } else {
                "Downloading…"
            }
        case .verifying:
            "Verifying…"
        case .extracting:
            "Installing…"
        case .ready:
            "Installed"
        case .failed:
            "Try Again"
        }
    }

    /// A line under the action, when there is something worth saying that the
    /// title cannot carry. Nil most of the time — a progress label repeated in
    /// prose is noise.
    public var detail: String? {
        switch phase {
        case .failed:
            error ?? "The install did not finish."
        case .ready:
            version.map { "Node \($0.trimmingPrefix("v")) is ready." }
        default:
            nil
        }
    }
}

/// The Node Runtime section while an app update replaces the managed runtime.
/// Agents keep running on the older runtime, so this reads as an update in
/// progress — not as a missing runtime to install.
public struct ManagedRuntimeUpdatePresentation: Equatable, Sendable {
    /// The button, when there is something to press.
    public let actionTitle: String?
    public let isActionEnabled: Bool
    /// Progress or status when there is nothing to press.
    public let statusText: String?
    public let detail: String?

    public init(update: AgentUpdateStatus, install: ManagedRuntimeInstall) {
        let target = "Node \(update.version.trimmingPrefix("v"))"
        func failure(_ error: String?) -> String {
            let reason = error ?? "the download did not finish."
            return "Couldn’t update to \(target): \(reason)\(reason.hasSuffix(".") ? "" : ".") "
                + "Agents keep using the current version."
        }
        // The live flow wins over the possibly stale list snapshot.
        switch install.phase {
        case .starting, .downloading, .verifying, .extracting:
            actionTitle = nil
            isActionEnabled = false
            statusText = install.phase == .downloading
                ? install.actionTitle.replacingOccurrences(of: "Downloading", with: "Updating to \(target)")
                : "Updating to \(target)…"
            detail = nil
            return
        case .failed:
            actionTitle = "Retry"
            isActionEnabled = true
            statusText = nil
            detail = failure(install.error)
            return
        case .ready:
            actionTitle = nil
            isActionEnabled = false
            statusText = "Updated"
            detail = install.detail
            return
        case .cancelled, nil:
            break
        }
        switch update.state {
        case .updating:
            actionTitle = nil
            isActionEnabled = false
            statusText = "Updating to \(target)…"
            detail = nil
        case .failed:
            actionTitle = "Retry"
            isActionEnabled = true
            statusText = nil
            detail = failure(update.error)
        case .pending:
            actionTitle = "Update Now"
            isActionEnabled = true
            statusText = nil
            detail = "\(target) is available. Ottam installs it in the background at launch."
        }
    }
}

extension EngineSummary {
    /// Whether this engine can take a message right now.
    ///
    /// Shared by the engine picker and the composer's submit gate so a row the
    /// picker greys out can never be one the composer thinks it can send to.
    public var isUsable: Bool {
        installState != .missing
            && installState != .unsupported
            && authState != .missing
            && authState != .expired
            && EngineRuntimeProblem(engine: self) == nil
    }

    /// The engine the composer should start on when nothing is remembered.
    ///
    /// Readiness first — usable, then installed but a sign-in (or Node) away,
    /// then not installed — so a fresh machine lands on the adapter the user
    /// actually got rather than a disabled row. Within a tier, `engine.list`
    /// order decides: core lists agents in its preference order.
    public static func defaultSelection(from engines: [EngineSummary]) -> EngineSummary? {
        func rank(_ position: Int, _ engine: EngineSummary) -> (Int, Int) {
            let readiness = engine.isUsable ? 0 : (engine.installState == .missing ? 2 : 1)
            return (readiness, position)
        }
        return engines.enumerated()
            .min { rank($0.offset, $0.element) < rank($1.offset, $1.element) }?
            .element
    }
}
