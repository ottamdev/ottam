import OttamBackendClient
import OttamWorkspaceLogic

public enum ComposerPresentationMode: Equatable, Sendable {
    case preparing
    case setup
    case send
    /// A run is working. Return queues the message behind it rather than
    /// joining it; steering is the deliberate Command-Return gesture, so the
    /// primary action is named for what Return does.
    case queue
}

public enum ComposerLoadState: Equatable, Sendable {
    case loading
    case loaded
    case failed
}

public enum ComposerDisabledReason: Equatable, Sendable {
    case workspaceUnavailable
    case submissionInProgress
    case unresolvedDelivery
    case workingRootUnavailable
    case preparingEngines
    case agentSetupRequired
    case noSelectedEngine
    case selectedEngineUnavailable
}

public struct ComposerPresentation: Equatable, Sendable {
    public let mode: ComposerPresentationMode
    public let disabledReason: ComposerDisabledReason?

    public init(
        loadState: ComposerLoadState,
        hasWorkspace: Bool,
        isSubmitting: Bool,
        hasUnresolvedDelivery: Bool,
        isWorkingRootAvailable: Bool,
        hasLiveRun: Bool,
        selectedEngine: EngineSummary?,
        engines: [EngineSummary]
    ) {
        if hasLiveRun {
            mode = .queue
        } else if loadState == .loading {
            mode = .preparing
        } else if loadState == .loaded, shouldOfferAgentSetup(
            didCompleteInitialLoad: true,
            hasLiveRun: false,
            engines: engines
        ) {
            mode = .setup
        } else {
            mode = .send
        }

        if !hasWorkspace {
            disabledReason = .workspaceUnavailable
        } else if isSubmitting {
            disabledReason = .submissionInProgress
        } else if hasUnresolvedDelivery {
            disabledReason = .unresolvedDelivery
        } else if !isWorkingRootAvailable {
            disabledReason = .workingRootUnavailable
        } else if hasLiveRun {
            // Queueing needs nothing from the adapter, so a live run never
            // disables the composer. An engine that cannot steer only loses
            // Command-Return; Return still takes the message.
            disabledReason = nil
        } else if loadState == .loading {
            disabledReason = .preparingEngines
        } else if mode == .setup {
            disabledReason = .agentSetupRequired
        } else if let selectedEngine {
            disabledReason = selectedEngine.isUsable ? nil : .selectedEngineUnavailable
        } else {
            disabledReason = .noSelectedEngine
        }
    }

    public var setupNeeded: Bool { mode == .setup }

    public var actionLabel: String {
        mode == .queue ? "Queue" : "Send"
    }

    public var canSubmit: Bool {
        disabledReason == nil && (mode == .send || mode == .queue)
    }

    /// The composer is the one place that names a missing working root, since
    /// the window shows it nowhere else.
    public var placeholderOverride: String? {
        if disabledReason == .workingRootUnavailable {
            return "This conversation's folder is unavailable"
        }
        return mode == .setup ? "Set up an agent to start" : nil
    }
}

/// Reconciles run state observed through independently delivered responses and
/// notifications. A start response can still say `queued` after the live event
/// stream has already advanced the same run to `running`, so active state must
/// never move backward. Terminal state is similarly sticky against late events.
public enum ChatRunStatusReconciler {
    public static func reconcile(
        current: ConversationRunStatus,
        observed: ConversationRunStatus
    ) -> ConversationRunStatus {
        switch current {
        case .completed, .failed, .canceled, .interrupted:
            current
        case .running where observed == .queued:
            .running
        default:
            observed
        }
    }
}

public enum ChatDeliveryAction: Equatable, Sendable {
    case retry
    case markDelivered
    case startRetry
    case sendAgain

    public var title: String {
        switch self {
        case .retry: "Retry"
        case .markDelivered: "Mark Delivered"
        case .startRetry: "Start Retry"
        case .sendAgain: "Send Again"
        }
    }
}

public struct ChatDeliveryStatusPresentation: Equatable, Sendable {
    public let label: String
    public let isWarning: Bool
    public let action: ChatDeliveryAction?
    public let isUserMarkedDelivered: Bool
}

public struct ChatDeliveryRecoveryPresentation: Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        case deliveryUnknown
        case pendingRetry
    }

    public let kind: Kind
    public let title: String
    public let detail: String
    public let primaryAction: ChatDeliveryAction
    public let secondaryAction: ChatDeliveryAction?
    public let accessibilityValue: String

    public init(kind: Kind) {
        self.kind = kind
        switch kind {
        case .deliveryUnknown:
            title = "Delivery could not be confirmed"
            detail = "The agent may already have accepted this input. Retry can duplicate work; Mark Delivered records your confirmation without resending it."
            primaryAction = .retry
            secondaryAction = .markDelivered
            accessibilityValue = "delivery unknown"
        case .pendingRetry:
            title = "Retry is ready to start"
            detail = "The retry was saved, but its run did not start. Start it to continue; the agent may already have accepted the original input."
            primaryAction = .startRetry
            secondaryAction = nil
            accessibilityValue = "pending retry"
        }
    }
}

