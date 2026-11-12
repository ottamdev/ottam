import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

private func engine(
    installState: EngineInstallState = .ready,
    authState: EngineAuthState = .ready,
    runtime: AgentRuntimeStatus? = nil
) -> EngineSummary {
    EngineSummary(
        engineID: EngineID("claude"),
        displayName: "Claude Code",
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

private func status(
    _ availability: AgentRuntimeAvailability,
    component: String? = nil,
    foundMajor: UInt64? = nil,
    minimumMajor: UInt64? = nil
) -> AgentRuntimeStatus {
    AgentRuntimeStatus(
        availability: availability,
        component: component,
        foundMajor: foundMajor,
        minimumMajor: minimumMajor,
        nodeVersion: nil,
        npxVersion: nil
    )
}

@Suite("EngineRuntimeProblem")
struct EngineRuntimeProblemTests {
    @Test("a ready runtime is not a problem")
    func readyIsNoProblem() {
        #expect(EngineRuntimeProblem(engine: engine(runtime: status(.ready))) == nil)
    }

    @Test("a runtime still being checked is no problem, and keeps the engine in the running")
    func checkingKeepsTheEngineUsable() {
        let checking = engine(installState: .unknown, runtime: status(.checking))
        #expect(EngineRuntimeProblem(engine: checking) == nil)
        #expect(checking.isUsable)
        #expect(engineSetupState(engines: [checking]) == .usable)
        #expect(EngineSummary.defaultSelection(from: [checking, engine()])?.installState == .unknown)
    }

    /// An older server sends no runtime at all. Inventing a problem for it
    /// would disable every engine against a server that is working fine.
    @Test("an absent runtime is not a problem")
    func absentRuntimeIsNoProblem() {
        #expect(EngineRuntimeProblem(engine: engine(runtime: nil)) == nil)
    }

    @Test("the missing component is carried through")
    func missingNamesItsComponent() {
        let problem = EngineRuntimeProblem(
            engine: engine(runtime: status(.missing, component: "npx"))
        )
        #expect(problem == .missing(component: "npx"))
        #expect(problem?.menuSuffix == "Npx required")
    }

    @Test("an unsupported runtime reports both versions")
    func unsupportedCarriesVersions() {
        let problem = EngineRuntimeProblem(
            engine: engine(runtime: status(.unsupported, foundMajor: 16, minimumMajor: 18))
        )
        #expect(problem == .unsupported(found: 16, minimum: 18))
        #expect(problem?.menuSuffix == "Node 16 too old (needs 18+)")
        #expect(problem?.explanation.contains("Node.js 18 or newer") == true)
    }

    /// A status that says "unsupported" without the numbers still has to
    /// produce sensible copy rather than crashing or reading as "needs 0" —
    /// and it falls back to the strictest engine floor, since telling someone
    /// to install 18 when their engine needs 22 wastes a whole round trip.
    @Test("missing version detail falls back to the strictest engine floor")
    func unsupportedWithoutDetail() {
        let problem = EngineRuntimeProblem(engine: engine(runtime: status(.unsupported)))
        #expect(problem == .unsupported(found: 0, minimum: 22))
    }
}

@Suite("EngineSummary.isUsable")
struct EngineUsabilityTests {
    @Test("a ready engine with a ready runtime is usable")
    func readyIsUsable() {
        #expect(engine(runtime: status(.ready)).isUsable)
    }

    /// The bug this closes: `invalid` used to map to an install state the
    /// picker treated as fine, so the engine looked selectable and failed only
    /// once a prompt had been sent.
    @Test("an invalid runtime makes an otherwise-ready engine unusable")
    func invalidRuntimeBlocks() {
        #expect(!engine(runtime: status(.invalid, component: "npx")).isUsable)
    }

    @Test("a missing runtime makes an engine unusable")
    func missingRuntimeBlocks() {
        #expect(!engine(runtime: status(.missing, component: "node")).isUsable)
    }

    @Test("sign-in state still gates an engine with a healthy runtime")
    func authStillGates() {
        #expect(!engine(authState: .missing, runtime: status(.ready)).isUsable)
        #expect(!engine(authState: .expired, runtime: status(.ready)).isUsable)
    }

    @Test("install state still gates an engine with no runtime reported")
    func installStateStillGates() {
        #expect(!engine(installState: .missing).isUsable)
        #expect(!engine(installState: .unsupported).isUsable)
    }
}

/// The install's presentation, which three surfaces share. A test here is what
/// stops the Agents pane and the chat card wording the same state differently.
@Suite("ManagedRuntimeInstall")
struct ManagedRuntimeInstallTests {
    @Test("an untouched install offers the action and nothing else")
    func idleOffersTheAction() {
        let install = ManagedRuntimeInstall()
        #expect(install.actionTitle == "Install Node.js")
        #expect(install.isInFlight == false)
        #expect(install.fractionCompleted == nil)
        #expect(install.detail == nil)
    }

    @Test("a download in progress reports its percentage")
    func downloadingReportsPercent() {
        let install = ManagedRuntimeInstall(
            phase: .downloading,
            bytesReceived: 43,
            bytesTotal: 100
        )
        #expect(install.actionTitle == "Downloading… 43%")
        #expect(install.isInFlight)
        #expect(install.fractionCompleted == 0.43)
    }

    /// A server that sends no Content-Length leaves the total unknown. An
    /// indeterminate spinner is honest; a bar stuck at 0% is not.
    @Test("an unknown total draws no progress bar")
    func unknownTotalHasNoFraction() {
        let install = ManagedRuntimeInstall(phase: .downloading, bytesReceived: 4096)
        #expect(install.actionTitle == "Downloading…")
        #expect(install.fractionCompleted == nil)
        #expect(install.isInFlight)
    }

    @Test("verifying and extracting stay in flight under their own names")
    func middlePhasesAreNamed() {
        #expect(ManagedRuntimeInstall(phase: .verifying).actionTitle == "Verifying…")
        #expect(ManagedRuntimeInstall(phase: .extracting).actionTitle == "Installing…")
        #expect(ManagedRuntimeInstall(phase: .verifying).isInFlight)
        #expect(ManagedRuntimeInstall(phase: .extracting).isInFlight)
    }

    @Test("a finished install names the version it landed")
    func readyNamesTheVersion() {
        let install = ManagedRuntimeInstall(phase: .ready, version: "v24.22.1")
        #expect(install.actionTitle == "Installed")
        #expect(install.isInFlight == false)
        #expect(install.detail == "Node 24.22.1 is ready.")
    }

    /// A failure has to be retryable from the same control, and has to say why
    /// — "Install Node.js" over a silently-failed download is a lie.
    @Test("a failure offers a retry and surfaces core's reason")
    func failureIsRetryable() {
        let install = ManagedRuntimeInstall(
            phase: .failed,
            error: "checksum mismatch: expected abc, got def"
        )
        #expect(install.actionTitle == "Try Again")
        #expect(install.isInFlight == false)
        #expect(install.detail?.contains("checksum") == true)
    }

    @Test("a cancelled install returns to the plain offer")
    func cancelledReturnsToIdle() {
        let install = ManagedRuntimeInstall(phase: .cancelled)
        #expect(install.actionTitle == "Install Node.js")
        #expect(install.isInFlight == false)
        #expect(install.detail == nil)
    }
}

@Suite("EngineSummary.defaultSelection")
struct EngineDefaultSelectionTests {
    private func named(
        _ id: String,
        installState: EngineInstallState = .ready,
        authState: EngineAuthState = .ready
    ) -> EngineSummary {
        var summary = engine(installState: installState, authState: authState)
        summary.engineID = EngineID(id)
        return summary
    }

    @Test("a usable engine beats a preferred one that is not installed")
    func usableBeatsMissingPreferred() {
        let engines = [
            named("claude", installState: .missing),
            named("codex"),
            named("other", installState: .missing),
        ]
        #expect(EngineSummary.defaultSelection(from: engines)?.engineID.rawValue == "codex")
    }

    @Test("an installed but signed-out engine beats one that is not installed")
    func installedBeatsMissing() {
        let engines = [
            named("claude", installState: .missing),
            named("codex", authState: .missing),
        ]
        #expect(EngineSummary.defaultSelection(from: engines)?.engineID.rawValue == "codex")
    }

    /// Ids carry no preference: core lists agents in its preference order,
    /// so an unfamiliar id first in the list wins a tie.
    @Test("list order only breaks ties within a readiness tier")
    func listOrderBreaksTies() {
        let allUsable = [named("other"), named("codex"), named("claude")]
        #expect(EngineSummary.defaultSelection(from: allUsable)?.engineID.rawValue == "other")

        let firstMissing = [
            named("other", installState: .missing),
            named("codex"),
            named("claude"),
        ]
        #expect(EngineSummary.defaultSelection(from: firstMissing)?.engineID.rawValue == "codex")
    }

    @Test("no engines means no selection")
    func emptyIsNil() {
        #expect(EngineSummary.defaultSelection(from: []) == nil)
    }
}

@Suite("ManagedRuntimeUpdatePresentation")
struct ManagedRuntimeUpdatePresentationTests {
    private func presentation(
        state: AgentUpdateState,
        error: String? = nil,
        install: ManagedRuntimeInstall = ManagedRuntimeInstall()
    ) -> ManagedRuntimeUpdatePresentation {
        ManagedRuntimeUpdatePresentation(
            update: AgentUpdateStatus(version: "v24.22.1", state: state, error: error),
            install: install
        )
    }

    @Test func aBackgroundUpdateReadsUpdatingWithNoButton() {
        let presented = presentation(state: .updating)
        #expect(presented.actionTitle == nil)
        #expect(presented.statusText == "Updating to Node 24.22.1…")
    }

    @Test func downloadProgressCarriesThePercentage() {
        let presented = presentation(
            state: .updating,
            install: ManagedRuntimeInstall(phase: .downloading, bytesReceived: 30, bytesTotal: 60)
        )
        #expect(presented.statusText == "Updating to Node 24.22.1… 50%")
    }

    @Test func aFailedUpdateOffersRetry() {
        let presented = presentation(state: .failed, error: "offline")
        #expect(presented.actionTitle == "Retry")
        #expect(presented.isActionEnabled)
        #expect(presented.detail?.contains("offline") == true)
        #expect(presented.detail?.contains("keep using the current version") == true)
    }

    @Test func aFailureEndsInOnePeriodWithOrWithoutAnError() {
        #expect(presentation(state: .failed).detail
            == "Couldn’t update to Node 24.22.1: the download did not finish. Agents keep using the current version.")
        #expect(presentation(state: .failed, error: "offline").detail
            == "Couldn’t update to Node 24.22.1: offline. Agents keep using the current version.")
    }

    @Test func aPendingUpdateOffersUpdateNow() {
        #expect(presentation(state: .pending).actionTitle == "Update Now")
    }
}
