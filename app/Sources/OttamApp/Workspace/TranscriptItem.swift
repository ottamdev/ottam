import OttamBackendClient
import OttamPresentationLogic
import OttamWorkspaceLogic
import Foundation

/// One renderer-neutral value in the transcript plan.
///
/// It deliberately contains no SwiftUI, AppKit, TextKit, markdown-engine,
/// attachment-instance, callback, or theme state. Presentation layers adapt
/// these values into their own concrete views.
struct TranscriptItem: Equatable, Identifiable {
    enum Content: Equatable {
        case markdown(String, syntax: TranscriptMarkdownSyntax, container: TranscriptContainerStyle?)
        case card(ChatTranscriptCardSpec)
    }

    enum Relation: Equatable, Sendable {
        case turn
        case attachesToPrevious
    }

    let id: String
    let content: Content
    var relation: Relation = .turn

    init(id: String, content: Content, relation: Relation = .turn) {
        self.id = id
        self.content = content
        self.relation = relation
    }

    var markdownText: String? {
        if case .markdown(let text, _, _) = content { return text }
        return nil
    }

    var cardSpec: ChatTranscriptCardSpec? {
        if case .card(let spec) = content { return spec }
        return nil
    }
}

enum TranscriptMarkdownSyntax: Equatable, Sendable {
    case inline
    case full
}

enum TranscriptContainerStyle: Equatable, Sendable {
    case bubble
}

/// Why an assistant turn stopped producing output, as far as the trailer has to
/// explain it to the reader.
///
/// Derived from the run's status, not its stop reason: the wire flattens
/// `interruptedForSteering` into a generic `other`, while Core writes the
/// `interrupted` status on the steer path alone, so the status carries the
/// meaning by itself.
enum ChatTurnOutcome: Equatable, Sendable {
    /// The agent ended the turn itself.
    case complete
    /// The reader stopped it.
    case stopped
    /// A message sent mid-run cut it short. The agent has that message and
    /// answers it in the turn below.
    case steered
    /// It ended early, and the run that would say how is outside the loaded
    /// page, so only the message's own completion state is known.
    case endedEarly
    /// The run ended in an error after the agent took the turn. `reason` is
    /// the run's wire message, when the loaded page has it.
    case failed(reason: String?)
}

/// Renderer-neutral state for the stable row that trails an assistant turn.
///
/// A run-backed turn keeps this one presentation identity while it moves from
/// live status, through the persistence stand-in, to its canonical message.
enum ChatTurnTrailerPresentation: Equatable {
    case running(
        runID: ConversationRunID,
        status: ConversationRunStatus,
        startedAt: String?,
        activityCount: Int,
        activityLabel: String? = nil,
        backgroundTasks: Int = 0,
        subagents: Int = 0
    )
    case finished(
        runID: ConversationRunID,
        body: String,
        outcome: ChatTurnOutcome
    )
    case persisted(
        runID: ConversationRunID?,
        body: String,
        createdAt: String,
        outcome: ChatTurnOutcome,
        /// The run's background tasks still running after it ended.
        backgroundTasks: Int = 0,
        /// The tokens the turn used; nil for a run from before they were
        /// recorded, or one no longer on the loaded page.
        usage: ChatTurnUsage? = nil,
        /// A delegated child's latest turn whose report waits in its parent.
        sendsToParent: Bool = false
    )

    var copyBody: String? {
        switch self {
        case .running: nil
        case .finished(_, let body, _), .persisted(_, let body, _, _, _, _, _): body.isEmpty ? nil : body
        }
    }
}

/// What a finished turn's trailer says about the tokens it used.
enum ChatTurnUsage: Equatable {
    case tokens(ConversationRunUsage)
    /// The turn ended without its answer reporting usage.
    case unavailable

    init?(_ run: ConversationRunSummary) {
        if let usage = run.usage {
            self = .tokens(usage)
        } else if run.usageUnavailable {
            self = .unavailable
        } else {
            return nil
        }
    }
}

