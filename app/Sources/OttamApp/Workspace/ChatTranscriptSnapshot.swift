import OttamBackendClient
import OttamPresentationLogic
import OttamWorkspaceLogic
import Foundation

/// Normalized Core items plus input delivery and live permission state.
struct ChatTranscriptSnapshot: Equatable {
    struct Message: Equatable {
        var message: ConversationMessage
        var runInputs: [ConversationRunInput] = []
        var authRecovery: ChatAuthRecovery?
        /// The sign-in controller's per-agent state, flattened for the card.
        var authSignInFailure: String?
        var authTerminalLogin = false
        var runtimeRecovery: ChatRuntimeRecovery?
        var selectionRecovery: ChatSelectionRecovery?
    }

    /// Document identity: a different conversation is a new document, not a
    /// diff of the old one.
    var conversationID: ConversationID?
    /// The Subagent tab's thread: its rows render as a run's do, its nested
    /// threads as cards, and nothing else of the conversation shows.
    var subagentThreadScope: String?
    var messages: [Message] = []
    var liveRun: ChatLiveRunState?
    /// Terminal status per run, so a persisted turn can say *how* it was cut
    /// short rather than only that it was. Runs page with the conversation, so
    /// a turn scrolled far enough back may not be here; the trailer then falls
    /// back to the message's own completion state.
    var runStatuses: [ConversationRunID: ConversationRunStatus] = [:]
    /// The wire message of each failed run on the loaded page, so the row that
    /// says "Failed" can also say why.
    var runFailures: [ConversationRunID: String] = [:]
    /// Only the latest failed turn can continue; historical failures stay readable.
    var retryableFailedRunID: ConversationRunID?
    /// What each finished run on the loaded page used, for its trailer.
    var runUsage: [ConversationRunID: ChatTurnUsage] = [:]
    /// Runs the agent started on its own; every other run is the user's.
    var autonomousRunIDs: Set<ConversationRunID> = []
    /// Runs a different agent answered than the run before, or whose new
    /// session was sent the earlier turns, with the divider each opens with.
    var engineSwitches: [ConversationRunID: ChatTranscriptDivider] = [:]
    /// Live background tasks per run, in this core instance.
    var backgroundTasks: [ConversationRunID: Int] = [:]
    /// Running subagent threads per run, in this core instance.
    var subagentThreads: [ConversationRunID: Int] = [:]
    var normalizedItems: [ConversationTranscriptItem] = []
    var normalizedBodies: [String: String] = [:]
    var bodyErrors: [String: String] = [:]
    var captureProblems: [ConversationRunID: String] = [:]
    var captureGaps: [ConversationRunID: [TranscriptCoverage]] = [:]
    var sessionStates: [ConversationRunID: ConversationSessionState] = [:]
    var isMutating = false
    var isRefreshingEngines = false
    /// The process-wide managed-runtime install, flattened; nil when no
    /// controller is reachable (the card then offers Check Again alone).
    var runtimeInstall: ManagedRuntimeInstall?
    /// A resend needs a free conversation to run in: Core refuses one while a
    /// run is live, and an offer that cannot be taken is worse than no offer.
    var allowsResend: Bool { liveRun == nil }
    /// The input whose resend a recovery card owns, wherever in the transcript
    /// that card hangs. Its delivery row shows no Send Again: two on one turn
    /// would be a puzzle, not a choice.
    var cardOwnedInputID: ConversationRunInputID? {
        for message in messages {
            if let id = message.authRecovery?.undeliveredInput?.runInputID
                ?? message.selectionRecovery?.undeliveredInput?.runInputID {
                return id
            }
        }
        return nil
    }
    /// A prompt error may fail its input after producing output. Those turns
    /// own recovery below the output, including after a later run finishes.
    /// Replaying their original input could repeat already completed work.
    var failureOwnedInputIDs: Set<ConversationRunInputID> {
        var failedRuns = Set(runStatuses.compactMap { $0.value == .failed ? $0.key : nil })
        failedRuns.formUnion(runFailures.keys)
        failedRuns.formUnion(normalizedItems.compactMap { $0.runStatus == .failed ? $0.runID : nil })
        let withOutput = Set(normalizedItems.compactMap { item -> ConversationRunID? in
            guard item.kind != .runBoundary, item.completion != .suppressed,
                  let runID = item.runID, failedRuns.contains(runID) else { return nil }
            return runID
        })
        return Set(messages.flatMap(\.runInputs).compactMap { input in
            input.targetRunID.map(withOutput.contains) == true ? input.runInputID : nil
        })
    }
    /// Failed inputs whose engine is signed out. Core refuses the resend, and
    /// the sign-in that unblocks it is the auth card's to offer, so the row
    /// shows no Send Again until the engine list says otherwise.
    var unsendableInputIDs: Set<ConversationRunInputID> = []
    /// Reference identity: the same `ChatPendingPermission` across two
    /// snapshots is the same card.
    var pendingPermissions: [ChatPendingPermission] = []
    /// Servers the agent could not reach for want of a sign-in, since the
    /// user last wrote.
    var mcpSignIns: [ChatMcpSignIn] = []
    /// The conversation's working root, which a markdown image destination
    /// written as a bare path resolves against and may not escape. Nil until
    /// the detail loads — the same pass that brings the messages brings this.
    var resourceRoot: URL?
    /// This conversation's delegated children by delegation key, oldest
    /// first, so a `delegate` call can show and open the child it started.
    /// An agent that delegates the same prompt twice gets two children under
    /// one key; its calls take them in order.
    var delegatedChildren: [String: [ChatDelegatedChild]] = [:]
    /// The conversation that delegated this one, for its Task header.
    var delegationParent: ConversationSummary?
    /// This conversation's tile, when it was delegated.
    var delegatedAvatar: AgentAvatar?
    /// A delegated child's turn whose report waits in its parent without
    /// waking it: that turn offers Send to Parent.
    var deferredReportRunID: ConversationRunID?
    /// Agents' display names, for "Started by Claude Code".
    var engineNames: [EngineID: String] = [:]
    /// This conversation's agent, for "Stopped by Claude Code".
    var agentName: String?

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.conversationID == rhs.conversationID
            && lhs.subagentThreadScope == rhs.subagentThreadScope
            && lhs.messages == rhs.messages
            && lhs.liveRun == rhs.liveRun
            && lhs.runStatuses == rhs.runStatuses
            && lhs.runFailures == rhs.runFailures
            && lhs.retryableFailedRunID == rhs.retryableFailedRunID
            && lhs.runUsage == rhs.runUsage
            && lhs.autonomousRunIDs == rhs.autonomousRunIDs
            && lhs.engineSwitches == rhs.engineSwitches
            && lhs.backgroundTasks == rhs.backgroundTasks
            && lhs.subagentThreads == rhs.subagentThreads
            && lhs.normalizedItems == rhs.normalizedItems
            && lhs.normalizedBodies == rhs.normalizedBodies
            && lhs.bodyErrors == rhs.bodyErrors
            && lhs.captureProblems == rhs.captureProblems
            && lhs.captureGaps == rhs.captureGaps
            && lhs.sessionStates == rhs.sessionStates
            && lhs.isMutating == rhs.isMutating
            && lhs.isRefreshingEngines == rhs.isRefreshingEngines
            && lhs.runtimeInstall == rhs.runtimeInstall
            && lhs.unsendableInputIDs == rhs.unsendableInputIDs
            && lhs.resourceRoot == rhs.resourceRoot
            && lhs.mcpSignIns == rhs.mcpSignIns
            && lhs.delegatedChildren == rhs.delegatedChildren
            && lhs.delegationParent == rhs.delegationParent
            && lhs.delegatedAvatar == rhs.delegatedAvatar
            && lhs.deferredReportRunID == rhs.deferredReportRunID
            && lhs.agentName == rhs.agentName
            && lhs.engineNames == rhs.engineNames
            && lhs.pendingPermissions.count == rhs.pendingPermissions.count
            && zip(lhs.pendingPermissions, rhs.pendingPermissions).allSatisfy { $0 === $1 }
    }
}

