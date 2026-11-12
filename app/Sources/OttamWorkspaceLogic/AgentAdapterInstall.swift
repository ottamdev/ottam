import OttamBackendClient

/// One agent's adapter-install flow, as the UI observes it.
///
/// Mirrors `ManagedRuntimeInstall`, minus byte counts (npm owns the fetch) and
/// plus a handshake: the `verifying` phase is a throwaway adapter completing
/// its first ACP handshake, so *Installed* is a claim the adapter has already
/// backed up.
public struct AgentAdapterInstall: Equatable, Sendable {
    public var phase: AgentInstallPhase?
    public var error: String?

    public init(phase: AgentInstallPhase? = nil, error: String? = nil) {
        self.phase = phase
        self.error = error
    }

    /// Work is under way and the button should not be pressable again.
    public var isInFlight: Bool {
        switch phase {
        case .starting, .installing, .verifying:
            true
        case .ready, .failed, .cancelled, nil:
            false
        }
    }
}

/// What the Adapter section shows for one agent: a single action slot that
/// reads as one continuous thing — *Get* → *Installing…* → *Verifying…* →
/// *Installed* / *Try Again* — combining the live install flow with the
/// steady state `agent.list` reports.
public struct AgentAdapterInstallPresentation: Equatable, Sendable {
    /// The button, when there is something to press.
    public let actionTitle: String?
    public let isActionEnabled: Bool
    /// The plain status when no action applies (already installed).
    public let statusText: String?
    /// A footer line, only when the title cannot carry the news — today that
    /// is the failure explanation.
    public let detail: String?

    /// - Parameters:
    ///   - installedVersion: The version launching now, from `agent.list`.
    ///   - update: Present while an older install stands in for an app
    ///     update's new pin. The agent stays usable throughout, so the slot
    ///     reads as an update — *Updating…* / *Update Now* / *Retry* — never as
    ///     a first install.
    public init(
        installState: AgentAdapterInstallState?,
        install: AgentAdapterInstall,
        installedVersion: String? = nil,
        update: AgentUpdateStatus? = nil
    ) {
        if let update {
            self = Self(update: update, installedVersion: installedVersion, install: install)
            return
        }
        // The live flow wins over the possibly stale list snapshot.
        switch install.phase {
        case .starting:
            actionTitle = "Starting…"
            isActionEnabled = false
            statusText = nil
            detail = nil
            return
        case .installing:
            actionTitle = "Installing…"
            isActionEnabled = false
            statusText = nil
            detail = nil
            return
        case .verifying:
            actionTitle = "Verifying…"
            isActionEnabled = false
            statusText = nil
            detail = nil
            return
        case .failed:
            actionTitle = "Try Again"
            isActionEnabled = true
            statusText = nil
            detail = install.error ?? "The install did not finish."
            return
        case .ready:
            actionTitle = nil
            isActionEnabled = false
            statusText = "Installed"
            detail = nil
            return
        case .cancelled, nil:
            break
        }
        switch installState {
        case .installed:
            actionTitle = nil
            isActionEnabled = false
            statusText = "Installed"
            detail = nil
        case .installing:
            // Another window's install, observed before any event arrived.
            actionTitle = "Installing…"
            isActionEnabled = false
            statusText = nil
            detail = nil
        case .notInstalled:
            actionTitle = "Get"
            isActionEnabled = true
            statusText = nil
            detail = nil
        case nil:
            // No pinned package (the in-process test agent): nothing to offer.
            actionTitle = nil
            isActionEnabled = false
            statusText = nil
            detail = nil
        }
    }

    private init(update: AgentUpdateStatus, installedVersion: String?, install: AgentAdapterInstall) {
        // npm reports a failure as its whole stderr, stack trace included; the
        // first line ("npm install failed: npm error code ECONNREFUSED") is
        // the part that fits under a Settings row. It ends the sentence the
        // keeps-working note follows.
        func headline(_ error: String?) -> String {
            let line = error?
                .split(whereSeparator: \.isNewline)
                .first
                .map(String.init) ?? "the install did not finish."
            return line.hasSuffix(".") ? line : line + "."
        }
        let keepsWorking = installedVersion.map { " Version \($0) keeps working until it does." } ?? ""
        switch install.phase {
        case .starting, .installing:
            self.init(action: "Updating to \(update.version)…")
            return
        case .verifying:
            self.init(action: "Verifying \(update.version)…")
            return
        case .failed:
            self.init(
                action: "Retry",
                enabled: true,
                detail: "Couldn’t update to \(update.version): \(headline(install.error))" + keepsWorking
            )
            return
        case .ready:
            self.init(status: "Installed")
            return
        case .cancelled, nil:
            break
        }
        switch update.state {
        case .updating:
            self.init(action: "Updating to \(update.version)…")
        case .failed:
            self.init(
                action: "Retry",
                enabled: true,
                detail: "Couldn’t update to \(update.version): \(headline(update.error))" + keepsWorking
            )
        case .pending:
            self.init(
                action: "Update Now",
                enabled: true,
                detail: "Version \(update.version) is available. Ottam installs it in the background at launch."
            )
        }
    }

    private init(action: String? = nil, enabled: Bool = false, status: String? = nil, detail: String? = nil) {
        actionTitle = action
        isActionEnabled = enabled
        statusText = status
        self.detail = detail
    }
}