/// Renderer-neutral state for the stable row that trails a user message.
///
/// The mirror of `ChatTurnTrailerPresentation`: the same copy button and
/// timestamp the assistant side already carries, plus the state of the
/// message's delivery when there is something worth saying about it.
///
/// Delivery is silent unless something went wrong. A message the agent visibly
/// answers needs no badge asserting it arrived, `delivered` is a claim about
/// the adapter's handoff rather than the agent's attention, and an in-flight
/// prompt stays in flight for the whole turn — so the slot carries only the
/// outcomes the reader has to act on. Recovery-class outcomes are too big for
/// a row and keep their own card below this one.
struct ChatUserTrailerPresentation: Equatable {
    let messageID: ConversationMessageID
    /// What the copy button writes: the message as the reader sees it.
    let body: String
    let createdAt: String
    /// The delivery attempt whose one-line state the row carries, if any.
    var input: ConversationRunInput?
    /// Why that attempt's run failed, as the run reported it.
    var failureReason: String?
    var showsResend: Bool
    var isMutating: Bool
    /// Lined up with the leading edge, under a card that spans the column,
    /// rather than with a bubble on the trailing side.
    var isLeading = false
}

/// A saved execution failure, separate from the fixed-height turn trailer.
struct ChatRunFailurePresentation: Equatable {
    let runID: ConversationRunID
    let reason: String?
    var canRetry = false
    var isMutating = false

    var providerMessage: String? {
        guard let reason, !reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return reason
    }

    var explanation: String {
        guard let providerMessage else { return "The agent stopped because of an error. Partial output is kept above." }
        if providerMessage == "Internal error: Go usage limit exceeded" {
            return "OpenCode Go usage limit exceeded."
        }
        return providerMessage
    }
}

/// Everything in the transcript that is not markdown, represented as values.
/// Equality lets the native renderer preserve card identity while reconciling
/// canonical model updates.
enum ChatTranscriptCardSpec: Equatable {
    case attachments([ConversationAttachment])
    case userTrailer(ChatUserTrailerPresentation)
    case deliveryRecovery(ConversationRunInput, isMutating: Bool)
    case activityGroup([ChatLiveActivity], isActive: Bool)
    case turnTrailer(ChatTurnTrailerPresentation)
    case runFailure(ChatRunFailurePresentation)
    case permissionRecord(ChatPermissionRecord)
    case runtimeRecovery(ChatRuntimeRecovery, install: ManagedRuntimeInstall?, isChecking: Bool)
    case authRecovery(ChatAuthRecovery, failure: String?, isTerminalLogin: Bool, isMutating: Bool)
    case selectionRecovery(ChatSelectionRecovery, isMutating: Bool)
    case pendingPermission(ChatPendingPermission)
    /// A server the agent called needs a sign-in.
    case mcpSignIn(ChatMcpSignIn)
    /// Above the first output of a run the agent started on its own.
    case continuedAutomatically(ConversationRunID)
    /// Above the first output of a run a different agent answered, or whose
    /// new session was sent the earlier turns.
    case engineSwitched(ChatTranscriptDivider)
    /// A `delegate` call's child, the reports waiting for a turn, or a
    /// child's task.
    case delegation(ChatDelegationPresentation)
    /// A file tab's frontmatter, above the file's body.
    case frontmatter(MarkdownFrontmatter)

    static func == (lhs: Self, rhs: Self) -> Bool {
        switch (lhs, rhs) {
        case let (.attachments(a), .attachments(b)): return a == b
        case let (.userTrailer(a), .userTrailer(b)): return a == b
        case let (.deliveryRecovery(a, aMut), .deliveryRecovery(b, bMut)): return a == b && aMut == bMut
        case let (.activityGroup(a, aActive), .activityGroup(b, bActive)): return a == b && aActive == bActive
        case let (.turnTrailer(a), .turnTrailer(b)): return a == b
        case let (.runFailure(a), .runFailure(b)): return a == b
        case let (.permissionRecord(a), .permissionRecord(b)): return a == b
        case let (.runtimeRecovery(a, aInstall, aChecking), .runtimeRecovery(b, bInstall, bChecking)):
            return a == b && aInstall == bInstall && aChecking == bChecking
        case let (.authRecovery(a, aFail, aTerm, aMut), .authRecovery(b, bFail, bTerm, bMut)):
            return a == b && aFail == bFail && aTerm == bTerm && aMut == bMut
        case let (.selectionRecovery(a, aMut), .selectionRecovery(b, bMut)):
            return a == b && aMut == bMut
        case let (.pendingPermission(a), .pendingPermission(b)): return a === b
        case let (.continuedAutomatically(a), .continuedAutomatically(b)): return a == b
        case let (.engineSwitched(a), .engineSwitched(b)): return a == b
        case let (.delegation(a), .delegation(b)): return a == b
        case let (.frontmatter(a), .frontmatter(b)): return a == b
        case let (.mcpSignIn(a), .mcpSignIn(b)): return a == b
        default: return false
        }
    }
}