extension ChatWorkspaceModel {
    func transcriptSnapshot(runtimeInstall: ManagedRuntimeInstall?) -> ChatTranscriptSnapshot {
        // One pass over the run inputs; a per-message filter is quadratic on
        // every snapshot.
        let runInputsByMessage = Dictionary(
            grouping: selectedDetail?.latestRunInputPage.runInputs ?? [], by: \.messageID
        )
        let loadedItems = transcriptStore.orderedItems
        let engineNames = Dictionary(
            engines.map { ($0.engineID, $0.displayName) }, uniquingKeysWith: { first, _ in first }
        )
        let loadedIDs = Set(loadedItems.map(\.itemID))
        let latestPosition = loadedItems.last?.position
        // Lifecycle detail has its own Message-only page. Do not prepend those
        // older inputs while their intervening transcript items remain unloaded.
        // A newly admitted Message may arrive in its RPC response before its delta.
        let admittedMessages = (selectedDetail?.messages ?? []).filter { message in
            loadedIDs.contains(message.messageID.rawValue)
                || latestPosition.map { message.position > $0 } ?? true
        }
        var messageByID = Dictionary(admittedMessages.map { ($0.messageID, $0) }, uniquingKeysWith: { _, latest in latest })
        for item in transcriptStore.orderedItems {
            if var message = item.message {
                message.body = transcriptStore.bodies[item.itemID] ?? item.bodyPreview
                // A note's item streams in when it is posted; that a turn
                // carried it reaches the app only in a reloaded detail.
                message.noteDeliveredAt = message.noteDeliveredAt ?? messageByID[message.messageID]?.noteDeliveredAt
                messageByID[message.messageID] = message
            }
        }
        let messages = messageByID.values.sorted { $0.position < $1.position }.map { message in
            let auth = authRecovery(for: message)
            return ChatTranscriptSnapshot.Message(
                message: message,
                runInputs: runInputsByMessage[message.messageID] ?? [],
                authRecovery: auth,
                authSignInFailure: auth.map { signIn.failure(for: $0.agentID) } ?? nil,
                authTerminalLogin: auth.map { signIn.isTerminalLogin($0.agentID) } ?? false,
                runtimeRecovery: runtimeRecovery(for: message),
                selectionRecovery: selectionRecovery(for: message)
            )
        }
        return ChatTranscriptSnapshot(
            conversationID: selectedConversationID,
            messages: messages,
            liveRun: selectedCanonicalLiveRun,
            runStatuses: Dictionary(
                (selectedDetail?.runs ?? []).map { ($0.runID, $0.status) },
                uniquingKeysWith: { first, _ in first }
            ),
            runFailures: selectedRunFailures,
            retryableFailedRunID: retryableFailedRunID,
            runUsage: Dictionary(
                (selectedDetail?.runs ?? []).compactMap { run in ChatTurnUsage(run).map { (run.runID, $0) } },
                uniquingKeysWith: { first, _ in first }
            ),
            autonomousRunIDs: selectedAutonomousRunIDs,
            engineSwitches: ChatTranscriptDivider.engineSwitches(
                in: selectedDetail?.runs ?? [], engineNames: engineNames
            ),
            backgroundTasks: transcriptStore.backgroundTasks,
            subagentThreads: transcriptStore.subagentThreads,
            normalizedItems: transcriptStore.orderedItems,
            normalizedBodies: transcriptStore.bodies,
            bodyErrors: transcriptStore.bodyErrors,
            captureProblems: transcriptStore.captureProblems,
            captureGaps: transcriptStore.captureGaps,
            sessionStates: transcriptStore.sessionStates,
            isMutating: isMutating,
            isRefreshingEngines: isRefreshingEngines,
            runtimeInstall: runtimeInstall,
            unsendableInputIDs: unsendableInputIDs(in: selectedDetail),
            pendingPermissions: selectedPendingPermissions,
            mcpSignIns: selectedConversationID.flatMap { mcpSignIn?.state.cards(for: $0) } ?? [],
            resourceRoot: selectedDetail.map {
                URL(fileURLWithPath: $0.summary.workingRoot.canonicalPath)
            },
            delegatedChildren: delegatedChildren(),
            delegationParent: selectedDetail?.summary.parentConversationID.flatMap { parent in
                conversations.first { $0.conversationID == parent }
            },
            delegatedAvatar: selectedDetail?.summary.delegatedAvatar,
            deferredReportRunID: selectedConversationID.flatMap { selected in
                conversations.first { $0.conversationID == selected }?.deferredReportRunID
            },
            engineNames: engineNames,
            agentName: selectedConversationID.flatMap { conversationAgentNames[$0] }
        )
    }