public enum ChatDeliveryPresentation: Equatable, Sendable {
    case hidden
    case status(ChatDeliveryStatusPresentation)
    case recovery(ChatDeliveryRecoveryPresentation)

    public init(
        status: RunInputStatus,
        inputKind: RunInputKind,
        deliveryEvidence: RunInputDeliveryEvidence?
    ) {
        let isPendingRetry = status == .pending && inputKind == .retry

        if isPendingRetry {
            self = .recovery(ChatDeliveryRecoveryPresentation(kind: .pendingRetry))
            return
        }

        switch status {
        case .pending, .sending:
            // Not a blip. A prompt input leaves `sending` only when
            // `session/prompt` returns, so it spans the whole turn — and for
            // that whole turn the run's own trailer is already saying the agent
            // is working. Labelling it here would have the transcript assert
            // the message arrived while the row denies it. Only a native steer
            // resolves fast enough for the window to mean anything, and one
            // honest state is worth more than a state that is right once.
            self = .hidden
        case .delivered:
            if deliveryEvidence == .userMarkedDelivered {
                self = .status(
                    ChatDeliveryStatusPresentation(
                        label: "Marked delivered",
                        isWarning: false,
                        action: nil,
                        isUserMarkedDelivered: true
                    )
                )
            } else {
                self = .hidden
            }
        case .deliveryUnknown:
            self = .recovery(ChatDeliveryRecoveryPresentation(kind: .deliveryUnknown))
        case .failed:
            self = .status(
                ChatDeliveryStatusPresentation(
                    label: "Failed",
                    isWarning: true,
                    action: .sendAgain,
                    isUserMarkedDelivered: false
                )
            )
        case .superseded:
            // Bookkeeping, not conversation: the retry that replaced it shows
            // its own state, and a row per attempt reads as a pile-up.
            self = .hidden
        }
    }

    public init(input: ConversationRunInput) {
        self.init(
            status: input.status,
            inputKind: input.kind,
            deliveryEvidence: input.deliveryEvidence
        )
    }
}

public enum ChatAuthRecoveryPhase: Equatable, Sendable {
    case signedOut
    case signingIn
    case signedIn
    case ready
}

public enum ChatAuthRecoveryAction: Equatable, Sendable {
    case signIn
    case signInAgain
    case cancelSignIn
    case sendAgain

    public var title: String {
        switch self {
        case .signIn: "Sign In"
        case .signInAgain: "Sign In Again"
        case .cancelSignIn: "Cancel"
        case .sendAgain: "Send Again"
        }
    }
}

public struct ChatAuthRecoveryPresentation: Equatable, Sendable {
    public let phase: ChatAuthRecoveryPhase
    public let hasLapsed: Bool
    public let canSendAgain: Bool
    public let primaryAction: ChatAuthRecoveryAction?
    public let visibleFailureMessage: String?

    public init(
        phase: ChatAuthRecoveryPhase,
        authState: EngineAuthState,
        hasUndeliveredInput: Bool,
        signInFailure: String? = nil
    ) {
        self.phase = phase
        hasLapsed = authState == .expired
        canSendAgain = hasUndeliveredInput && (phase == .signedIn || phase == .ready)
        switch phase {
        case .signedOut:
            primaryAction = hasLapsed ? .signInAgain : .signIn
        case .signingIn:
            primaryAction = .cancelSignIn
        case .signedIn, .ready:
            primaryAction = canSendAgain ? .sendAgain : nil
        }
        visibleFailureMessage = phase == .signedOut ? signInFailure : nil
    }

    public func title(engineDisplayName: String) -> String {
        switch phase {
        case .signedOut:
            hasLapsed
                ? "\(engineDisplayName) needs signing in again"
                : "\(engineDisplayName) is not signed in"
        case .signingIn:
            "Signing in to \(engineDisplayName)"
        case .signedIn:
            "Signed in to \(engineDisplayName)"
        case .ready:
            "This message never reached \(engineDisplayName)"
        }
    }

    public func detail(engineDisplayName: String) -> String {
        switch phase {
        case .signedOut:
            hasLapsed
                ? "The saved credentials stopped working, so this run never reached the agent. Sign in again to pick it back up."
                : "This run never reached the agent. Sign in to pick it back up."
        case .signingIn:
            "Ottam is waiting for the login to finish. This message is still here — you can send it again as soon as you are back."
        case .signedIn:
            canSendAgain
                ? "The message never reached the agent, so nothing was lost. Send it again to continue this conversation."
                : "The agent is ready again. Carry on where you left off."
        case .ready:
            "The credentials are working now, but this message was rejected before the agent saw it. Send it again to continue."
        }
    }

}
