import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("AgentAdapterInstall")
struct AgentAdapterInstallTests {
    @Test func onlyActivePhasesAreInFlight() {
        #expect(AgentAdapterInstall(phase: .starting).isInFlight)
        #expect(AgentAdapterInstall(phase: .installing).isInFlight)
        #expect(AgentAdapterInstall(phase: .verifying).isInFlight)
        #expect(!AgentAdapterInstall(phase: .ready).isInFlight)
        #expect(!AgentAdapterInstall(phase: .failed).isInFlight)
        #expect(!AgentAdapterInstall(phase: .cancelled).isInFlight)
        #expect(!AgentAdapterInstall().isInFlight)
    }
}

@Suite("AgentAdapterInstallPresentation")
struct AgentAdapterInstallPresentationTests {
    private func presentation(
        installState: AgentAdapterInstallState?,
        phase: AgentInstallPhase? = nil,
        error: String? = nil
    ) -> AgentAdapterInstallPresentation {
        AgentAdapterInstallPresentation(
            installState: installState,
            install: AgentAdapterInstall(phase: phase, error: error)
        )
    }

    @Test func anUninstalledAdapterOffersGet() {
        let presented = presentation(installState: .notInstalled)
        #expect(presented.actionTitle == "Get")
        #expect(presented.isActionEnabled)
        #expect(presented.statusText == nil)
        #expect(presented.detail == nil)
    }

    @Test func anInstalledAdapterReadsInstalled() {
        let presented = presentation(installState: .installed)
        #expect(presented.actionTitle == nil)
        #expect(presented.statusText == "Installed")
    }

    @Test func aBuiltInAgentShowsNothing() {
        let presented = presentation(installState: nil)
        #expect(presented.actionTitle == nil)
        #expect(presented.statusText == nil)
        #expect(presented.detail == nil)
    }

    @Test func theInFlightPhasesDisableTheButton() {
        #expect(presentation(installState: .notInstalled, phase: .starting).actionTitle == "Starting…")
        #expect(presentation(installState: .notInstalled, phase: .installing).actionTitle == "Installing…")
        #expect(presentation(installState: .notInstalled, phase: .verifying).actionTitle == "Verifying…")
        for phase: AgentInstallPhase in [.starting, .installing, .verifying] {
            #expect(!presentation(installState: .notInstalled, phase: phase).isActionEnabled)
        }
    }

    @Test func theLiveFlowWinsOverAStaleListSnapshot() {
        // The list still says installed while a pin-bump reinstall runs.
        let presented = presentation(installState: .installed, phase: .installing)
        #expect(presented.actionTitle == "Installing…")
        #expect(presented.statusText == nil)
    }

    @Test func aFailureOffersTryAgainWithTheError() {
        let presented = presentation(
            installState: .notInstalled,
            phase: .failed,
            error: "npm install exited with status 1"
        )
        #expect(presented.actionTitle == "Try Again")
        #expect(presented.isActionEnabled)
        #expect(presented.detail == "npm install exited with status 1")
    }

    @Test func aFailureWithoutDetailStillExplainsItself() {
        let presented = presentation(installState: .notInstalled, phase: .failed)
        #expect(presented.detail == "The install did not finish.")
    }

    @Test func readySettlesOnInstalled() {
        let presented = presentation(installState: .notInstalled, phase: .ready)
        #expect(presented.actionTitle == nil)
        #expect(presented.statusText == "Installed")
    }

    @Test func aCancelledInstallFallsBackToGet() {
        let presented = presentation(installState: .notInstalled, phase: .cancelled)
        #expect(presented.actionTitle == "Get")
        #expect(presented.isActionEnabled)
    }

    @Test func anotherWindowsInstallReadsInstalling() {
        let presented = presentation(installState: .installing)
        #expect(presented.actionTitle == "Installing…")
        #expect(!presented.isActionEnabled)
    }
}

/// An app update moved the pin; the older install keeps launching, so the
/// slot reads as an update and never as a first install.
@Suite("AgentAdapterInstallPresentation · updates")
struct AgentAdapterUpdatePresentationTests {
    private func presentation(
        state: AgentUpdateState,
        error: String? = nil,
        phase: AgentInstallPhase? = nil,
        installError: String? = nil
    ) -> AgentAdapterInstallPresentation {
        AgentAdapterInstallPresentation(
            installState: .installed,
            install: AgentAdapterInstall(phase: phase, error: installError),
            installedVersion: "0.85.1",
            update: AgentUpdateStatus(version: "0.95.0", state: state, error: error)
        )
    }

    @Test func aBackgroundUpdateReadsUpdating() {
        let presented = presentation(state: .updating)
        #expect(presented.actionTitle == "Updating to 0.95.0…")
        #expect(!presented.isActionEnabled)
        #expect(presented.detail == nil)
    }

    @Test func aFailedUpdateOffersRetryAndSaysTheOldVersionStillWorks() {
        let presented = presentation(state: .failed, error: "npm install failed: offline")
        #expect(presented.actionTitle == "Retry")
        #expect(presented.isActionEnabled)
        #expect(presented.detail?.contains("npm install failed: offline") == true)
        #expect(presented.detail?.contains("0.85.1 keeps working") == true)
    }

    @Test func aMultiLineNpmFailureShowsOnlyItsFirstLine() {
        let presented = presentation(
            state: .failed,
            error: "npm install failed: npm error code ECONNREFUSED\nnpm error syscall connect\nnpm error     at ClientRequest"
        )
        #expect(presented.detail?.contains("ECONNREFUSED. Version 0.85.1 keeps working") == true)
        #expect(presented.detail?.contains("syscall") == false)
        #expect(presented.detail?.contains("\n") == false)
    }

    @Test func aPendingUpdateOffersUpdateNow() {
        let presented = presentation(state: .pending)
        #expect(presented.actionTitle == "Update Now")
        #expect(presented.isActionEnabled)
    }

    @Test func theLiveFlowWinsOverTheListSnapshot() {
        #expect(presentation(state: .pending, phase: .installing).actionTitle == "Updating to 0.95.0…")
        #expect(presentation(state: .pending, phase: .verifying).actionTitle == "Verifying 0.95.0…")
        let failed = presentation(state: .updating, phase: .failed, installError: "handshake failed")
        #expect(failed.actionTitle == "Retry")
        #expect(failed.detail?.contains("handshake failed") == true)
        #expect(presentation(state: .updating, phase: .ready).statusText == "Installed")
    }
}