    private func delegatedChildren() -> [String: [ChatDelegatedChild]] {
        guard let selectedConversationID else { return [:] }
        let running = runningConversationIDs
        let attention = attentionConversationIDs
        let children = conversations
            .filter { $0.parentConversationID == selectedConversationID && $0.delegationKey != nil }
            .sorted { ($0.createdAt, $0.conversationID.rawValue) < ($1.createdAt, $1.conversationID.rawValue) }
        return Dictionary(grouping: children) { $0.delegationKey ?? "" }.mapValues { children in
            children.map {
                ChatDelegatedChild(
                    $0,
                    isRunning: running.contains($0.conversationID),
                    needsApproval: attention.contains($0.conversationID)
                )
            }
        }
    }

    /// The Subagent tab's document: one thread's rows, with no messages,
    /// turns, or prompts. The permission card stays in the main transcript.
    func subagentTranscriptSnapshot() -> ChatTranscriptSnapshot {
        ChatTranscriptSnapshot(
            conversationID: subagentTranscript.conversationID,
            subagentThreadScope: subagentTranscript.threadScope,
            normalizedItems: subagentTranscript.orderedItems,
            normalizedBodies: subagentTranscript.bodies,
            bodyErrors: subagentTranscript.bodyErrors,
            resourceRoot: selectedDetail.map {
                URL(fileURLWithPath: $0.summary.workingRoot.canonicalPath)
            }
        )
    }

    /// Failed inputs whose run's engine is currently signed out: a resend would
    /// fail the same way a moment later, and Core refuses it outright.
    private func unsendableInputIDs(in detail: ConversationDetail?) -> Set<ConversationRunInputID> {
        guard let detail else { return [] }
        let signedOut = Set(
            engines.lazy
                .filter { $0.authState == .missing || $0.authState == .expired }
                .map(\.engineID)
        )
        guard !signedOut.isEmpty else { return [] }
        let engineByRun = Dictionary(
            detail.runs.map { ($0.runID, $0.engineID) }, uniquingKeysWith: { first, _ in first }
        )
        return Set(
            detail.latestRunInputPage.runInputs.lazy
                .filter { input in
                    input.status == .failed
                        && input.targetRunID.flatMap { engineByRun[$0] }.map(signedOut.contains) == true
                }
                .map(\.runInputID)
        )
    }
}
