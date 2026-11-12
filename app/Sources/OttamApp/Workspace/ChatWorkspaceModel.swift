import OttamBackendClient
import OttamCore
import OttamPresentationLogic
import OttamWorkspaceLogic
import Foundation
import Observation
import OSLog

/// Builds a descriptive activity label from a tool call's arguments. An
/// adapter may title calls with the bare tool name ("read", "bash") and
/// stream the arguments through title-less updates, so without this their
/// timelines never say *what* was read or run the way claude/codex titles do.
func chatDerivedToolCallLabel(payload: JSONValue, kind: String?) -> String? {
    let rawInput = payload.objectValue("rawInput")
    if let command = rawInput?.objectString("command") {
        let firstLine = command
            .split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: true)
            .first
            .map(String.init)?
            .trimmingCharacters(in: .whitespaces)
        if let firstLine, !firstLine.isEmpty {
            return firstLine.count > 80 ? String(firstLine.prefix(79)) + "…" : firstLine
        }
    }
    let path = rawInput?.objectString("path")
        ?? rawInput?.objectString("file_path")
        ?? payload.objectValue("locations")?.arrayFirst?.objectString("path")
    if let path, !path.isEmpty {
        // Component-aware abbreviation: a raw hasPrefix check would turn a
        // sibling like /Users/meg into ~g for home /Users/me.
        let display = (path as NSString).abbreviatingWithTildeInPath
        switch kind {
        case "read": return "Read \(display)"
        case "edit": return "Edit \(display)"
        case "delete": return "Delete \(display)"
        case "move": return "Move \(display)"
        default: return display
        }
    }
    if let pattern = rawInput?.objectString("pattern") ?? rawInput?.objectString("query") {
        return "Search: \(pattern)"
    }
    if let url = rawInput?.objectString("url") {
        return "Fetch \(url)"
    }
    return nil
}

/// A subagent's work, anchored at the call that launched it. Core groups the
/// rows; the app never joins on the adapter's parent link.
struct ChatSubagentThread: Equatable {
    var id: String
    /// The agent type the parent asked for, such as `Explore`. A native
    /// subagent has none: its name is its `task`.
    var name: String?
    /// The parent's short description of the work.
    var task: String?
    /// Absent when the launching call is not loaded, so nothing says how the
    /// subagent is doing.
    var state: SubagentThreadState?
    /// The subagent's calls in Core order. A nested subagent is one member
    /// that owns its own thread.
    var members: [ChatLiveActivity]
    /// The launch's result is the subagent's final answer; an asynchronous
    /// launch returns only a receipt.
    var hasFinalAnswer = false

    var stateLabel: String? {
        switch state {
        case .running: "Running"
        case .completed: "Completed"
        case .failed: "Failed"
        case .cancelled: "Cancelled"
        case .disconnected: "Disconnected"
        case nil: nil
        }
    }

    /// The newest call, however deep, for the collapsed running headline.
    var latestMember: ChatLiveActivity? {
        guard let last = members.last else { return nil }
        return last.thread?.latestMember ?? last
    }
}

struct ChatLiveActivity: Identifiable, Equatable {
    /// Core's local transcript identity preserves disclosure state across revisions.
    var id = ""
    var label: String
    /// The MCP tool a call ran ("server · tool"), dimmed after its title.
    var subtitle: String? = nil
    var detail: String?
    var kind: String?
    var toolCallID: String?
    var filePath: String? = nil
    var payloads: [TranscriptPayload] = []
    var revision: UInt64 = 0
    var bodyByteCount: UInt64 = 0
    /// The run whose tool this is; set for a tool with a background task,
    /// which Stop addresses through it.
    var runID: ConversationRunID? = nil
    /// The work this tool handed off to run in the background.
    var backgroundTask: TranscriptBackgroundTask? = nil
    /// The subagent this call launched, with the calls it made.
    var thread: ChatSubagentThread? = nil
    /// The form this call asked the user, and what became of it.
    var question: TranscriptQuestion? = nil
    /// A "Received message" row's note; `label` is the sender's title and
    /// `detail` the message it sent.
    var note: DelegationNote? = nil
    /// The conversation a "Received message" row's title opens: the child,
    /// or for a parent's message the parent.
    var noteSender: ConversationID? = nil
    /// This conversation's agent, for a note's "stopped by Claude Code".
    var noteAgent: String? = nil
    /// The tile of the child a "Received message" row names.
    var noteAvatar: AgentAvatar? = nil
    /// The parent read this note and had nothing to add.
    var noteAcknowledged = false

    /// The activity and every activity inside its threads, outermost first.
    var flattened: [ChatLiveActivity] {
        [self] + (thread?.members.flatMap(\.flattened) ?? [])
    }

    /// Running or paused: the agent can still stop it.
    var canStopBackgroundTask: Bool {
        guard let task = backgroundTask, task.id != nil, runID != nil else { return false }
        return task.state == .running || task.state == .paused
    }

    var captureWarning: String? {
        guard payloads.contains(where: { $0.locallyTruncated }) else { return nil }
        return payloads.contains(where: { $0.locallyTruncated && ["partial", "available"].contains($0.availability) })
            ? "Partial" : "Details unavailable"
    }

    var captureWarningHelp: String {
        var reasons: [String] = []
        for payload in payloads where payload.locallyTruncated {
            let reason = Self.captureReason(payload.reason)
            if !reasons.contains(reason) { reasons.append(reason) }
        }
        return reasons.joined(separator: "\n")
    }

    static func captureReason(_ reason: String?) -> String {
        switch reason {
        case "bytesUnavailable": return "The agent supplied a reference without image data."
        case "invalidImage": return "The agent supplied invalid or unsupported image data."
        case "unfinishedOutput": return "This output was unfinished and was not saved."
        case "captureLimit": return "This detail exceeded a capture limit. Only the retained content is available."
        case "storageFull": return "Storage capacity was exhausted while capturing this detail."
        case "queueFull": return "The pending capture queue was full, so some details could not be saved."
        case "writeFailed": return "A storage write failed while saving this detail."
        case "unsupportedContent": return "The agent supplied content in a format that could not be captured."
        default: return "Some details could not be saved. No specific capture reason is available."
        }
    }

    var hoverText: String {
        if let filePath { return filePath }
        if kind == "think" { return "Reasoning shared by the agent" }
        if kind == "streamGap" || kind == "transcriptError" { return detail ?? label }
        if note != nil { return "Received message from " + label }
        return subtitle.map { label + "\n" + $0 } ?? label
    }

    /// After "· " on a "Received message" row: how the child's turn ended,
    /// when not on its own, and whether the parent acknowledged it.
    var receivedOutcome: String? {
        guard let note else { return nil }
        let ended: String? = switch note.state {
        case .completed: nil
        case .failed: "failed"
        case .stopped: note.stopCause.map { "stopped \($0.phrase(agent: noteAgent))" } ?? "stopped"
        }
        let parts = [ended, noteAcknowledged ? "acknowledged" : nil].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// Codex-style summary fragment ("read files", "ran commands") for the group line.
    var summaryPhrase: String {
        switch kind {
        case "read": return "read files"
        case "edit": return "requested edits"
        case "delete": return "requested deletions"
        case "move": return "requested moves"
        case "search": return "searched"
        case "execute": return "ran commands"
        case "fetch": return "fetched pages"
        case "think": return "thought"
        case "plan": return "planned"
        case "streamGap": return "dropped updates"
        case "transcriptError": return label
        case "receivedMessage": return "received messages"
        default: return "used tools"
        }
    }
}

/// What was decided about one tool the agent asked permission to use, kept in the
/// transcript after the card that asked is gone. An approval is a decision the
/// user made about their own machine; it should still be there tomorrow.
struct ChatPermissionRecord: Equatable {
    /// The tool call the request was about, phrased like an activity label.
    var title: String
    /// The option the user picked ("Allow once"), or why there was no pick.
    var outcomeLabel: String
    var wasAllowed: Bool
    /// The request payload, shown under the disclosure — same JSON the live card
    /// offers under "Request details".
    var detailJSON: String?
    /// The plan's markdown when the request approved or declined a plan.
    var plan: String?

    /// The bold part of the row: what happened.
    var headline: String {
        guard plan != nil else { return outcomeLabel }
        if wasAllowed { return "Plan approved" }
        // Core's label when the request ended with no option picked.
        return outcomeLabel == "Cancelled" ? "Plan cancelled" : "Plan sent back for feedback"
    }

    /// The quieter part: the tool, or for an approved plan the way ahead
    /// the user picked.
    var subject: String? {
        guard plan != nil else { return title }
        return wasAllowed ? outcomeLabel : nil
    }

    var chatPlan: ChatPlan? { plan.map { ChatPlan(title: title, markdown: $0) } }
}

/// Failures raised by the submit flow before any run starts.
enum ChatSubmitError: LocalizedError {
    /// `conversation.create` accepted the message but returned no pending
    /// RunInput to hand to `startRun`.
    case missingInitialInput
    case noEngine
    case noWorkspace

    var errorDescription: String? {
        switch self {
        case .missingInitialInput:
            return "The conversation was created, but its first message has no pending input to run."
        case .noEngine:
            return "No Chat engine is available."
        case .noWorkspace:
            return "No workspace is open."
        }
    }
}

/// A run that stopped for want of credentials, and the engine to sign in to.
///
/// The card outlives the sign-in it offers. Signing in fixes the engine but not
/// the turn — the message still never reached the agent — so the same card
/// carries the whole recovery: sign in, watch it, then send the message again.
struct ChatAuthRecovery: Equatable {
    /// What the card is currently asking of the user.
    typealias Phase = ChatAuthRecoveryPhase

    let engine: EngineSummary
    let phase: Phase
    /// The input the failed run never delivered. Absent only if the run failed
    /// with no input recorded against it, in which case there is nothing to
    /// re-send and the card is a sign-in offer alone.
    let undeliveredInput: ConversationRunInput?

    /// Engines and agents are the same thing on the wire for harness engines;
    /// the two id types are the seam between the Chat surface and the ACP one.
    var agentID: AgentID {
        AgentID(engine.engineID.rawValue)
    }

    /// Credentials that worked earlier in this generation and then lapsed.
    /// Worth distinguishing: "Sign in again" reads very differently from
    /// "Sign in" to someone who thought they already had.
    var hasLapsed: Bool {
        presentation.hasLapsed
    }

    var canSendAgain: Bool {
        presentation.canSendAgain
    }

    var presentation: ChatAuthRecoveryPresentation {
        ChatAuthRecoveryPresentation(
            phase: phase,
            authState: engine.authState,
            hasUndeliveredInput: undeliveredInput != nil
        )
    }
}

/// The offer shown when a run failed because the Node runtime behind its engine
/// is unusable. The parallel of `ChatAuthRecovery` for the other reason an
/// engine cannot start: nothing is wrong with the conversation, and the fix is
/// outside the app.
struct ChatRuntimeRecovery: Equatable {
    let engine: EngineSummary
    let problem: EngineRuntimeProblem
    /// The wire message from the failed run, kept as secondary detail — it
    /// names the agent and component precisely where the card generalizes.
    let detail: String?
}

/// The offer shown when a run was refused because its model, permission mode
/// or effort is one the agent no longer offers. The handshake that refused it
/// also refreshed the engine list, so the composer already holds valid picks:
/// the card names what went stale and resends with the current ones.
struct ChatSelectionRecovery: Equatable {
    enum Setting: Equatable {
        case model, mode, thoughtLevel
    }

    let engine: EngineSummary
    /// Nil when the refreshed list still offers every value the run recorded,
    /// so the stale one cannot be named; the card falls back to `detail`.
    let setting: Setting?
    /// The id the run asked for and the agent refused.
    let staleValue: String?
    /// What a resend runs with in place of `staleValue`.
    let replacement: String
    /// The wire message from the failed run.
    let detail: String?
    let undeliveredInput: ConversationRunInput?
}

/// The headline a permission card and a permission record both show for the tool
/// under review: the call's own title, or the most identifying argument it has.
func chatPermissionToolSummary(_ toolCall: JSONValue) -> String {
    func nonEmpty(_ key: String) -> String? {
        toolCall.objectString(key).flatMap { $0.isEmpty ? nil : $0 }
    }
    // The same precedence the activity timeline uses: a title with whitespace is a
    // real description, a one-word title is just the tool name, and the call's
    // arguments say more than that. codex-acp sends no title at all and carries the
    // command in `rawInput`, so without the derived label the card asks you to
    // approve "a tool" — which is not a question anyone can answer.
    let title = nonEmpty("title")
    let descriptiveTitle = title.flatMap { $0.contains(" ") ? $0 : nil }
    return descriptiveTitle
        ?? chatDerivedToolCallLabel(payload: toolCall, kind: nonEmpty("kind"))
        ?? title
        ?? nonEmpty("command")
        ?? nonEmpty("path")
        ?? nonEmpty("name")
        ?? "The agent wants to use a tool."
}

/// A conversation's commands and the engine whose session reported them.
struct ChatReportedCommands: Equatable {
    var engineID: EngineID
    var commands: [ConversationSessionCommand]
}

struct ChatLiveRunState: Identifiable, Equatable {
    var id: ConversationRunID { runID }
    var runID: ConversationRunID
    var conversationID: ConversationID
    var status: ConversationRunStatus
    var startedAt: String?
    /// An autonomous run is the agent continuing on its own; a message sent
    /// during it takes over rather than waiting behind it.
    var origin: RunOrigin = .user
}

@MainActor
final class ChatPendingPermission: Identifiable {
    let id: UUID
    let request: AgentRequestPermissionParams
    private var continuation: CheckedContinuation<AgentPermissionOutcome, Never>?

    init(
        id: UUID = UUID(),
        request: AgentRequestPermissionParams,
        continuation: CheckedContinuation<AgentPermissionOutcome, Never>
    ) {
        self.id = id
        self.request = request
        self.continuation = continuation
    }

    func resolve(_ outcome: AgentPermissionOutcome) {
        continuation?.resume(returning: outcome)
        continuation = nil
    }

    #if DEBUG
    /// Lab/preview-only: a pending permission with nothing awaiting it, so the
    /// card can render outside a live run (markdown engine card spike).
    init(previewRequest: AgentRequestPermissionParams) {
        id = UUID()
        request = previewRequest
        continuation = nil
    }
    #endif
}

@MainActor
@Observable
final class ChatWorkspaceModel {
    private static let selectedEngineStorageKey = "chat.selectedEngineID"
    private static func selectedModelStorageKey(for engineID: EngineID) -> String {
        "chat.selectedModelID.\(engineID.rawValue)"
    }
    private static func selectedModeStorageKey(for engineID: EngineID) -> String {
        "chat.selectedModeID.\(engineID.rawValue)"
    }
    private static func selectedThoughtLevelStorageKey(for engineID: EngineID) -> String {
        "chat.selectedThoughtLevelID.\(engineID.rawValue)"
    }
    nonisolated private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "Chat"
    )

    private(set) var projectSummary: ResolvedProject?
    private(set) var workspaceID: WorkspaceID?
    private(set) var workspaceFiles: WorkingRootSummary?
    private(set) var projects: [WorkspaceProjectSummary] = []
    var draftWorkingRoot: WorkingRootSelection = .workspaceFiles
    /// Advances whenever the composer-first landing starts a logically new
    /// draft without necessarily leaving the Chat route.
    private(set) var draftGeneration = 0
    private(set) var engines: [EngineSummary] = []
    private var uninstalledEngineIDs: Set<EngineID> = []
    var pickerEngines: [EngineSummary] {
        engines.filter { !uninstalledEngineIDs.contains($0.engineID) }
    }
    /// Sign-in belongs to the workspace rather than to any one view: the
    /// failed-run card and the engine picker are two doors onto the same flow,
    /// and core allows only one of it per agent.
    let signIn = AgentSignInController()
    /// Remote MCP servers' sign-ins, the app's; their cards hang at the end
    /// of the chat whose agent called a signed-out server.
    @ObservationIgnored weak var mcpSignIn: McpSignInController?
    private(set) var conversations: [ConversationSummary] = []
    /// Called after the user deletes a conversation, so the window can close
    /// what belongs to it.
    @ObservationIgnored var didDeleteConversation: (ConversationID) -> Void = { _ in }
    /// A subagent card was clicked: show its thread in the Subagent tab.
    @ObservationIgnored var openSubagentThread: (ConversationID, String) -> Void = { _, _ in }
    /// A plan asks for approval, or Open plan was pressed: show it in the
    /// Plan tab.
    @ObservationIgnored var openPlan: (ConversationID, ChatPlan) -> Void = { _, _ in }
    /// A link or Read row was clicked: a markdown file opens in its tab.
    @ObservationIgnored var openFile: (ConversationID, URL) -> Void = { _, _ in }
    /// Bumped to put the caret in the composer, as Revise does so the user
    /// can type what to change.
    private(set) var composerFocusRequest = 0

    func focusComposer() {
        composerFocusRequest &+= 1
    }
    private(set) var selectedDetail: ConversationDetail? {
        didSet { recordAgentNames() }
    }
    /// Conversations' agents by display name, as far as loaded details
    /// say: a conversation's latest turn, and a child's task names its
    /// parent's. For "Stopped by Claude Code" and the agents pill.
    private(set) var conversationAgentNames: [ConversationID: String] = [:]
    /// Their brand keys, as far as the same details say, for the pill's
    /// brand tiles.
    private(set) var conversationAgentBrands: [ConversationID: String] = [:]
    /// True while a newly selected conversation's detail is in flight. Switching
    /// sessions clears the outgoing thread immediately and shows a loading state,
    /// then the loaded content applies in a single flip.
    private(set) var isLoadingDetail = false
    private(set) var isLoading = false
    /// One successful full load (engines + conversations) per workspace; the
    /// window's first-paint gate holds on this so the whole surface lands at once.
    private(set) var didCompleteInitialLoad = false
    private(set) var isSubmitting = false
    private(set) var isMutating = false
    /// Not `isMutating`: that flag also greys resend affordances on every
    /// message row, and a background engine refresh should only hold the
    /// button that started it.
    private(set) var isRefreshingEngines = false
    private(set) var isLoadingOlderMessages = false
    private(set) var errorText: String?
    var sessionNoticeText: String? {
        guard selectedPendingPermissions.isEmpty,
              let run = selectedLiveRun?.runID ?? selectedDetail?.runs.max(by: { $0.startedAt < $1.startedAt })?.runID,
              let state = transcriptStore.sessionStates[run] else { return nil }
        if let failure = state.signal.failure, failure.severity == "error" {
            return (state.live ? "" : "Last reported: ") + failure.message
        }
        if let limit = state.signal.limit, limit.status == "rejected" || limit.status == "allowed_warning" {
            var message = limit.status == "rejected" ? "Agent usage limit reached" : "Agent usage is nearing its limit"
            if let reset = limit.resetsAt {
                message += "; reported reset " + Date(timeIntervalSince1970: Double(reset)).formatted(date: .abbreviated, time: .shortened)
            }
            return (state.live ? "" : "Last reported: ") + message
        }
        return nil
    }
    private(set) var transcriptLoadError: String?
    private(set) var noticeText: String?
    private(set) var liveRuns: [ConversationRunID: ChatLiveRunState] = [:]
    /// The shown conversation's store. Leaving a conversation keeps its store
    /// in `transcriptCache`, merging deltas, so returning shows it at once.
    private(set) var transcriptStore = ChatTranscriptStore()
    let transcriptCache: ConversationTranscriptCache
    @ObservationIgnored private var memoryPressure: DispatchSourceMemoryPressure?
    @ObservationIgnored private var idleSweep: Task<Void, Never>?
    /// `selectedDetail` is a cached one not yet refreshed; leaving must not
    /// save it back as current.
    @ObservationIgnored private var showsCachedDetail = false
    /// The Subagent tab's rows: one thread of the selected conversation, and
    /// the threads nested in it.
    let subagentTranscript = ChatTranscriptStore()
    private(set) var subagentLoadError: String?
    /// The Subagent tab view the store loads for.
    @ObservationIgnored private var subagentOwner: UUID?
    private(set) var pendingPermissions: [ChatPendingPermission] = []
    /// Every agent form waiting on the user, across conversations, oldest first.
    private(set) var pendingQuestions: [ChatPendingQuestion] = []
    private var eventSubscription: AgentEventSubscriptionToken?
    private var engineRefreshSubscription: AgentEventSubscriptionToken?
    private var planUsageSubscription: AgentEventSubscriptionToken?
    @ObservationIgnored private var engineRefreshPending = false
    private var permissionRegistration: AgentPermissionRegistrationToken?
    @ObservationIgnored private var subscribedWorkspaceID: WorkspaceID?
    @ObservationIgnored private var terminalRunIDs: Set<ConversationRunID> = []
    @ObservationIgnored private var pendingStartConversationID: ConversationID?
    @ObservationIgnored private var lastEventSequenceByRun: [ConversationRunID: UInt64] = [:]
    @ObservationIgnored private var configureTask: Task<Void, Never>?
    @ObservationIgnored private var backendHandlersRetired = false
    @ObservationIgnored private var initialLoadTask: Task<Void, Never>?
    @ObservationIgnored private var initialLoadGeneration = 0
    @ObservationIgnored private var detailLoadTask: Task<Void, Never>?
    @ObservationIgnored private let defaults: UserDefaults
    /// Messages waiting for a working run to finish. Observed, so the composer's
    /// queue pill tracks it without a manual publish.
    private(set) var messageQueue = ChatMessageQueue()
    /// Conversations whose head is already on the wire, so a second terminal
    /// event for the same run cannot send it twice.
    @ObservationIgnored private var drainingConversationIDs: Set<ConversationID> = []

    /// Commands each conversation's agent reported since launch.
    private(set) var conversationCommands: [ConversationID: ChatReportedCommands] = [:]
    /// How full each chat's context was when its agent last reported, live
    /// or restored from the chat's session.
    private(set) var conversationContextUsage: [ConversationID: ConversationContextUsage] = [:]
    /// Each agent's last-known plan usage, account-wide rather than per chat.
    private(set) var planUsage: [EngineID: AgentPlanUsage] = [:]
    /// Changes announced per agent, so a slower read cannot overwrite one.
    @ObservationIgnored private var planUsageAnnouncements: [EngineID: Int] = [:]
    /// Agents whose popover refresh is waiting on core's probe.
    private(set) var refreshingPlanUsage: [EngineID: UUID] = [:]
    /// Longer than core's 15-second probe timeout, so a slow probe still
    /// lands while the spinner shows.
    static let planUsageRefreshTimeout: Duration = .seconds(20)
    /// The last commands each engine reported, saved so a conversation lists
    /// them before its agent session reports its own, also after a relaunch.
    private(set) var engineCommands: [EngineID: [ConversationSessionCommand]] = [:]
    /// The composer's command card is showing; a question or plan panel steps aside.
    var isCommandPickerOpen = false

    var selectedConversationID: ConversationID?
    var selectedEngineID: EngineID? {
        didSet {
            guard selectedEngineID != oldValue else { return }
            if let selectedEngineID {
                defaults.set(selectedEngineID.rawValue, forKey: Self.selectedEngineStorageKey)
            } else {
                defaults.removeObject(forKey: Self.selectedEngineStorageKey)
            }
            restoreEngineSelections()
        }
    }
    /// Adapter-defined model id for new harness sessions; nil means the
    /// engine's default. Persisted per engine so switching engines restores
    /// each engine's own choice.
    var selectedModelID: String? {
        didSet {
            guard selectedModelID != oldValue, let engineID = selectedEngineID else { return }
            let key = Self.selectedModelStorageKey(for: engineID)
            if let selectedModelID {
                defaults.set(selectedModelID, forKey: key)
            } else {
                defaults.removeObject(forKey: key)
            }
        }
    }
    /// Adapter-defined permission-mode id for new harness sessions; nil means
    /// the engine's default. Same persistence contract as `selectedModelID`.
    var selectedModeID: String? {
        didSet {
            guard selectedModeID != oldValue, let engineID = selectedEngineID else { return }
            let key = Self.selectedModeStorageKey(for: engineID)
            if let selectedModeID {
                defaults.set(selectedModeID, forKey: key)
            } else {
                defaults.removeObject(forKey: key)
            }
        }
    }
    /// The mode the agent last confirmed for each conversation's session: its
    /// acknowledgment of a request, or a change it made itself (leaving plan
    /// mode). The picker shows it in place of the new-chat default.
    private(set) var confirmedModeIDs: [ConversationID: String] = [:]
    /// A mode picked in an existing conversation that the agent has not
    /// confirmed yet: being sent to the live session, or, when no session is
    /// live, riding on the next message.
    private(set) var pendingModeIDs: [ConversationID: String] = [:]
    /// Each conversation's active AgentSession, the target of a live mode
    /// change. Core rejects a change aimed at a session that has since ended.
    private(set) var activeAgentSessionIDs: [ConversationID: AgentSessionID] = [:]
    /// Conversations with a mode change on its way to the agent. The rows stay
    /// disabled until it answers, so two changes cannot land out of order.
    private(set) var modeWriteConversationIDs: Set<ConversationID> = []
    /// Conversations whose pick waits for the next message because no session
    /// was live to take it.
    private(set) var deferredModeConversationIDs: Set<ConversationID> = []
    /// Why the agent refused the last mode change, per conversation.
    private(set) var modeErrorTexts: [ConversationID: String] = [:]

    /// The mode the next message requests: in an existing conversation the
    /// pending pick, else the agent's confirmed mode, else the new-chat
    /// default. A conversation mode the selected engine does not advertise
    /// (another engine's) falls through to the default.
    var effectiveModeID: String? { modeID(for: selectedConversationID) }

    /// `effectiveModeID` for a given conversation; nil is a draft.
    func modeID(for conversationID: ConversationID?) -> String? {
        guard let conversationID else { return selectedModeID }
        return conversationModeID(conversationID, advertisedBy: selectedEngine) ?? selectedModeID
    }

    /// The conversation's pending pick, else its confirmed mode, when `engine`
    /// advertises it.
    func conversationModeID(
        _ conversationID: ConversationID,
        advertisedBy engine: EngineSummary?
    ) -> String? {
        let advertised = (engine?.modes ?? []).map(\.id)
        return [pendingModeIDs[conversationID], confirmedModeIDs[conversationID]]
            .compactMap { $0 }
            .first { advertised.isEmpty || advertised.contains($0) }
    }

    /// Whether the picker's mode waits for the next message to take effect.
    var hasPendingMode: Bool {
        guard let conversationID = selectedConversationID else { return false }
        return pendingModeIDs[conversationID] != nil
            && deferredModeConversationIDs.contains(conversationID)
    }

    /// Whether a mode change for the conversation on screen awaits the agent.
    var isWritingMode: Bool {
        selectedConversationID.map { modeWriteConversationIDs.contains($0) } ?? false
    }

    /// Whether a mode change now reaches only the agent's next turn: a turn is
    /// running and the agent reads its mode when a turn starts (codex-acp).
    var modeChangeWaitsForNextTurn: Bool {
        guard selectedCanonicalLiveRun != nil, let engine = selectedEngine else { return false }
        return !engine.capabilities.contains(.liveModeChange)
    }

    var selectedModeErrorText: String? {
        selectedConversationID.flatMap { modeErrorTexts[$0] }
    }

    /// A mode pick. A new chat changes the per-engine default; an existing
    /// conversation keeps the default and marks the pick pending until
    /// `applyPendingMode` sends it. Its "Default" row names the engine's
    /// default mode explicitly, since the session may have left it.
    func selectMode(_ modeID: String?) {
        guard let conversationID = selectedConversationID else {
            selectedModeID = modeID
            return
        }
        guard !modeWriteConversationIDs.contains(conversationID) else { return }
        modeErrorTexts[conversationID] = nil
        let resolved = modeID ?? selectedEngine?.defaultModeID
        if resolved == nil || resolved == confirmedModeIDs[conversationID] {
            pendingModeIDs[conversationID] = nil
            deferredModeConversationIDs.remove(conversationID)
        } else {
            pendingModeIDs[conversationID] = resolved
        }
    }

    /// Record which AgentSession a live mode change targets; nil when the
    /// conversation has none active.
    func noteActiveAgentSession(_ agentSessionID: AgentSessionID?, conversationID: ConversationID) {
        activeAgentSessionIDs[conversationID] = agentSessionID
    }

    /// Record a mode the agent confirmed. A pending pick it matches is done.
    func confirmMode(_ modeID: String, conversationID: ConversationID) {
        confirmedModeIDs[conversationID] = modeID
        if pendingModeIDs[conversationID] == modeID {
            pendingModeIDs[conversationID] = nil
            deferredModeConversationIDs.remove(conversationID)
        }
    }

    func applyPendingMode(backend: DiffReviewModel) async {
        guard let workspaceID else { return }
        await applyPendingMode { conversationID, agentSessionID, modeID in
            try await backend.setConversationMode(ConversationSetModeParams(
                workspaceID: workspaceID,
                conversationID: conversationID,
                agentSessionID: agentSessionID,
                modeID: modeID
            ))
        }
    }

    /// Sends the selected conversation's pending pick to its live session.
    /// Applied confirms it now; deferred leaves it riding on the next message;
    /// a refusal drops it, keeps the confirmed mode, and says why.
    func applyPendingMode(
        send: (ConversationID, AgentSessionID?, String) async throws -> ConversationSetModeResult
    ) async {
        guard let conversationID = selectedConversationID,
              let modeID = pendingModeIDs[conversationID],
              !modeWriteConversationIDs.contains(conversationID)
        else { return }
        modeWriteConversationIDs.insert(conversationID)
        defer { modeWriteConversationIDs.remove(conversationID) }
        do {
            // A conversation started in this launch has no session in its
            // loaded detail yet; Core then targets its active session.
            let result = try await send(conversationID, activeAgentSessionIDs[conversationID], modeID)
            switch result.outcome {
            case .applied:
                confirmMode(result.currentModeID ?? modeID, conversationID: conversationID)
            case .deferred:
                deferredModeConversationIDs.insert(conversationID)
            }
        } catch AgentConnectionError.rpcError(_, _, let data?)
            where data.objectString("kind") == "staleAgentSession" {
            // The session changed under the picker; its replacement picks the
            // mode up from the next message.
            deferredModeConversationIDs.insert(conversationID)
        } catch {
            pendingModeIDs[conversationID] = nil
            deferredModeConversationIDs.remove(conversationID)
            modeErrorTexts[conversationID] = error.localizedDescription
        }
    }

    /// The always-allow value a new chat starts from: the one last set in any
    /// conversation. Core copies it into the conversation it creates.
    private(set) var draftAlwaysAllowToolActions = false
    @ObservationIgnored private var alwaysAllowWrite: Task<Void, Never>?
    /// Counts `conversation.listChanged` re-lists, so only the latest applies.
    @ObservationIgnored private var listChangedGeneration = 0
    /// Re-lists when a named agent changes in Settings, for its children's
    /// tiles.
    @ObservationIgnored private var namedAgentsObserver: Task<Void, Never>?

    /// Whether the selected agent has no bypass mode of its own, so Ottam
    /// answers its permission requests when the chat always allows them. A
    /// delegated child has no bypass mode to pick, so Ottam answers for
    /// any agent there.
    var selectedEngineSupportsAlwaysAllow: Bool {
        if let selectedConversationID,
           conversations.first(where: { $0.conversationID == selectedConversationID })?.parentConversationID != nil {
            return true
        }
        return selectedEngine?.capabilities.contains(.alwaysAllowToolActions) ?? false
    }

    /// The selected chat's value; a draft, or a chat the list has not caught
    /// up with yet, shows the value new chats start from.
    var alwaysAllowToolActions: Bool {
        guard let conversationID = selectedConversationID,
              let summary = conversations.first(where: { $0.conversationID == conversationID })
        else { return draftAlwaysAllowToolActions }
        return summary.alwaysAllowToolActions
    }

    func loadToolPermissions(backend: DiffReviewModel) async {
        do {
            draftAlwaysAllowToolActions = try await backend.toolPermissions().alwaysAllowToolActions
        } catch {
            Self.logger.error(
                "failed to read tool permissions: \(error.localizedDescription, privacy: .private)"
            )
        }
    }

    /// Starts the write at once rather than from a detached task, so a send
    /// made right after the switch moves waits for it.
    @discardableResult
    func setAlwaysAllowToolActions(_ enabled: Bool, backend: DiffReviewModel) -> Task<Void, Never>? {
        guard let workspaceID else { return nil }
        return setAlwaysAllowToolActions(enabled) { conversationID, enabled in
            guard let conversationID else {
                return try await backend.setToolPermissions(
                    ToolPermissionsState(alwaysAllowToolActions: enabled)
                )
            }
            return try await backend.setConversationAlwaysAllowToolActions(
                ConversationSetAlwaysAllowToolActionsParams(
                    workspaceID: workspaceID,
                    conversationID: conversationID,
                    enabled: enabled
                )
            )
        }
    }

    /// Waits for the switch's pending writes. Every path that creates a chat
    /// or starts a run waits here, after pinning what it sends, so core reads
    /// the value the switch shows. A steer need not: a running turn reads it
    /// on each request.
    func settleAlwaysAllowToolActions() async {
        await alwaysAllowWrite?.value
    }

    /// A draft writes the default; a chat writes its own value, which core
    /// also makes the default. Either way the draft follows. Writes run in
    /// order, each after the one before.
    @discardableResult
    func setAlwaysAllowToolActions(
        _ enabled: Bool,
        send: @escaping @MainActor (ConversationID?, Bool) async throws -> ToolPermissionsState
    ) -> Task<Void, Never> {
        let conversationID = selectedConversationID
        let previous = alwaysAllowWrite
        let write = Task { [weak self] in
            await previous?.value
            await self?.writeAlwaysAllowToolActions(enabled, conversationID: conversationID, send: send)
        }
        alwaysAllowWrite = write
        return write
    }

    private func writeAlwaysAllowToolActions(
        _ enabled: Bool,
        conversationID: ConversationID?,
        send: @MainActor (ConversationID?, Bool) async throws -> ToolPermissionsState
    ) async {
        do {
            let enabled = try await send(conversationID, enabled).alwaysAllowToolActions
            draftAlwaysAllowToolActions = enabled
            if let conversationID,
               let index = conversations.firstIndex(where: { $0.conversationID == conversationID }) {
                conversations[index].alwaysAllowToolActions = enabled
            }
        } catch {
            Self.logger.error(
                "failed to set always allow tool actions: \(error.localizedDescription, privacy: .private)"
            )
        }
    }

    /// Adapter-defined thought-level (reasoning effort) id for new harness
    /// sessions; nil means the engine's default.
    var selectedThoughtLevelID: String? {
        didSet {
            guard selectedThoughtLevelID != oldValue, let engineID = selectedEngineID else { return }
            let key = Self.selectedThoughtLevelStorageKey(for: engineID)
            if let selectedThoughtLevelID {
                defaults.set(selectedThoughtLevelID, forKey: key)
            } else {
                defaults.removeObject(forKey: key)
            }
        }
    }

    let starterPrompts = [
        "Ask what changed recently in this project.",
        "Plan the next implementation slice.",
        "Handle an ad-hoc task and show the evidence.",
        "Explore an open product or technical question."
    ]

    init(defaults: UserDefaults = .standard, transcriptCache: ConversationTranscriptCache? = nil) {
        self.defaults = defaults
        self.transcriptCache = transcriptCache ?? ConversationTranscriptCache()
        selectedEngineID = defaults.string(forKey: Self.selectedEngineStorageKey).map(EngineID.init)
        restoreEngineSelections()
        let pressure = DispatchSource.makeMemoryPressureSource(eventMask: [.warning, .critical], queue: .main)
        pressure.setEventHandler { [weak self] in
            MainActor.assumeIsolated { self?.relieveMemoryPressure() }
        }
        pressure.activate()
        memoryPressure = pressure
        idleSweep = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(300))
                guard let self else { return }
                self.evictTranscripts()
            }
        }
    }

    deinit { memoryPressure?.cancel(); idleSweep?.cancel(); namedAgentsObserver?.cancel() }

    // MARK: - Conversation cache

    /// The store a delta or run event for `conversationID` belongs to: the
    /// shown one, or a cached one.
    private func transcriptStore(for conversationID: ConversationID) -> ChatTranscriptStore? {
        transcriptStore.conversationID == conversationID
            ? transcriptStore
            : transcriptCache.session(for: conversationID)?.store
    }

    /// Shows `conversationID`'s cached store, or a new cached one. A draft
    /// store (no conversation yet) becomes the new conversation's.
    @discardableResult
    private func showTranscript(of conversationID: ConversationID) -> ConversationTranscriptSession {
        if transcriptStore.conversationID != conversationID { leaveTranscript() }
        let draft = transcriptStore.conversationID == nil || transcriptStore.conversationID == conversationID
        let session = transcriptCache.open(conversationID, adopting: draft ? transcriptStore : nil)
        if transcriptStore !== session.store { transcriptStore = session.store }
        evictTranscripts()
        return session
    }

    /// Puts the shown store in the background with the detail shown beside
    /// it. It keeps merging deltas but reads no bodies.
    private func leaveTranscript() {
        guard let conversationID = transcriptStore.conversationID else { return }
        if let detail = selectedDetail, detail.summary.conversationID == conversationID, !showsCachedDetail {
            transcriptCache.session(for: conversationID)?.detail = detail
        }
        transcriptStore.suspendBodies()
        transcriptCache.markIdle(conversationID)
    }

    /// Shows an empty store bound to no conversation: New Chat, or nothing.
    private func showDraftTranscript() {
        leaveTranscript()
        transcriptStore = ChatTranscriptStore()
    }

    private func evictTranscripts() {
        transcriptCache.evict(
            shown: transcriptStore.conversationID,
            running: Set(liveRuns.values.map(\.conversationID))
        )
    }

    private func relieveMemoryPressure() {
        Self.logger.info("memory pressure: dropping cached conversations")
        transcriptCache.removeSessions(keeping: transcriptStore.conversationID)
    }

    /// The connection dropped: no session is live in any store.
    func disconnectSessionActivity() {
        transcriptStore.disconnectSessionActivity()
        for id in transcriptCache.cachedIDs where id != transcriptStore.conversationID {
            transcriptCache.session(for: id)?.store.disconnectSessionActivity()
        }
    }

    /// Update lifecycle state independently of transcript content.
    private func withLiveRun(
        _ runID: ConversationRunID,
        conversationID: ConversationID,
        status: ConversationRunStatus = .running,
        _ body: (inout ChatLiveRunState) -> Void
    ) {
        var run = liveRuns[runID] ?? ChatLiveRunState(
            runID: runID,
            conversationID: conversationID,
            status: status,
            startedAt: persistedStartedAt(runID),
            origin: persistedOrigin(runID)
        )
        body(&run)
        liveRuns[runID] = run
    }

    var conversationCount: Int {
        conversations.count
    }

    /// The selected conversation's title from the list summary — available before the
    /// detail loads, so chrome tied to the selection doesn't blank out mid-switch.
    var selectedSummaryTitle: String? {
        guard let selectedConversationID else { return nil }
        return conversations.first { $0.conversationID == selectedConversationID }?.title
    }

    var selectedEngine: EngineSummary? {
        guard let selectedEngineID else { return engines.first }
        return engines.first { $0.engineID == selectedEngineID }
    }

    var selectedAgentRemoved: Bool {
        guard didCompleteInitialLoad, let engineID = selectedDetail?.runs.first?.engineID else { return false }
        return !engines.contains { $0.engineID == engineID }
    }

    var selectedProfileRemoved: Bool {
        guard didCompleteInitialLoad, let selectedEngineID else { return false }
        return !engines.contains { $0.engineID == selectedEngineID }
    }

    var visibleNoticeText: String? {
        if selectedAgentRemoved {
            return "This agent was removed. Saved conversation content remains available; start a new conversation to use another profile."
        }
        if selectedProfileRemoved { return "This agent was removed. Choose a profile to start a new conversation." }
        return noticeText
    }

    func selectEngine(_ engineID: EngineID, backend: DiffReviewModel) {
        guard !selectedAgentRemoved else { return }
        selectedEngineID = engineID
        Task { [weak self] in
            await backend.probeAgent(engineID: engineID)
            await self?.refreshEngines(backend: backend)
        }
    }

    var availableModels: [EngineModelOption] {
        selectedEngine?.models ?? []
    }

    var selectedModelLabel: String {
        guard let selectedModelID else { return defaultModelDisplayName ?? "Default" }
        return availableModels.first { $0.modelID == selectedModelID }?.displayName ?? selectedModelID
    }

    /// Display name of the model the engine actually runs when no explicit
    /// choice is made, resolved from the probed default; nil when unknown.
    var defaultModelDisplayName: String? {
        guard let engine = selectedEngine, let defaultID = engine.defaultModelID else { return nil }
        return engine.models.first { $0.modelID == defaultID }?.displayName ?? defaultID
    }

    /// The selected engine's commands: the selected conversation's session
    /// when it runs on that engine, else the last the engine reported; nil
    /// until it has reported any.
    var selectedCommands: [ConversationSessionCommand]? {
        guard let engineID = selectedEngineID else { return nil }
        if let conversationID = selectedConversationID,
           let reported = conversationCommands[conversationID],
           reported.engineID == engineID {
            return reported.commands
        }
        return engineCommands[engineID]
    }

    /// The selected chat's context gauge; nil for a draft and until its
    /// agent reports.
    var selectedContextUsage: ConversationContextUsage? {
        selectedConversationID.flatMap { conversationContextUsage[$0] }
    }

    /// The selected agent's subscription usage windows; nil for an agent
    /// without plan limits and until core first reads them.
    var selectedPlanUsage: AgentPlanUsage? {
        selectedEngineID.flatMap { planUsage[$0] }
    }

    /// Asks core for the agent's snapshot. A stale one answers now and the
    /// refresh lands on `agent.planUsageChanged`.
    func refreshPlanUsage(engineID: EngineID, backend: DiffReviewModel) async {
        let announced = planUsageAnnouncements[engineID, default: 0]
        let usage = await backend.planUsage(engineID: engineID)
        // A change announced while the read was in flight is newer than it.
        guard planUsageAnnouncements[engineID, default: 0] == announced else { return }
        recordPlanUsage(usage, engineID: engineID)
    }

    /// The popover's refresh: core probes now, and the spinner holds until
    /// the result is announced. A probe that changes nothing announces
    /// nothing, so the spinner also gives up after `planUsageRefreshTimeout`.
    func forceRefreshSelectedPlanUsage(backend: DiffReviewModel) async {
        guard let engineID = selectedEngineID,
              refreshingPlanUsage[engineID] == nil else { return }
        let token = UUID()
        refreshingPlanUsage[engineID] = token
        _ = await backend.planUsage(engineID: engineID, refresh: true)
        try? await Task.sleep(for: Self.planUsageRefreshTimeout)
        // A later refresh owns the spinner now.
        if refreshingPlanUsage[engineID] == token {
            refreshingPlanUsage[engineID] = nil
        }
    }

    var isRefreshingSelectedPlanUsage: Bool {
        selectedEngineID.map { refreshingPlanUsage[$0] != nil } ?? false
    }

    /// Whether a change is the probe's answer, which ends a refresh: its own
    /// reading, its failure, or its clear. A turn's live update arrives as
    /// `session` and leaves the spinner waiting on the probe.
    static func settlesProbe(_ change: AgentPlanUsageChangedParams) -> Bool {
        guard let usage = change.usage else { return true }
        return usage.source == .probe || usage.error != nil
    }

    /// Applies `agent.planUsageChanged`; a nil usage clears the meter.
    func applyPlanUsageChange(_ change: AgentPlanUsageChangedParams) {
        let engineID = EngineID(change.agentID.rawValue)
        planUsageAnnouncements[engineID, default: 0] += 1
        if Self.settlesProbe(change) {
            refreshingPlanUsage[engineID] = nil
        }
        recordPlanUsage(change.usage, engineID: engineID)
    }

    func recordPlanUsage(_ usage: AgentPlanUsage?, engineID: EngineID) {
        guard planUsage[engineID] != usage else { return }
        planUsage[engineID] = usage
    }

    /// Nil clears the gauge: the chat's active session hasn't reported.
    func recordContextUsage(_ usage: ConversationContextUsage?, conversationID: ConversationID) {
        guard conversationContextUsage[conversationID] != usage else { return }
        conversationContextUsage[conversationID] = usage
    }

    func recordCommands(
        _ commands: [ConversationSessionCommand],
        engineID: EngineID,
        conversationID: ConversationID
    ) {
        let reported = ChatReportedCommands(engineID: engineID, commands: commands)
        if conversationCommands[conversationID] != reported {
            conversationCommands[conversationID] = reported
        }
        guard engineCommands[engineID] != commands else { return }
        engineCommands[engineID] = commands
        if let data = try? JSONEncoder().encode(commands) {
            defaults.set(data, forKey: Self.commandsStorageKey(engineID))
        }
    }

    private static func commandsStorageKey(_ engineID: EngineID) -> String {
        "chat.commands.\(engineID.rawValue)"
    }

    var availableModes: [EngineSelectOption] {
        selectedEngine?.modes ?? []
    }

    var selectedModeLabel: String {
        guard let modeID = effectiveModeID else { return defaultModeDisplayName ?? "Default" }
        return availableModes.first { $0.id == modeID }?.displayName ?? modeID
    }

    var defaultModeDisplayName: String? {
        guard let engine = selectedEngine, let defaultID = engine.defaultModeID else { return nil }
        return engine.modes.first { $0.id == defaultID }?.displayName ?? defaultID
    }

    var availableThoughtLevels: [EngineSelectOption] {
        selectedEngine?.thoughtLevelSelect(forModel: selectedModelID).levels ?? []
    }

    /// The pick the selected model honors, which is what runs send and the
    /// picker shows. Effort levels belong to the model, so a pick it lacks is
    /// kept for the models that offer it rather than sent or erased.
    var effectiveThoughtLevelID: String? {
        guard let pick = selectedThoughtLevelID, selectedEngine != nil else { return selectedThoughtLevelID }
        return availableThoughtLevels.contains { $0.id == pick } ? pick : nil
    }

    /// The level the selected model runs at without a pick; nil when unknown.
    var defaultThoughtLevelID: String? {
        selectedEngine?.thoughtLevelSelect(forModel: selectedModelID).defaultID
    }

    var selectedThoughtLevelLabel: String {
        guard let pick = effectiveThoughtLevelID else { return defaultThoughtLevelDisplayName ?? "Default" }
        return availableThoughtLevels.first { $0.id == pick }?.displayName ?? pick
    }

    var defaultThoughtLevelDisplayName: String? {
        guard let defaultID = defaultThoughtLevelID else { return nil }
        return availableThoughtLevels.first { $0.id == defaultID }?.displayName ?? defaultID
    }

    /// Restores the persisted model/mode/thought-level choices for the current
    /// engine. Validation against the engine's advertised options only applies
    /// once engines have loaded — before that, the persisted values are trusted
    /// so the chip doesn't flash "Default" on launch. The thought-level pick is
    /// kept as is: `effectiveThoughtLevelID` gates it per model.
    private func restoreEngineSelections() {
        guard let selectedEngineID else {
            selectedModelID = nil
            selectedModeID = nil
            selectedThoughtLevelID = nil
            return
        }
        if engineCommands[selectedEngineID] == nil,
           let saved = defaults.data(forKey: Self.commandsStorageKey(selectedEngineID)),
           let commands = try? JSONDecoder().decode([ConversationSessionCommand].self, from: saved) {
            engineCommands[selectedEngineID] = commands
        }
        let engine = engines.first { $0.engineID == selectedEngineID }
        selectedModelID = Self.validated(
            defaults.string(forKey: Self.selectedModelStorageKey(for: selectedEngineID)),
            in: engine.map { $0.models.map(\.modelID) }
        )
        selectedModeID = Self.validated(
            defaults.string(forKey: Self.selectedModeStorageKey(for: selectedEngineID)),
            in: engine.map { $0.modes.map(\.id) }
        )
        selectedThoughtLevelID = defaults.string(forKey: Self.selectedThoughtLevelStorageKey(for: selectedEngineID))
    }

    /// A persisted id is trusted until the engine's advertised list is known
    /// and non-empty; after that, an id no longer advertised falls back to the
    /// engine default rather than sending a stale value to the adapter.
    private static func validated(_ persisted: String?, in advertised: [String]?) -> String? {
        guard let persisted else { return nil }
        guard let advertised, !advertised.isEmpty else { return persisted }
        return advertised.contains(persisted) ? persisted : nil
    }

    /// True until the initial load has fully landed (or failed): the composer's
    /// menus render as static stand-ins (mounting/rebuilding a live Menu mid-load
    /// flashes its popup window on macOS 26) and the window's first-paint
    /// overlay stays up.
    var isPreparingEngines: Bool {
        !didCompleteInitialLoad && errorText == nil
    }

    var canSubmit: Bool {
        !selectedAgentRemoved && composerPresentation.canSubmit
    }

    /// Whether the composer should stop pretending and ask for setup instead:
    /// engines are loaded and none can take a message. The live-run guard is
    /// load-bearing, not defensive — steering a run is legal with zero usable
    /// engines (`canSubmit` short-circuits on it), so no setup pitch mid-run.
    var engineSetupNeeded: Bool {
        !selectedAgentRemoved && !selectedProfileRemoved && composerPresentation.setupNeeded
    }

    var composerPlaceholderOverride: String? {
        selectedAgentRemoved ? "This agent was removed" : (selectedProfileRemoved ? "Choose a profile" : composerPresentation.placeholderOverride)
    }

    private var composerPresentation: ComposerPresentation {
        ComposerPresentation(
            loadState: composerLoadState,
            hasWorkspace: workspaceID != nil,
            isSubmitting: isSubmitting || isDrainingSelectedConversation,
            hasUnresolvedDelivery: unresolvedDeliveryInput != nil,
            isWorkingRootAvailable: selectedDetail.map {
                $0.summary.workingRoot.availability == .available
            } ?? true,
            hasLiveRun: selectedLiveRun != nil,
            selectedEngine: selectedEngine,
            engines: engines
        )
    }

    private var composerLoadState: ComposerLoadState {
        if didCompleteInitialLoad { return .loaded }
        return errorText == nil ? .loading : .failed
    }

    var submitActionLabel: String {
        composerPresentation.actionLabel
    }

    /// The Engine and session options the next Message will actually run with.
    ///
    /// One accessor because the four have to be read from one place. While a
    /// run is live the Message joins that run's Engine, which need not be the
    /// one the picker shows — and the picker's model, mode and effort belong to
    /// whatever Engine the *picker* is on, since `restoreEngineSelections`
    /// reassigns all three the moment it changes. Taking the Engine from the run
    /// and the options from the picker pairs one Engine with another's model,
    /// which Core rejects as unavailable. The run records its own three for
    /// exactly this reason: so a later send can be launched the same way.
    private var submissionRouting: SubmissionRouting? {
        if let runID = selectedLiveRun?.runID,
           let run = selectedDetail?.runs.first(where: { $0.runID == runID }),
           let engine = engines.first(where: { $0.engineID == run.engineID }) {
            // The mode is the exception: a pick made during the run applies to
            // a message queued behind it, or the drain would restore the old one.
            let modeID = selectedConversationID
                .flatMap { conversationModeID($0, advertisedBy: engine) } ?? run.modeID
            return SubmissionRouting(
                engine: engine,
                modelID: run.modelID,
                modeID: modeID,
                thoughtLevelID: run.thoughtLevelID
            )
        }
        // No live run, or a run whose Engine is no longer installed. Either way
        // the picker is the only coherent source, and its options match it.
        guard let engine = selectedEngine else { return nil }
        return SubmissionRouting(
            engine: engine,
            modelID: selectedModelID,
            modeID: effectiveModeID,
            thoughtLevelID: effectiveThoughtLevelID
        )
    }

    /// The Engine that will actually receive the next Message.
    private var submittingEngine: EngineSummary? { submissionRouting?.engine }

    /// Whether the agent accepts image blocks at all. This is what the adapter
    /// reported during `initialize` — whether the *model* has vision is not
    /// expressible in ACP, and only surfaces as a failed run.
    var canAttachImages: Bool {
        submittingEngine?.capabilities.contains(.imageInput) ?? false
    }

    var attachDisabledReason: String? {
        guard !canAttachImages else { return nil }
        guard let engine = submittingEngine else {
            return "Choose an agent before attaching an image."
        }
        return "\(engine.displayName) doesn’t accept image attachments."
    }

    var selectedSteeringCapability: SteeringCapability {
        guard let runID = selectedLiveRun?.runID,
              let engineID = selectedDetail?.runs.first(where: { $0.runID == runID })?.engineID
        else { return selectedEngine?.steeringCapability ?? .unsupported }
        return engines.first(where: { $0.engineID == engineID })?.steeringCapability ?? .unsupported
    }

    var unresolvedDeliveryInput: ConversationRunInput? {
        selectedDetail?.latestRunInputPage.runInputs.first {
            $0.status == .deliveryUnknown || ($0.status == .pending && $0.kind == .retry)
        }
    }

    var canLoadOlderMessages: Bool {
        transcriptStore.nextCursor != nil
    }

    func runInputs(for messageID: ConversationMessageID) -> [ConversationRunInput] {
        selectedDetail?.latestRunInputPage.runInputs.filter { $0.messageID == messageID } ?? []
    }

    /// The sign-in offer for a conversation whose most recent run stopped for
    /// want of credentials, anchored to the last turn in the transcript — a
    /// failed run ends the thread, so that is where the user is already looking.
    ///
    /// Derived from the persisted run rather than `errorText`: the banner text
    /// is cleared by the very reload that follows a failed run, so an
    /// affordance hung off it would appear and vanish in the same breath.
    func authRecovery(for message: ConversationMessage) -> ChatAuthRecovery? {
        guard let detail = selectedDetail,
              detail.messages.last?.messageID == message.messageID,
              selectedLiveRun == nil,
              let run = detail.runs.first,
              run.status == .failed,
              run.error.objectString("kind") == "authRequired",
              let engine = engines.first(where: { $0.engineID == run.engineID })
        else { return nil }
        let agentID = AgentID(engine.engineID.rawValue)
        // Deliberately not gated on the engine still looking signed out: the
        // sign-in fixes the engine, not the turn, so the card has to survive its
        // own success to offer the resend. It clears when the retry starts —
        // `selectedLiveRun` above — or when a newer message ends the thread.
        let phase: ChatAuthRecovery.Phase
        if signIn.isSigningIn(agentID) {
            phase = .signingIn
        } else if signIn.hasJustSignedIn(agentID) {
            // A completed sign-in outranks the engine list, which is refreshed
            // asynchronously behind it and still describes the world before the
            // login. Credentials that lapse *again* clear this latch from
            // `refreshEngines`, where the state is known to be current — a stale
            // "signed out" is not evidence of anything.
            phase = .signedIn
        } else if engine.authState == .missing || engine.authState == .expired {
            phase = .signedOut
        } else {
            phase = .ready
        }
        return ChatAuthRecovery(
            engine: engine,
            phase: phase,
            undeliveredInput: undeliveredInput(for: run, in: detail)
        )
    }

    /// The runtime-setup offer for a conversation whose most recent run stopped
    /// because Node was missing, broken, or too old. Anchored to the last turn
    /// for the same reason the sign-in card is, and derived from the persisted
    /// run rather than `errorText` so it survives the reload that follows a
    /// failed run.
    ///
    /// The engine's *current* runtime status drives the copy: the user may have
    /// installed Node since the run failed, and a refresh should say so rather
    /// than keep describing the world at failure time.
    func runtimeRecovery(for message: ConversationMessage) -> ChatRuntimeRecovery? {
        guard let detail = selectedDetail,
              detail.messages.last?.messageID == message.messageID,
              selectedLiveRun == nil,
              let run = detail.runs.first,
              run.status == .failed,
              let kind = run.error.objectString("kind"),
              ["runtimeMissing", "runtimeInvalid", "runtimeUnsupported"].contains(kind),
              let engine = engines.first(where: { $0.engineID == run.engineID }),
              let problem = EngineRuntimeProblem(engine: engine)
        else { return nil }
        return ChatRuntimeRecovery(
            engine: engine,
            problem: problem,
            detail: run.error.objectString("message")
        )
    }

    /// Runs the agent started on its own, on the loaded page or live.
    var selectedAutonomousRunIDs: Set<ConversationRunID> {
        var ids = Set((selectedDetail?.runs ?? []).lazy.filter { $0.origin == .autonomous }.map(\.runID))
        for run in liveRuns.values where run.conversationID == selectedConversationID && run.origin == .autonomous {
            ids.insert(run.runID)
        }
        return ids
    }

    /// Each failed run's wire message on the loaded page.
    var selectedRunFailures: [ConversationRunID: String] {
        Dictionary(
            (selectedDetail?.runs ?? []).compactMap { run in
                run.status == .failed ? run.error.objectString("message").map { (run.runID, $0) } : nil
            },
            uniquingKeysWith: { first, _ in first }
        )
    }

    /// Once a turn produced output, recovery is an explicit continuation,
    /// even when the provider's prompt error also marked its input failed.
    var retryableFailedRunID: ConversationRunID? {
        guard let detail = selectedDetail,
              let run = detail.runs.first, run.status == .failed,
              selectedLiveRun == nil, !isSubmitting, !isDrainingSelectedConversation,
              pendingStartConversationID == nil, unresolvedDeliveryInput == nil,
              !selectedAgentRemoved, !selectedProfileRemoved,
              detail.summary.workingRoot.availability == .available,
              engines.contains(where: { $0.engineID == run.engineID && $0.isUsable }),
              !Self.retryWaits(on: detail.latestRunInputPage.runInputs, for: run)
        else { return nil }
        return run.runID
    }

    /// Whether an input still in flight should hold the failed run's retry:
    /// one aimed at that run, or a message whose start failed before it made
    /// a run, which a continuation would strand. This failure's own
    /// continuation is exempt so a repeated press can replay it.
    static func retryWaits(on inputs: [ConversationRunInput], for run: ConversationRunSummary) -> Bool {
        let replayKey = failureContinuation(for: run).id.uuidString
        return inputs.contains { input in
            if input.targetRunID == run.runID {
                return input.status != .delivered && input.status != .failed
            }
            return input.kind == .initial && input.targetRunID == nil
                && (input.status == .pending || input.status == .sending)
                && input.idempotencyKey != replayKey
        }
    }

    static func failureContinuation(for run: ConversationRunSummary) -> QueuedMessage {
        QueuedMessage(
            // A repeated press after a lost RPC response reuses the durable
            // post, including after reopening this conversation.
            id: UUID(uuidString: run.runID.rawValue) ?? UUID(),
            body: "Continue from where the previous attempt stopped.",
            engineID: run.engineID, modelID: run.modelID,
            modeID: run.modeID, thoughtLevelID: run.thoughtLevelID
        )
    }

    /// Continue on a note card: core hands the waiting reports to the agent
    /// in a new run.
    func continueDelegation(backend: DiffReviewModel) async {
        guard !isMutating, let workspaceID, let conversationID = selectedConversationID else { return }
        isMutating = true
        errorText = nil
        defer { isMutating = false }
        do {
            let result = try await backend.continueDelegation(workspaceID: workspaceID, conversationID: conversationID)
            if let run = result.run { mergeActiveRun(run) }
            await refreshAfterSend(conversationID, backend: backend)
        } catch {
            await loadDetailIfSelected(conversationID, backend: backend)
            if selectedConversationID == conversationID { errorText = error.localizedDescription }
        }
    }

    /// Send to Parent in a delegated child: its waiting report wakes the
    /// parent. Core's `conversation.listChanged` then hides the button.
    func sendToParent(backend: DiffReviewModel) async {
        guard !isMutating, let workspaceID, let conversationID = selectedConversationID else { return }
        isMutating = true
        errorText = nil
        defer { isMutating = false }
        do {
            _ = try await backend.sendToParent(workspaceID: workspaceID, conversationID: conversationID)
        } catch {
            if selectedConversationID == conversationID { errorText = error.localizedDescription }
        }
    }

    func retryFailedRun(_ runID: ConversationRunID, backend: DiffReviewModel) async {
        guard !isMutating, retryableFailedRunID == runID,
              let detail = selectedDetail,
              let run = detail.runs.first, run.runID == runID
        else { return }
        let conversationID = detail.summary.conversationID
        isMutating = true
        isSubmitting = true
        errorText = nil
        defer {
            isMutating = false
            isSubmitting = false
        }
        do {
            try await postAndStart(Self.failureContinuation(for: run), to: conversationID, backend: backend)
            await refreshAfterSend(conversationID, backend: backend)
        } catch {
            await loadDetailIfSelected(conversationID, backend: backend)
            if selectedConversationID == conversationID { errorText = error.localizedDescription }
        }
    }

    /// The offer for a conversation whose most recent run was refused because
    /// a setting it recorded is no longer offered. Anchored and derived like
    /// `runtimeRecovery`; the resend it offers runs with `retrySelections`.
    func selectionRecovery(for message: ConversationMessage) -> ChatSelectionRecovery? {
        guard let detail = selectedDetail,
              detail.messages.last?.messageID == message.messageID,
              selectedLiveRun == nil,
              let run = detail.runs.first,
              run.status == .failed,
              run.error.objectString("kind") == "selectionUnavailable",
              let engine = engines.first(where: { $0.engineID == run.engineID })
        else { return nil }
        let retry = retrySelections(for: run)
        let stale: (ChatSelectionRecovery.Setting, String, String)? = {
            if let model = run.modelID, retry.modelID != model {
                return (.model, model, displayName(retry.modelID, in: engine.models.map { ($0.modelID, $0.displayName) },
                                                   default: engine.defaultModelID))
            }
            if let mode = run.modeID, retry.modeID != mode {
                return (.mode, mode, displayName(retry.modeID, in: engine.modes.map { ($0.id, $0.displayName) },
                                                 default: engine.defaultModeID))
            }
            if let level = run.thoughtLevelID, retry.thoughtLevelID != level {
                let select = engine.thoughtLevelSelect(forModel: retry.modelID)
                return (.thoughtLevel, level, displayName(
                    retry.thoughtLevelID,
                    in: select.levels.map { ($0.id, $0.displayName) },
                    default: select.defaultID
                ))
            }
            return nil
        }()
        return ChatSelectionRecovery(
            engine: engine,
            setting: stale?.0,
            staleValue: stale?.1,
            replacement: stale?.2 ?? "Default",
            detail: run.error.objectString("message"),
            undeliveredInput: undeliveredInput(for: run, in: detail)
        )
    }

    private func displayName(_ id: String?, in options: [(String, String)], default defaultID: String?) -> String {
        let resolved = id ?? defaultID
        return options.first { $0.0 == resolved }?.1 ?? resolved ?? "Default"
    }

    /// The settings a resend of `run` launches with.
    ///
    /// Replayed from the run, not from the pickers: a retry has to run the way
    /// the original was launched. Falling back to the engine defaults here is
    /// how a turn the user restricted to a read-only mode comes back able to
    /// write. Runs recorded before the backend stored these carry nil, which
    /// is the old behavior for them and only them.
    ///
    /// The exception is a run Core refused as `selectionUnavailable`: replaying
    /// its stale value is refused every time. The first setting the engine no
    /// longer offers, in the model, mode, effort order Core checks them, takes
    /// the composer's pick for the run's engine — the engine default when the
    /// composer is on another — and the selection card names it before the
    /// user resends. One per resend: the card names only one, and a second
    /// stale value swapped with it would change a mode nobody was told about.
    /// Core refuses that one next, and the card names it then. Any other
    /// failure replays exactly.
    func retrySelections(for run: ConversationRunSummary) -> RetrySelections {
        Self.retrySelections(
            for: run,
            refused: run.status == .failed && run.error.objectString("kind") == "selectionUnavailable",
            engine: engines.first { $0.engineID == run.engineID },
            picks: selectedEngine?.engineID == run.engineID
                ? (selectedModelID, modeID(for: run.conversationID), effectiveThoughtLevelID)
                : (nil, nil, nil)
        )
    }

    typealias RetrySelections = (modelID: String?, modeID: String?, thoughtLevelID: String?)

    /// `retrySelections` without the model: `refused` is whether Core refused
    /// the run as `selectionUnavailable`; `picks` are the composer's, or all
    /// nil when the composer is on another engine.
    static func retrySelections(
        for run: ConversationRunSummary,
        refused: Bool,
        engine: EngineSummary?,
        picks: RetrySelections
    ) -> RetrySelections {
        var retry: RetrySelections = (run.modelID, run.modeID, run.thoughtLevelID)
        guard refused else { return retry }
        func isStale(_ recorded: String?, offered: [String]?) -> Bool {
            guard let recorded, let offered, !offered.isEmpty else { return false }
            return !offered.contains(recorded)
        }
        if isStale(run.modelID, offered: engine?.models.map(\.modelID)) {
            retry.modelID = picks.modelID
        } else if isStale(run.modeID, offered: engine?.modes.map(\.id)) {
            retry.modeID = picks.modeID
        } else if isStale(
            run.thoughtLevelID,
            offered: engine?.thoughtLevelSelect(forModel: run.modelID).levels.map(\.id)
        ) {
            retry.thoughtLevelID = picks.thoughtLevelID
        }
        return retry
    }

    /// The input a failed run left behind, if it is still re-sendable. A run
    /// that failed after acceptance has a `delivered` input and nothing to
    /// recover.
    ///
    /// `failed` only. A `deliveryUnknown` input has its own card, with the
    /// Retry/Mark Delivered pair its ambiguity calls for, and offering a second
    /// resend beside it would be a puzzle rather than a choice.
    private func undeliveredInput(
        for run: ConversationRunSummary,
        in detail: ConversationDetail
    ) -> ConversationRunInput? {
        detail.latestRunInputPage.runInputs.first { input in
            input.targetRunID == run.runID && input.status == .failed
        }
    }

    private func configureSignInIfNeeded(
        workspaceID: WorkspaceID,
        backend: DiffReviewModel
    ) async {
        // The app-wide setup subscription refreshes local and Settings sign-ins.
        guard !backendHandlersRetired else { return }
        await signIn.configure(workspaceID: workspaceID, backend: backend)
    }

    /// The core invalidates the engine list after catalog and lifecycle changes.
    /// Chat does not need to interpret individual install or sign-in phases.
    private func configureEngineRefreshIfNeeded(backend: DiffReviewModel) async {
        guard engineRefreshSubscription == nil, !backendHandlersRetired else { return }
        let subscription = await backend.subscribeToBackendEvents(
            filter: AgentEventFilter(
                methods: ["engine.changed"]
            )
        ) { [weak self, weak backend] _ in
            guard let self, let backend else { return }
            Task { @MainActor in
                await self.refreshEnginesCoalescing(backend: backend)
            }
        }
        guard !backendHandlersRetired, engineRefreshSubscription == nil else {
            await backend.unsubscribeFromBackendEvents(subscription)
            return
        }
        engineRefreshSubscription = subscription
    }

    /// Core announces each new plan-usage snapshot, from a turn or its
    /// background probe; every window shows the same account-wide numbers.
    private func configurePlanUsageIfNeeded(backend: DiffReviewModel) async {
        guard planUsageSubscription == nil, !backendHandlersRetired else { return }
        let subscription = await backend.subscribeToBackendEvents(
            filter: AgentEventFilter(methods: ["agent.planUsageChanged"])
        ) { [weak self] notification in
            guard let change = try? JSONDecoder().decode(
                ProtocolNotification<AgentPlanUsageChangedParams>.self,
                from: notification.payload
            ).params else { return }
            Task { @MainActor in
                self?.applyPlanUsageChange(change)
            }
        }
        guard !backendHandlersRetired, planUsageSubscription == nil else {
            await backend.unsubscribeFromBackendEvents(subscription)
            return
        }
        planUsageSubscription = subscription
    }

    /// One catalog probe lands three separately-real facts — the models, the
    /// sign-ins, the auth verdict — so the core announces three changes for it.
    /// Folding a burst into the refresh already running is what keeps a cold
    /// launch to a couple of `engine.list` calls instead of one per fact per
    /// agent. Deliberately not inside `refreshEngines`: an explicit "Check
    /// Again" carries `refreshRuntime` and must not be answered by a refresh
    /// that doesn't.
    private func refreshEnginesCoalescing(backend: DiffReviewModel) async {
        guard !noteEngineChangeIfRefreshing() else { return }
        await refreshEngines(backend: backend)
    }

    /// Records a change announced while a refresh is running, so whichever
    /// caller started that refresh answers it on the way out. Returns whether
    /// the change was recorded; `false` means nothing is running and the caller
    /// owes a refresh of its own.
    func noteEngineChangeIfRefreshing() -> Bool {
        guard isRefreshingEngines else { return false }
        engineRefreshPending = true
        return true
    }

    /// Re-read the engine list after something outside Chat changed an engine's
    /// auth state — a sign-in, most of all.
    ///
    /// A change announced while this runs is answered here, whichever caller
    /// started it. Sign-in, "Check Again" and failed-run recovery each call this
    /// directly rather than through `refreshEnginesCoalescing`, and their
    /// `engine.list` was issued before the probe wrote; draining only in the
    /// coalescing path left those callers holding the pre-probe catalog with
    /// nothing to come back for it once the timed refetch was removed.
    /// `isRefreshingEngines` stays set across the drain so a change announced
    /// between passes joins the loop instead of starting a second refresh.
    func refreshEngines(
        backend: DiffReviewModel,
        refreshRuntime: Bool = false
    ) async {
        guard workspaceID != nil else { return }
        await coalescedEngineRefresh(refreshRuntime: refreshRuntime) { runtime in
            await self.performEngineRefresh(backend: backend, refreshRuntime: runtime)
        }
    }

    /// The guard, the flag and the drain in one place, with the refresh itself
    /// passed in so the contract holds without a backend to fake. `pass` runs
    /// once for the initiating request, then once more for each announcement
    /// `noteEngineChangeIfRefreshing` recorded while it ran — always plain,
    /// since only the initiating request carries `refreshRuntime`.
    func coalescedEngineRefresh(
        refreshRuntime: Bool,
        _ pass: (Bool) async -> Void
    ) async {
        isRefreshingEngines = true
        defer { isRefreshingEngines = false }
        await pass(refreshRuntime)
        while engineRefreshPending {
            engineRefreshPending = false
            await pass(false)
        }
    }

    private func performEngineRefresh(
        backend: DiffReviewModel,
        refreshRuntime: Bool
    ) async {
        guard let workspaceID else { return }
        guard let refreshed = try? await backend.listEngines(
            workspaceID: workspaceID,
            refreshRuntime: refreshRuntime
        ).engines,
              self.workspaceID == workspaceID
        else { return }
        let uninstalled = try? await backend.uninstalledEngineIDs()
        guard self.workspaceID == workspaceID, !Task.isCancelled else { return }
        if let uninstalled { uninstalledEngineIDs = uninstalled }
        engines = refreshed
        selectReadyLandingEngineIfNeeded()
        // This list is the first current word on whether a sign-in took. An
        // engine that still reports no credentials here has genuinely not been
        // signed in, whatever the login claimed, so the card goes back to
        // asking rather than showing a success that never landed.
        for engine in refreshed where engine.authState == .missing || engine.authState == .expired {
            signIn.forgetSuccess(AgentID(engine.engineID.rawValue))
        }
        restoreEngineSelections()
    }

    /// Setup may make a different agent ready than the unavailable saved choice.
    /// Existing conversations keep their own engine selection.
    private func selectReadyLandingEngineIfNeeded() {
        guard selectedConversationID == nil, selectedEngine?.isUsable != true,
              selectedEngineID == nil || engines.contains(where: { $0.engineID == selectedEngineID }),
              let ready = EngineSummary.defaultSelection(from: engines), ready.isUsable
        else { return }
        selectedEngineID = ready.engineID
    }

    /// Live Run lifecycle remains visible while transcript pages load.
    var selectedCanonicalLiveRun: ChatLiveRunState? {
        // Keyed off the selection ID, not the loaded detail: a streaming run's live
        // output and Cancel control must survive a pending or failed detail load.
        guard let conversationID = selectedConversationID else { return nil }
        return liveRuns.values.first(where: { run in
            run.conversationID == conversationID && (run.status == .queued || run.status == .running)
        })
    }

    var selectedLiveRun: ChatLiveRunState? { selectedCanonicalLiveRun }

    /// Every approval this conversation is waiting on, oldest first.
    ///
    /// All of them, not just the first: each `ChatPendingPermission` holds its own
    /// continuation, so an unrendered one leaves the run blocked with nothing on
    /// screen to explain why, until the backend's permission timeout fires. Showing
    /// only one was a concession to the pinned bar, where the cards stacked into the
    /// composer; in the thread they cost nothing.
    ///
    /// Keyed off the selection, not the loaded detail, for the same reason
    /// `selectedLiveRun` is: an approval must stay answerable through a pending or
    /// failed detail load.
    ///
    /// Plans are not among them: they wait in the plan panel above the
    /// composer, not in the thread (`selectedPendingPlans`).
    var selectedPendingPermissions: [ChatPendingPermission] {
        guard let conversationID = selectedConversationID else { return [] }
        return pendingPermissions.filter { $0.request.conversationID == conversationID && $0.plan == nil }
    }

    /// The plans the selected conversation waits on, oldest first. The panel
    /// shows the first; answering it brings up the next.
    var selectedPendingPlans: [ChatPendingPermission] {
        guard let conversationID = selectedConversationID else { return [] }
        return pendingPermissions.filter { $0.request.conversationID == conversationID && $0.plan != nil }
    }

    /// The selected conversation's newest decided plan among the transcript
    /// pages loaded so far, without subagents'.
    var selectedLatestPlanRecord: ChatPlan? {
        guard let conversationID = selectedConversationID,
              transcriptStore.conversationID == conversationID else { return nil }
        return transcriptStore.items.values
            .filter { $0.subagentThreadID == nil && $0.permission?.plan != nil }
            .max { $0.position < $1.position }?
            .permission?.chatPlan
    }

    /// The forms the selected conversation waits on, oldest first. The panel
    /// shows the first; answering it brings up the next.
    var selectedPendingQuestions: [ChatPendingQuestion] {
        guard let conversationID = selectedConversationID else { return [] }
        return pendingQuestions.filter { $0.request.conversationID == conversationID }
    }

    var attentionConversationIDs: Set<ConversationID> {
        Set(pendingPermissions.compactMap(\.request.conversationID))
            .union(pendingQuestions.compactMap(\.request.conversationID))
    }

    var runningConversationIDs: Set<ConversationID> {
        Set(liveRuns.values.compactMap { run in
            (run.status == .queued || run.status == .running) ? run.conversationID : nil
        })
    }

    var draftWorkingRootName: String {
        switch draftWorkingRoot {
        case .workspaceFiles:
            return workspaceFiles?.displayName ?? "Workspace files"
        case .project(let projectID, let sourceID):
            return projects.first {
                $0.project.projectID == projectID
                    && $0.source.projectSourceID == sourceID
            }?.project.displayName ?? "Unavailable project"
        }
    }

    /// Applies a root the user picked from the composer's chooser.
    ///
    /// Normalized as well as validity-checked: the chooser only exists in the
    /// multi-project shape, so a pick arriving in a collapsed shape came from
    /// stale state rather than a visible control and must not stand.
    func selectDraftWorkingRoot(_ selection: WorkingRootSelection) {
        guard isValid(selection) else { return }
        draftWorkingRoot = normalizeRestoredWorkingRoot(
            selection,
            projects: projects,
            selectedProject: projectSummary,
            hasWorkspaceFiles: workspaceFiles != nil
        )
    }

    private func isValid(_ selection: WorkingRootSelection) -> Bool {
        switch selection {
        case .workspaceFiles:
            return workspaceFiles != nil
        case .project(let projectID, let sourceID):
            return projects.contains {
                $0.project.projectID == projectID
                    && $0.source.projectSourceID == sourceID
            }
        }
    }

    func configure(
        workspaceID: WorkspaceID,
        workspaceFiles: WorkingRootSummary?,
        projects: [WorkspaceProjectSummary],
        projectSummary: ResolvedProject?,
        backend: DiffReviewModel
    ) {
        let changedWorkspace = self.workspaceID != workspaceID
        self.workspaceID = workspaceID
        if changedWorkspace {
            transcriptCache.forgetAll()
            transcriptStore = ChatTranscriptStore()
            messageQueue = ChatMessageQueue.restored(from: defaults, workspaceID: workspaceID)
        }
        self.workspaceFiles = workspaceFiles
        self.projects = projects
        self.projectSummary = projectSummary
        conversations = conversations.map { conversation in
            var updated = conversation
            updated.workingRoot = conversation.workingRoot.updatingAvailability(for: projects)
            return updated
        }
        if var detail = selectedDetail {
            detail.summary.workingRoot = detail.summary.workingRoot
                .updatingAvailability(for: projects)
            selectedDetail = detail
        }
        // The draft root here is never one the user just picked — it is either a
        // pre-configure literal or a root left over from the previous shape — so
        // it is normalized rather than merely validity-checked. A validity check
        // alone would let `.workspaceFiles` survive into a collapsed shape.
        if changedWorkspace {
            draftWorkingRoot = impliedWorkingRoot(
                projects: projects,
                selectedProject: projectSummary
            )
        } else {
            draftWorkingRoot = normalizeRestoredWorkingRoot(
                draftWorkingRoot,
                projects: projects,
                selectedProject: projectSummary,
                hasWorkspaceFiles: workspaceFiles != nil
            )
        }
        if changedWorkspace {
            draftGeneration &+= 1
            initialLoadGeneration &+= 1
            initialLoadTask?.cancel()
            initialLoadTask = nil
            cancelDetailLoad()
            resolveAllPermissions(.cancelled)
            isLoading = false
            didCompleteInitialLoad = false
            engines = []
            conversations = []
            selectedDetail = nil
            selectedConversationID = nil
            errorText = nil
            noticeText = nil
            liveRuns = [:]
            terminalRunIDs = []
            pendingStartConversationID = nil
            lastEventSequenceByRun = [:]
            subscribedWorkspaceID = nil
        }
        guard !backendHandlersRetired else { return }
        configureTask?.cancel()
        configureTask = Task {
            await reconcilePermissionRegistration(backend: backend, focused: false)
            guard !Task.isCancelled else { return }
            await subscribeToEventsIfNeeded(backend: backend)
            guard !Task.isCancelled else { return }
            // The chat route can mount before workspace.open resolves, in which case
            // its load task ran with no workspaceID and gave up. Now that the model
            // is configured, load whatever is still missing (no-op once loaded).
            await loadIfNeeded(backend: backend)
        }
    }

    func installBackendHandlers(backend: DiffReviewModel) async {
        await reconcilePermissionRegistration(backend: backend, focused: false)
        await subscribeToEventsIfNeeded(backend: backend)
    }

    /// Releases everything the model holds on core, for good: the window is
    /// closing. Each install path checks `backendHandlersRetired` after its
    /// awaits, so one still in flight releases what it took instead of keeping it.
    func uninstallBackendHandlers(backend: DiffReviewModel) async {
        backendHandlersRetired = true
        configureTask?.cancel()
        configureTask = nil
        namedAgentsObserver?.cancel()
        namedAgentsObserver = nil
        signIn.detach()
        cancelDetailLoad()
        resolveAllPermissions(.cancelled)
        if let eventSubscription {
            await backend.unsubscribeFromBackendEvents(eventSubscription)
            self.eventSubscription = nil
        }
        if let engineRefreshSubscription {
            await backend.unsubscribeFromBackendEvents(engineRefreshSubscription)
            self.engineRefreshSubscription = nil
        }
        if let planUsageSubscription {
            await backend.unsubscribeFromBackendEvents(planUsageSubscription)
            self.planUsageSubscription = nil
        }
        subscribedWorkspaceID = nil
        if let permissionRegistration {
            self.permissionRegistration = nil
            await backend.unregisterAgentPermissionHandler(permissionRegistration)
        }
    }

    func loadIfNeeded(backend: DiffReviewModel) async {
        guard !backendHandlersRetired else { return }
        guard let workspaceID else {
            markUnavailable()
            return
        }
        await subscribeToEventsIfNeeded(backend: backend)
        await configureSignInIfNeeded(workspaceID: workspaceID, backend: backend)
        await configureEngineRefreshIfNeeded(backend: backend)
        await configurePlanUsageIfNeeded(backend: backend)
        if didCompleteInitialLoad {
            return
        }
        if let initialLoadTask {
            await initialLoadTask.value
            return
        }

        let generation = initialLoadGeneration
        let task = Task { @MainActor [weak self, weak backend] in
            guard let self, let backend else { return }
            await self.load(workspaceID: workspaceID, backend: backend)
        }
        initialLoadTask = task
        await task.value
        if initialLoadGeneration == generation {
            initialLoadTask = nil
        }
    }

    func reload(backend: DiffReviewModel) async {
        guard let workspaceID else {
            markUnavailable()
            return
        }
        await load(workspaceID: workspaceID, backend: backend)
    }

    func select(_ conversationID: ConversationID, backend: DiffReviewModel) {
        transcriptLoadError = nil
        // Switching sessions lands on the destination immediately: drop the outgoing
        // thread and flag the load so the view shows a spinner instead of stale
        // content. Re-selecting the current session refreshes silently in place.
        let isSwitch = selectedDetail?.summary.conversationID != conversationID
        // Before the flag, not after: cancelling clears it, so raising it first
        // would hand this switch a spinner that the cancel immediately lowers.
        cancelDetailLoad()
        noticeText = nil
        let session = showTranscript(of: conversationID)
        selectedConversationID = conversationID
        // A cached conversation returns as it was left: its store kept
        // merging deltas, so only lifecycle detail refreshes, in place.
        let cached = isSwitch ? session.detail.flatMap { detail in
            session.store.coreInstanceID == nil ? nil : detail
        } : nil
        // Cleared only by a resync that finishes: switching away first must
        // not lose it.
        let resyncs = session.needsResync
        if isSwitch {
            if let cached {
                // Shown at once but not applied: the engine, mode and usage it
                // carries went stale while out of view. The refresh applies.
                selectedDetail = cached
                showsCachedDetail = true
                isLoadingDetail = false
            } else {
                selectedDetail = nil
                isLoadingDetail = true
            }
            // "Signed in just now" is a claim about the turn the user signed in
            // from. Carried to another thread it would put a green success on
            // an unrelated old failure, so it does not travel.
            signIn.forgetSuccesses()
        }
        detailLoadTask = Task { [weak self, weak backend] in
            guard let self, let backend else { return }
            await self.reconcilePermissionRegistration(backend: backend, focused: true)
            guard !Task.isCancelled else { return }
            await self.loadDetail(conversationID, backend: backend, transcript: cached == nil)
            guard resyncs, !Task.isCancelled else { return }
            await self.resyncTranscript(conversationID, backend: backend)
        }
    }

    /// Selects a conversation and waits for its detail to load, so the route screenshot
    /// capture path can render the populated workbench deterministically. Picks the first
    /// conversation whose title contains `titleFragment` (case-insensitive), falling back
    /// to the most recent conversation.
    func selectForCapture(matching titleFragment: String?, backend: DiffReviewModel) async {
        var match = conversations.first
        if let fragment = titleFragment?.lowercased(), !fragment.isEmpty {
            match = conversations.first { $0.conversationID.rawValue.lowercased() == fragment }
                ?? conversations.first { $0.title.lowercased().contains(fragment) }
                ?? match
        }
        guard let match else { return }
        cancelDetailLoad()
        selectedConversationID = match.conversationID
        await reconcilePermissionRegistration(backend: backend, focused: true)
        guard !Task.isCancelled else { return }
        await loadDetail(match.conversationID, backend: backend)
    }

    func focusBackendHandlers(backend: DiffReviewModel) async {
        await reconcilePermissionRegistration(backend: backend, focused: true)
    }

    private func reconcilePermissionRegistration(
        backend: DiffReviewModel,
        focused: Bool
    ) async {
        guard !backendHandlersRetired else { return }
        guard let workspaceID else {
            if let permissionRegistration {
                self.permissionRegistration = nil
                await backend.unregisterAgentPermissionHandler(permissionRegistration)
            }
            return
        }

        let registration: AgentPermissionRegistrationToken
        if let permissionRegistration {
            registration = permissionRegistration
        } else {
            let candidate = await backend.registerAgentPermissionHandler(
                workspaceID: workspaceID,
                conversationID: selectedConversationID
            ) { [weak self] request in
                guard let self else { return .cancelled }
                return await self.enqueuePermission(request)
            } questionHandler: { [weak self] request in
                guard let self else { return .cancelled }
                return await self.enqueueQuestion(request)
            }
            guard !backendHandlersRetired, self.workspaceID == workspaceID else {
                await backend.unregisterAgentPermissionHandler(candidate)
                return
            }
            if let existing = permissionRegistration {
                await backend.unregisterAgentPermissionHandler(candidate)
                registration = existing
            } else {
                permissionRegistration = candidate
                registration = candidate
            }
        }

        await backend.updateAgentPermissionRegistration(
            registration,
            workspaceID: workspaceID,
            conversationID: selectedConversationID
        )
        guard self.workspaceID == workspaceID else { return }
        if focused {
            await backend.focusAgentPermissionRegistration(registration)
        }
    }

    func beginNewConversation(backend: DiffReviewModel) {
        showDraftTranscript()
        cancelDetailLoad()
        draftGeneration &+= 1
        selectedConversationID = nil
        selectedDetail = nil
        selectReadyLandingEngineIfNeeded()
        errorText = nil
        noticeText = nil
        Task {
            await reconcilePermissionRegistration(backend: backend, focused: true)
        }
    }

    func recoverAfterCoreReplacement(backend: DiffReviewModel) async {
        // Another Core instance: every cached store and detail is stale. What
        // the reader left open and where stays, and applies after reload.
        transcriptCache.removeSessions()
        transcriptStore = ChatTranscriptStore()
        cancelDetailLoad()
        resolveAllPermissions(.cancelled)
        isLoading = false
            isSubmitting = false
            isMutating = false
            isLoadingOlderMessages = false
        liveRuns = [:]
        terminalRunIDs = []
        pendingStartConversationID = nil
        lastEventSequenceByRun = [:]

        guard let workspaceID else {
            markUnavailable()
            await reconcilePermissionRegistration(backend: backend, focused: false)
            return
        }

        await reconcilePermissionRegistration(backend: backend, focused: true)
        await subscribeToEventsIfNeeded(backend: backend)
        await load(workspaceID: workspaceID, backend: backend)
    }

    // MARK: - Workbench actions

    /// Deletes a conversation. A parent's delegated children go with it
    /// (`.delete`) or stop and stay as top-level conversations (`.keep`).
    func delete(
        _ conversation: ConversationSummary,
        children: ConversationDeleteChildren = .keep,
        backend: DiffReviewModel
    ) async -> Bool {
        guard let workspaceID else {
            markUnavailable()
            return false
        }
        isMutating = true
        errorText = nil
        noticeText = nil
        let deleted = [conversation.conversationID] + (children == .delete
            ? conversations.filter { $0.parentConversationID == conversation.conversationID }.map(\.conversationID)
            : [])
        do {
            let result = try await backend.deleteConversation(
                workspaceID: workspaceID,
                conversationID: conversation.conversationID,
                children: children
            )
            for conversationID in deleted {
                await forgetDeleted(conversationID, backend: backend)
            }
            // Kept children are top-level now; the next list says so too.
            for index in conversations.indices where conversations[index].parentConversationID == conversation.conversationID {
                conversations[index].parentConversationID = nil
            }
            noticeText = Self.harnessCleanupNotice(result.harnessSessionCleanup)
            isMutating = false
            Self.logger.info(
                "deleted conversation conversationID=\(conversation.conversationID.rawValue, privacy: .public)"
            )
            return true
        } catch {
            isMutating = false
            errorText = error.localizedDescription
            Self.logger.error(
                "failed to delete conversation: \(error.localizedDescription, privacy: .private)"
            )
            return false
        }
    }

    private func recordAgentNames() {
        guard let detail = selectedDetail else { return }
        func engine(_ engineID: EngineID?) -> EngineSummary? {
            engineID.flatMap { engineID in engines.first { $0.engineID == engineID } }
        }
        var names = conversationAgentNames
        var brands = conversationAgentBrands
        func record(_ conversationID: ConversationID, _ engine: EngineSummary?) {
            guard let engine else { return }
            names[conversationID] = engine.displayName
            brands[conversationID] = engine.brand
        }
        record(detail.summary.conversationID, engine(detail.runs.first?.engineID))
        // The task names the agent that delegated it, which may since have
        // changed; the parent's own latest run, once seen, says who it is now.
        if let parentID = detail.summary.parentConversationID, names[parentID] == nil {
            record(parentID, engine(detail.messages.lazy.compactMap { $0.task?.startedByEngineID }.first))
        }
        if names != conversationAgentNames { conversationAgentNames = names }
        if brands != conversationAgentBrands { conversationAgentBrands = brands }
    }

    /// The agents working with the selected conversation, for the pill.
    var selectedAgentGroup: ConversationAgentGroup? {
        ConversationAgentGroup.of(
            selectedConversationID,
            conversations: conversations,
            agentNames: conversationAgentNames,
            agentBrands: conversationAgentBrands,
            running: runningConversationIDs,
            attention: attentionConversationIDs
        )
    }

    /// Drops what the app held for a conversation core deleted.
    private func forgetDeleted(_ conversationID: ConversationID, backend: DiffReviewModel) async {
        let deletedRunIDs = Set(
            selectedDetail?.summary.conversationID == conversationID
                ? selectedDetail?.runs.map(\.runID) ?? []
                : []
        )
        resolvePermissions(conversationID: conversationID, outcome: .cancelled)
        liveRuns = liveRuns.filter { $0.value.conversationID != conversationID }
        for runID in deletedRunIDs {
            terminalRunIDs.remove(runID)
            lastEventSequenceByRun.removeValue(forKey: runID)
        }
        conversations.removeAll { $0.conversationID == conversationID }
        didDeleteConversation(conversationID)
        transcriptCache.forget(conversationID)
        if transcriptStore.conversationID == conversationID {
            transcriptStore = ChatTranscriptStore()
        }
        if selectedConversationID == conversationID {
            selectedConversationID = nil
            selectedDetail = nil
            isLoadingDetail = false
            await reconcilePermissionRegistration(backend: backend, focused: true)
        }
    }

    /// A run that carried images and failed is the only signal we get that the
    /// *model* lacks vision: ACP describes image support per agent, never per
    /// model, so the agent accepts the block and the model rejects the turn.
    /// This is a client-side guess on state the client already has — Core does
    /// not try to pattern-match adapter error text.
    private func visionHint(forFailedRun runID: ConversationRunID) -> String? {
        guard let detail = selectedDetail else { return nil }
        let messageIDs = Set(
            detail.latestRunInputPage.runInputs
                .filter { $0.targetRunID == runID || $0.requestedAgainstRunID == runID }
                .map(\.messageID)
        )
        let carriedImages = detail.latestMessagePage.messages.contains {
            messageIDs.contains($0.messageID) && !$0.attachments.isEmpty
        }
        guard carriedImages else { return nil }
        return " This message included an image — the selected model may not accept image input."
    }

    /// Surface a composer-side intake failure (an unreadable drop, too many
    /// images) in the same place RPC failures land.
    func reportAttachmentProblem(_ message: String?) {
        errorText = message
    }

    /// Hand an image to Core and get back the attachment the composer shows.
    ///
    /// Core copies the bytes out of the temp file, so the file is ours to
    /// remove the moment the call returns — Core never reads it again.
    func ingestAttachment(
        _ payload: ChatAttachmentPayload,
        sourceKind: ConversationAttachmentSourceKind,
        originPath: String? = nil,
        backend: DiffReviewModel
    ) async -> ConversationAttachment? {
        guard let workspaceID else {
            markUnavailable()
            return nil
        }
        do {
            let handoff = try ChatAttachmentIntake.writeTemporaryFile(payload)
            defer { try? FileManager.default.removeItem(at: handoff) }
            let created = try await backend.createConversationAttachment(
                workspaceID: workspaceID,
                sourcePath: handoff.path,
                mimeType: payload.mimeType,
                width: payload.pixelWidth,
                height: payload.pixelHeight,
                sourceKind: sourceKind,
                originPath: originPath,
                idempotencyKey: UUID().uuidString
            )
            errorText = nil
            Self.logger.notice(
                """
                attachment stored: id=\(created.attachment.attachmentID.rawValue, privacy: .public) \
                path=\(created.attachment.path, privacy: .public)
                """
            )
            return created.attachment
        } catch {
            errorText = error.localizedDescription
            Self.logger.error(
                "failed to attach image: \(error.localizedDescription, privacy: .public)"
            )
            return nil
        }
    }

    /// Drop an attachment the user removed from the composer. A failure here is
    /// logged and swallowed: the card is already gone from the user's view, and
    /// the unclaimed-attachment sweep reclaims the bytes either way.
    func discardAttachment(
        _ attachment: ConversationAttachment,
        workspaceID explicitWorkspaceID: WorkspaceID? = nil,
        backend: DiffReviewModel
    ) async {
        guard let workspaceID = explicitWorkspaceID ?? self.workspaceID else { return }
        do {
            _ = try await backend.discardConversationAttachment(
                workspaceID: workspaceID,
                attachmentID: attachment.attachmentID
            )
        } catch {
            Self.logger.error(
                "failed to discard attachment: \(error.localizedDescription, privacy: .private)"
            )
        }
    }

    func submit(
        _ draft: String,
        attachments: [ConversationAttachment] = [],
        backend: DiffReviewModel
    ) async -> Bool {
        await submit(draft, attachmentIDs: attachments.map(\.attachmentID), backend: backend)
    }

    /// The identifier-only entry point. A queued message kept its attachment IDs
    /// rather than the attachment instances, and Send Now sends it down exactly
    /// the path a typed message takes — which with a run still working means it
    /// steers.
    func submit(
        _ draft: String,
        attachmentIDs: [ConversationAttachmentID],
        backend: DiffReviewModel
    ) async -> Bool {
        let message = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        // An image on its own is a complete Message; Core accepts an empty body
        // when attachments are claimed.
        guard !message.isEmpty || !attachmentIDs.isEmpty else { return false }
        guard let workspaceID else {
            markUnavailable()
            return false
        }
        // The Sign in cards were for the turn before this one; a message
        // that fails to send puts them back.
        let clearedSignIns = selectedConversationID.map { conversationID in
            (conversationID, mcpSignIn?.clearNeeds(conversationID) ?? [])
        }
        isSubmitting = true
        errorText = nil
        do {
            // A follow-up the agent started on its own has no turn to steer
            // into: the message posts and starts a run, which takes over.
            if let activeRun = selectedLiveRun, activeRun.origin != .autonomous {
                let steered = try await backend.steerConversationRun(
                    workspaceID: workspaceID,
                    conversationID: activeRun.conversationID,
                    runID: activeRun.runID,
                    message: message,
                    attachmentIDs: attachmentIDs,
                    idempotencyKey: UUID().uuidString
                )
                if steered.run.runID != activeRun.runID {
                    liveRuns.removeValue(forKey: activeRun.runID)
                    // The awaited loadDetail below re-derives this run's text
                    // from persisted events, which is the authority on it.
                }
                mergeActiveRun(steered.run)
                await loadDetail(activeRun.conversationID, backend: backend)
                isSubmitting = false
                return true
            }

            guard let engineID = selectedEngine?.engineID else {
                throw ChatSubmitError.noEngine
            }
            // Read before a new conversation is selected below: a draft sends
            // the new-chat default, an existing conversation its own mode.
            let modeID = effectiveModeID
            // Pinned before the wait below, which a new selection can outlast.
            let targetConversationID = selectedConversationID
            let workingRoot = draftWorkingRoot
            // A new chat copies the switch's default, and a run reads its value.
            await settleAlwaysAllowToolActions()
            let conversationID: ConversationID
            let runInputID: ConversationRunInputID
            // Branch on the selection ID, not the loaded detail: submitting while a
            // detail load is in flight must post to the selected conversation, not
            // silently create a new one.
            if let selectedConversationID = targetConversationID {
                let posted = try await backend.postConversationMessage(
                    workspaceID: workspaceID,
                    conversationID: selectedConversationID,
                    message: message,
                    attachmentIDs: attachmentIDs,
                    idempotencyKey: UUID().uuidString
                )
                conversationID = selectedConversationID
                runInputID = posted.input.runInputID
            } else {
                let created = try await backend.createConversation(
                    workspaceID: workspaceID,
                    workingRoot: workingRoot,
                    title: nil,
                    message: ConversationInitialMessage(
                        body: message,
                        idempotencyKey: UUID().uuidString,
                        attachmentIDs: attachmentIDs
                    )
                )
                conversationID = created.conversation.summary.conversationID
                guard let initialInput = created.initialInput else {
                    throw ChatSubmitError.missingInitialInput
                }
                runInputID = initialInput.runInputID
            }

            showTranscript(of: conversationID)
            selectedConversationID = conversationID
            pendingStartConversationID = conversationID
            let run = try await backend.startConversationRun(
                workspaceID: workspaceID,
                conversationID: conversationID,
                engineID: engineID,
                modelID: selectedModelID,
                modeID: modeID,
                thoughtLevelID: effectiveThoughtLevelID,
                runInputID: runInputID
            )
            pendingStartConversationID = nil
            if terminalRunIDs.remove(run.run.runID) != nil {
                await loadDetail(run.run.conversationID, backend: backend)
            } else {
                await loadDetail(run.run.conversationID, backend: backend)
                await reconcilePermissionRegistration(backend: backend, focused: true)
                mergeActiveRun(run.run)
            }
            let list = try await backend.listConversations(workspaceID: workspaceID)
            apply(conversations: list.conversations)
            isSubmitting = false
            Self.logger.info("submitted Chat run conversationID=\(run.run.conversationID.rawValue, privacy: .public)")
            return true
        } catch {
            pendingStartConversationID = nil
            isSubmitting = false
            if let (conversationID, needs) = clearedSignIns {
                mcpSignIn?.restoreNeeds(needs, for: conversationID)
            }
            errorText = error.localizedDescription
            if let selectedConversationID {
                await loadDetail(selectedConversationID, backend: backend)
            }
            Self.logger.error("failed to submit Chat run: \(error.localizedDescription, privacy: .private)")
            return false
        }
    }

    // MARK: - Queued messages

    /// What the composer's queue pill shows for the conversation on screen.
    var queuedMessages: [QueuedMessage] {
        guard let selectedConversationID else { return [] }
        return messageQueue[selectedConversationID]
    }

    /// Writes the whole queue for the workspace on screen. Before `configure`
    /// names one there is nothing worth persisting: the queue can only be
    /// non-empty once a conversation is selected, which needs a workspace.
    private func persistQueue() {
        guard let workspaceID else { return }
        messageQueue.persist(to: defaults, workspaceID: workspaceID)
    }

    /// Holds a message behind the working run. Return does this; steering is the
    /// deliberate Command-Return gesture.
    ///
    /// Routing comes from `submissionRouting` as one value: while a run is live
    /// the message belongs to that run's Engine and that run's options, and the
    /// picker may have moved to a different Engine since the turn started.
    @discardableResult
    func enqueue(_ draft: String, attachments: [ConversationAttachment] = []) -> Bool {
        let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty || !attachments.isEmpty,
              let conversationID = selectedConversationID,
              let routing = submissionRouting
        else { return false }
        messageQueue.append(
            QueuedMessage(
                body: body,
                attachmentIDs: attachments.map(\.attachmentID),
                routing: routing
            ),
            to: conversationID
        )
        persistQueue()
        return true
    }

    /// Drops one queued message. Removing every message is how a queue is
    /// discarded; there is no separate clear verb.
    func removeQueued(_ id: QueuedMessage.ID, in conversationID: ConversationID) {
        messageQueue.take(id, from: conversationID)
        persistQueue()
    }

    /// Promotes one message past the rest of the queue, which is why the queue
    /// needs no reorder affordance. With a run still working it steers, so the
    /// message cuts into the turn rather than waiting behind it.
    func sendQueuedNow(
        _ id: QueuedMessage.ID,
        in conversationID: ConversationID,
        backend: DiffReviewModel
    ) async {
        // Not while the queue is already draining. The turn has finished, the
        // head is mid-flight, and a second send racing it means whichever loses
        // is rejected for a run it did not know had started — with one of the
        // two messages already published to the transcript out of order.
        guard !drainingConversationIDs.contains(conversationID),
              let message = messageQueue.take(id, from: conversationID)
        else { return }
        persistQueue()
        do {
            try await sendPromoted(message, to: conversationID, backend: backend)
        } catch {
            // Nothing reached Core, so the message goes back rather than
            // disappearing with only a banner to explain it. To the head, not
            // to where it was: the reader just said this one cannot wait, and
            // the head is where they will look for it.
            messageQueue.restore(message, toHeadOf: conversationID)
            persistQueue()
            if conversationID == selectedConversationID {
                errorText = error.localizedDescription
            }
            Self.logger.error(
                "failed to send a queued message now: \(error.localizedDescription, privacy: .private)"
            )
            return
        }
        await refreshAfterSend(conversationID, backend: backend)
    }

    /// Sends a promoted message into the conversation it was queued for.
    ///
    /// Deliberately not `submit`, which resolves the conversation, Engine and
    /// options from the selection. This runs from a popover action whose task
    /// can land after the reader has moved to another conversation — and by
    /// then the message has already left the first one's queue, so a
    /// selection-based send would post it into the second.
    private func sendPromoted(
        _ message: QueuedMessage,
        to conversationID: ConversationID,
        backend: DiffReviewModel
    ) async throws {
        guard let workspaceID else { throw ChatSubmitError.noWorkspace }
        guard let activeRun = canonicalLiveRun(in: conversationID) else {
            _ = try await postAndStart(message, to: conversationID, backend: backend)
            return
        }
        let steered = try await backend.steerConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            runID: activeRun.runID,
            message: message.body,
            attachmentIDs: message.attachmentIDs,
            idempotencyKey: message.id.uuidString
        )
        if steered.run.runID != activeRun.runID {
            liveRuns.removeValue(forKey: activeRun.runID)
            // The refresh below re-derives this run's text from persisted
            // events, which is the authority on it.
        }
        mergeActiveRun(steered.run)
    }

    /// The working run in one named conversation, which is not necessarily the
    /// one on screen. `selectedCanonicalLiveRun` answers only for the selection.
    private func canonicalLiveRun(in conversationID: ConversationID) -> ChatLiveRunState? {
        liveRuns.values.first { run in
            run.conversationID == conversationID && (run.status == .queued || run.status == .running)
        }
    }

    /// Claims the next queued message for a run that just reached `status`.
    ///
    /// Only `completed` claims. Stopping a turn is how the reader takes the
    /// wheel back and must not immediately fire the next message, a failed run
    /// leaves a conversation the reader has to look at, and `interrupted` means
    /// a steer's follow-up run is already starting.
    ///
    /// Synchronous, and called before the terminal-event handler awaits
    /// anything. A drain otherwise spends a transcript reload deciding whether
    /// to send, and in that window the composer sees no live run and would
    /// happily start a competing one — which Core then refuses as
    /// `runAlreadyActive`. Claiming first means the composer is already in its
    /// sending state by the time control leaves this function.
    private func claimDrain(for conversationID: ConversationID, after status: ConversationRunStatus) -> Bool {
        guard ChatMessageQueue.drains(after: status),
              !messageQueue[conversationID].isEmpty,
              drainingConversationIDs.insert(conversationID).inserted
        else { return false }
        return true
    }

    /// Whether the conversation on screen is mid-drain, which the composer wears
    /// as its sending state.
    ///
    /// Derived from the claim rather than mirrored into `isSubmitting`, which is
    /// one flag for the whole workspace: a drain that set it would keep it set
    /// after the reader moved to another conversation, disabling a composer that
    /// has nothing to do with the send — and clearing it would answer for
    /// whichever conversation happened to be on screen at release.
    var isDrainingSelectedConversation: Bool {
        guard let selectedConversationID else { return false }
        return drainingConversationIDs.contains(selectedConversationID)
    }

    /// Sends the head of a claimed conversation's queue, then keeps going for as
    /// long as no terminal event will arrive to restart it.
    ///
    /// The loop exists for the run that reaches a terminal state before its own
    /// `startConversationRun` returns. That run's terminal event is the only one
    /// that would drain the message behind it, and it fires while this claim is
    /// still held, so `claimDrain` refuses it. Without picking the next message
    /// up here the rest of the queue waits for an event that has already been
    /// and gone.
    private func drainQueue(for conversationID: ConversationID, backend: DiffReviewModel) async {
        var isClaimed = true
        func release() {
            guard isClaimed else { return }
            isClaimed = false
            drainingConversationIDs.remove(conversationID)
        }
        defer { release() }

        while true {
            guard let message = messageQueue.takeHead(from: conversationID) else { return }
            persistQueue()
            let finishedDuringStart: Bool
            do {
                finishedDuringStart = try await postAndStart(
                    message,
                    to: conversationID,
                    backend: backend
                )
            } catch {
                // Nothing durable holds this message, so putting it back is all
                // that stands between a failed send and a lost one.
                messageQueue.restore(message, toHeadOf: conversationID)
                persistQueue()
                // Where the reader is now, not where they were when this was
                // claimed: a banner belongs to the conversation on screen.
                if conversationID == selectedConversationID {
                    errorText = error.localizedDescription
                }
                Self.logger.error(
                    "failed to drain queued message: \(error.localizedDescription, privacy: .private)"
                )
                return
            }
            // The run is live, so the claim has done its job. Released here
            // rather than on return: a short turn can finish during the refresh
            // below, and its terminal event has to find the queue free or the
            // next message never drains.
            release()
            await refreshAfterSend(conversationID, backend: backend)
            // Re-claim rather than recurse, and only when this run's own event
            // is the one that was swallowed. Any other finish still arrives
            // normally and drains through the terminal-event handler.
            guard finishedDuringStart,
                  !messageQueue[conversationID].isEmpty,
                  drainingConversationIDs.insert(conversationID).inserted
            else { return }
            isClaimed = true
        }
    }

    /// Posts and runs one queued message without reading the selection.
    ///
    /// Deliberately not `submit`: that path resolves the conversation, engine
    /// and session options from what is on screen, and a drain has to use what
    /// was on screen when the message was queued — the reader may have moved on.
    /// The message's own id is the idempotency key, so a retry after a failed
    /// `startRun` replays the post rather than duplicating it.
    /// Posts a queued message and starts its run.
    ///
    /// Returns as soon as the run exists, which is the point the drain claim is
    /// protecting: from here the composer sees a live run and can no longer
    /// start a competing one. Refreshing the panes is deliberately not part of
    /// this — see `refreshAfterSend`.
    ///
    /// Returns whether the run had already reached a terminal state by the time
    /// its own start RPC came back, which the caller needs because that run's
    /// terminal event has already been delivered and discarded.
    @discardableResult
    private func postAndStart(
        _ message: QueuedMessage,
        to conversationID: ConversationID,
        backend: DiffReviewModel
    ) async throws -> Bool {
        guard let workspaceID else { throw ChatSubmitError.noWorkspace }
        let posted = try await backend.postConversationMessage(
            workspaceID: workspaceID,
            conversationID: conversationID,
            message: message.body,
            attachmentIDs: message.attachmentIDs,
            idempotencyKey: message.id.uuidString
        )
        await settleAlwaysAllowToolActions()
        pendingStartConversationID = conversationID
        defer { pendingStartConversationID = nil }
        let run = try await backend.startConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            engineID: message.engineID,
            modelID: message.modelID,
            modeID: message.modeID,
            thoughtLevelID: message.thoughtLevelID,
            runInputID: posted.input.runInputID
        )
        // A short turn can publish its terminal event before this response
        // lands. `mergeActiveRun` would then be merging a run that is already
        // over — it declines, correctly, but the id has to be taken out of
        // `terminalRunIDs` or it is held forever. The ordinary submit path
        // makes the same check for the same reason.
        guard terminalRunIDs.remove(run.run.runID) == nil else { return true }
        mergeActiveRun(run.run)
        return false
    }

    /// Draws a just-sent message into the panes: its bubble in the transcript,
    /// its conversation's new preview in the sidebar.
    ///
    /// Two round trips, and nothing downstream waits on them, which is why a
    /// drain releases its claim before calling this. Holding the claim here
    /// would let a turn that finishes inside the window find the queue still
    /// claimed and stall it.
    private func refreshAfterSend(_ conversationID: ConversationID, backend: DiffReviewModel) async {
        await loadDetailIfSelected(conversationID, backend: backend)
        guard let workspaceID,
              let list = try? await backend.listConversations(workspaceID: workspaceID)
        else { return }
        apply(conversations: list.conversations)
    }

    func cancelSelectedRun(backend: DiffReviewModel) async {
        guard let workspaceID,
              let run = selectedLiveRun
        else { return }
        do {
            let result = try await backend.cancelConversationRun(
                workspaceID: workspaceID,
                conversationID: run.conversationID,
                runID: run.runID
            )
            liveRuns.removeValue(forKey: run.runID)
            _ = result
            await loadDetail(run.conversationID, backend: backend)
            resolvePermissions(runID: run.runID, outcome: .cancelled)
        } catch {
            errorText = error.localizedDescription
        }
    }

    /// The card's Stop. The task's stopped state arrives as a transcript
    /// revision.
    func stopBackgroundTask(runID: ConversationRunID, asyncTaskID: String, backend: DiffReviewModel) async {
        guard let workspaceID, let conversationID = selectedConversationID else { return }
        do {
            let result = try await backend.stopBackgroundTask(
                workspaceID: workspaceID,
                conversationID: conversationID,
                runID: runID,
                asyncTaskID: asyncTaskID
            )
            // Otherwise Stop would look ignored. The task's own state, ended
            // or lost, follows as a transcript revision.
            if !result.stopped {
                errorText = "The background task couldn't be stopped. It may have already ended."
            }
        } catch {
            errorText = error.localizedDescription
        }
    }

    func resolveDelivery(
        _ input: ConversationRunInput,
        retry: Bool,
        backend: DiffReviewModel
    ) async {
        guard let workspaceID, let conversationID = selectedConversationID else { return }
        // Each press spawns its own Task, so a double-press can arrive before
        // the disabled button re-renders. The loser would only earn a typed
        // rejection from Core and an error banner the user cannot act on.
        guard !isMutating else { return }
        isMutating = true
        errorText = nil
        do {
            if input.status == .pending && input.kind == .retry {
                try await startPendingRetry(
                    input,
                    originalInput: nil,
                    workspaceID: workspaceID,
                    conversationID: conversationID,
                    backend: backend
                )
            } else {
                let resolution: RunInputDeliveryResolution = retry
                    ? .retry(idempotencyKey: UUID().uuidString)
                    : .markDelivered
                let resolved = try await backend.resolveConversationRunInputDelivery(
                    workspaceID: workspaceID,
                    conversationID: conversationID,
                    runInputID: input.runInputID,
                    resolution: resolution
                )
                if let retryInput = resolved.retryInput {
                    try await startPendingRetry(
                        retryInput,
                        originalInput: input,
                        workspaceID: workspaceID,
                        conversationID: conversationID,
                        backend: backend
                    )
                }
            }
            await loadDetail(conversationID, backend: backend)
            isMutating = false
        } catch {
            let message = error.localizedDescription
            // Resolution is durable before launch. Refresh so a persisted pending
            // retry remains visible and can be started again after any launch failure.
            await loadDetail(conversationID, backend: backend)
            isMutating = false
            errorText = message
        }
    }

    private func startPendingRetry(
        _ retryInput: ConversationRunInput,
        originalInput: ConversationRunInput?,
        workspaceID: WorkspaceID,
        conversationID: ConversationID,
        backend: DiffReviewModel
    ) async throws {
        let originalInput = originalInput ?? retryInput.retryOfInputID.flatMap { originalID in
            selectedDetail?.latestRunInputPage.runInputs.first {
                $0.runInputID == originalID
            }
        }
        let originalRun = originalInput?.targetRunID.flatMap { targetID in
            selectedDetail?.runs.first { $0.runID == targetID }
        }
        let engineID: EngineID
        let modelID: String?
        let modeID: String?
        let thoughtLevelID: String?
        if let originalRun {
            engineID = originalRun.engineID
            (modelID, modeID, thoughtLevelID) = retrySelections(for: originalRun)
        } else {
            guard let selectedEngine else { throw ChatSubmitError.noEngine }
            engineID = selectedEngine.engineID
            modelID = selectedModelID
            modeID = self.modeID(for: conversationID)
            thoughtLevelID = effectiveThoughtLevelID
        }
        await settleAlwaysAllowToolActions()
        let started = try await backend.startConversationRun(
            workspaceID: workspaceID,
            conversationID: conversationID,
            engineID: engineID,
            modelID: modelID,
            modeID: modeID,
            thoughtLevelID: thoughtLevelID,
            runInputID: retryInput.runInputID
        )
        mergeActiveRun(started.run)
    }

    func loadOlderMessages(backend: DiffReviewModel) async {
        guard !isLoadingOlderMessages,
              let workspaceID,
              let detail = selectedDetail,
              let cursor = transcriptStore.nextCursor
        else { return }
        isLoadingOlderMessages = true
        let requestedInstance = transcriptStore.coreInstanceID
        do {
            let result = try await backend.listConversationTranscript(
                workspaceID: workspaceID,
                conversationID: detail.summary.conversationID,
                cursor: cursor
            )
            guard selectedConversationID == detail.summary.conversationID,
                  transcriptStore.coreInstanceID == requestedInstance,
                  result.coreInstanceID == requestedInstance else {
                isLoadingOlderMessages = false
                return
            }
            transcriptStore.merge(result, conversationID: detail.summary.conversationID, older: true)
            transcriptStore.loadBodies(workspaceID: workspaceID, backend: backend)
            isLoadingOlderMessages = false
        } catch {
            isLoadingOlderMessages = false
            errorText = error.localizedDescription
        }
    }

    /// Loads `thread` of `conversationID` into the Subagent tab's store for
    /// the tab view `owner`. Another thread starts clean, without the last
    /// one's error.
    func loadSubagentThread(_ thread: String, in conversationID: ConversationID, owner: UUID, backend: DiffReviewModel) async {
        subagentOwner = owner
        if subagentTranscript.conversationID != conversationID || subagentTranscript.threadScope != thread {
            subagentLoadError = nil
        }
        subagentTranscript.select(conversationID, thread: thread)
        await refreshSubagentThread(backend: backend)
    }

    /// Rescans the Subagent tab's thread in its own conversation: its newest
    /// page, or after a gap, every page it has loaded. Nothing while the tab
    /// is closed.
    func refreshSubagentThread(backend: DiffReviewModel) async {
        guard let workspaceID, let conversationID = subagentTranscript.conversationID,
              let thread = subagentTranscript.threadScope else { return }
        do {
            try await subagentTranscript.resync(conversationID) { cursor in
                try await backend.listConversationTranscript(
                    workspaceID: workspaceID, conversationID: conversationID,
                    cursor: cursor, subagentThreadID: thread
                )
            }
            guard subagentTranscript.threadScope == thread else { return }
            subagentLoadError = nil
            subagentTranscript.loadBodies(workspaceID: workspaceID, backend: backend)
        } catch {
            guard subagentTranscript.threadScope == thread else { return }
            subagentLoadError = error.localizedDescription
        }
    }

    /// The Subagent tab view `owner` went away: its store stops following
    /// the thread. A view that already replaced it keeps the store.
    func releaseSubagentThread(owner: UUID) {
        guard subagentOwner == owner else { return }
        subagentOwner = nil
        subagentTranscript.release()
        subagentLoadError = nil
    }

    private(set) var isLoadingOlderSubagentRows = false

    func loadOlderSubagentRows(backend: DiffReviewModel) async {
        guard !isLoadingOlderSubagentRows,
              let workspaceID, let conversationID = subagentTranscript.conversationID,
              let thread = subagentTranscript.threadScope,
              let cursor = subagentTranscript.nextCursor else { return }
        let requestedInstance = subagentTranscript.coreInstanceID
        isLoadingOlderSubagentRows = true
        defer { isLoadingOlderSubagentRows = false }
        do {
            let page = try await backend.listConversationTranscript(
                workspaceID: workspaceID, conversationID: conversationID,
                cursor: cursor, subagentThreadID: thread
            )
            guard subagentTranscript.threadScope == thread,
                  subagentTranscript.coreInstanceID == requestedInstance,
                  page.coreInstanceID == requestedInstance else { return }
            subagentTranscript.merge(page, conversationID: conversationID, older: true)
            subagentTranscript.loadBodies(workspaceID: workspaceID, backend: backend)
        } catch {
            subagentLoadError = error.localizedDescription
        }
    }

    func resolvePermission(_ permission: ChatPendingPermission, optionID: String?) {
        pendingPermissions.removeAll { $0.id == permission.id }
        if let optionID {
            permission.resolve(.selected(optionID: optionID))
        } else {
            permission.resolve(.cancelled)
        }
    }

    func resolveQuestion(_ question: ChatPendingQuestion, _ outcome: AgentQuestionOutcome) {
        pendingQuestions.removeAll { $0.id == question.id }
        question.resolve(outcome)
    }

    /// Holds an agent's form until the user answers it, the run ends, or core
    /// withdraws it, which cancels this task.
    private func enqueueQuestion(_ request: AgentAskQuestionParams) async -> AgentQuestionOutcome {
        guard request.workspaceID == workspaceID,
              let conversationID = request.conversationID,
              conversationID == selectedConversationID
                || conversations.contains(where: { $0.conversationID == conversationID })
        else {
            return .cancelled
        }
        let questionID = UUID()
        return await withTaskCancellationHandler {
            if Task.isCancelled { return .cancelled }
            return await withCheckedContinuation { continuation in
                pendingQuestions.append(ChatPendingQuestion(
                    id: questionID,
                    request: request,
                    continuation: continuation
                ))
            }
        } onCancel: { [weak self] in
            Task { @MainActor in
                self?.cancelQuestions { $0.id == questionID }
            }
        }
    }

    private func cancelQuestions(matching predicate: (ChatPendingQuestion) -> Bool) {
        let matching = pendingQuestions.filter(predicate)
        guard !matching.isEmpty else { return }
        pendingQuestions.removeAll(where: predicate)
        for question in matching { question.resolve(.cancelled) }
    }

    private func enqueuePermission(_ request: AgentRequestPermissionParams) async -> AgentPermissionOutcome {
        guard request.workspaceID == workspaceID,
              let conversationID = request.conversationID,
              let runID = request.runID,
              // A new conversation's first turn can ask before the list
              // that would name it is refreshed; it is the selected one.
              conversationID == selectedConversationID
                || conversations.contains(where: { $0.conversationID == conversationID })
        else {
            return .cancelled
        }
        if liveRuns[runID] == nil {
            liveRuns[runID] = ChatLiveRunState(
                runID: runID,
                conversationID: conversationID,
                status: .running,
                origin: persistedOrigin(runID)
            )
        }
        let permissionID = UUID()
        return await withTaskCancellationHandler {
            if Task.isCancelled { return .cancelled }
            return await withCheckedContinuation { continuation in
                let permission = ChatPendingPermission(
                    id: permissionID,
                    request: request,
                    continuation: continuation
                )
                pendingPermissions.append(permission)
            }
        } onCancel: { [weak self] in
            Task { @MainActor in
                self?.resolvePermission(id: permissionID, outcome: .cancelled)
            }
        }
    }

    private func subscribeToEventsIfNeeded(backend: DiffReviewModel) async {
        guard let workspaceID,
              subscribedWorkspaceID != workspaceID
        else { return }
        if let eventSubscription {
            await backend.unsubscribeFromBackendEvents(eventSubscription)
            self.eventSubscription = nil
        }
        guard self.workspaceID == workspaceID else { return }
        let subscription = await backend.subscribeToBackendEvents(
            filter: AgentEventFilter(
                methods: [
                    "conversation.eventDelta", "conversation.transcriptDelta",
                    "conversation.transcriptRecoveryRequired", "conversation.listChanged",
                ],
                workspaceID: workspaceID
            )
        ) { [weak self, weak backend] notification in
            guard let self, let backend else { return }
            switch notification.method {
            case "conversation.transcriptRecoveryRequired":
                await self.markCachedTranscriptsStale()
                if let selected = await self.selectedConversationID {
                    Task { @MainActor [weak self, weak backend] in
                        guard let self, let backend else { return }
                        await self.resyncTranscript(selected, backend: backend)
                    }
                }
                Task { @MainActor [weak self, weak backend] in
                    guard let self, let backend else { return }
                    await self.refreshSubagentThread(backend: backend)
                }
            case "conversation.transcriptDelta":
                do {
                    let delta = try JSONDecoder().decode(ProtocolNotification<ConversationTranscriptDeltaParams>.self, from: notification.payload).params
                    let refresh = await self.mergeTranscriptDelta(delta, workspaceID: workspaceID, backend: backend)
                    if refresh {
                        Task { @MainActor [weak self, weak backend] in
                            guard let self, let backend else { return }
                            await self.resyncTranscript(delta.conversationID, backend: backend)
                        }
                    }
                    let threadRefresh = await self.subagentTranscript.merge(delta)
                    await self.subagentTranscript.loadBodies(workspaceID: workspaceID, backend: backend)
                    if threadRefresh {
                        Task { @MainActor [weak self, weak backend] in
                            guard let self, let backend else { return }
                            await self.refreshSubagentThread(backend: backend)
                        }
                    }
                } catch {
                    Self.logger.error("Could not decode conversation.transcriptDelta; refreshing selected transcript")
                    await self.markCachedTranscriptsStale()
                    if let selected = await self.selectedConversationID {
                        Task { @MainActor [weak self, weak backend] in
                            guard let self, let backend else { return }
                            await self.resyncTranscript(selected, backend: backend)
                        }
                    }
                    Task { @MainActor [weak self, weak backend] in
                        guard let self, let backend else { return }
                        await self.refreshSubagentThread(backend: backend)
                    }
                }
            case "conversation.eventDelta":
                await self.receiveConversationEvent(notification, backend: backend)
            case "conversation.listChanged":
                // Core opened a conversation itself (a delegated child), or a
                // child's work changed.
                Task { @MainActor [weak self, weak backend] in
                    guard let self, let backend else { return }
                    await self.relistConversations(workspaceID: workspaceID, backend: backend)
                }
            default:
                Self.logger.warning("Ignoring unsupported conversation notification method")
            }
        }
        guard !backendHandlersRetired, self.workspaceID == workspaceID else {
            await backend.unsubscribeFromBackendEvents(subscription)
            return
        }
        if let superseded = eventSubscription {
            await backend.unsubscribeFromBackendEvents(superseded)
        }
        eventSubscription = subscription
        subscribedWorkspaceID = workspaceID
        namedAgentsObserver?.cancel()
        namedAgentsObserver = Task { [weak self, weak backend] in
            for await _ in NotificationCenter.default.notifications(named: NamedAgentsSettingsModel.changedNotification) {
                guard let self, let backend, let workspaceID = self.workspaceID else { return }
                await self.relistConversations(workspaceID: workspaceID, backend: backend)
            }
        }
    }

    /// Replaces the conversation list with core's, unless the window let go
    /// of core, the workspace changed, or a later re-list started meanwhile.
    private func relistConversations(workspaceID: WorkspaceID, backend: DiffReviewModel) async {
        guard !backendHandlersRetired else { return }
        listChangedGeneration += 1
        let generation = listChangedGeneration
        guard let list = try? await backend.listConversations(workspaceID: workspaceID),
              !backendHandlersRetired,
              self.workspaceID == workspaceID,
              generation == listChangedGeneration
        else { return }
        apply(conversations: list.conversations)
    }

    /// Merges into the shown store or a cached one. Only the shown store
    /// reads bodies and resyncs now (true asks for it); a cached one marks
    /// itself for a resync on return.
    func mergeTranscriptDelta(
        _ delta: ConversationTranscriptDeltaParams, workspaceID: WorkspaceID, backend: DiffReviewModel
    ) -> Bool {
        if transcriptStore.conversationID == delta.conversationID {
            let refresh = transcriptStore.merge(delta)
            transcriptStore.loadBodies(workspaceID: workspaceID, backend: backend)
            return refresh
        }
        if let session = transcriptCache.session(for: delta.conversationID), session.store.merge(delta) {
            session.needsResync = true
        }
        return false
    }

    /// Core lost track of what it sent: every store out of view resyncs on
    /// return; the shown one resyncs now.
    private func markCachedTranscriptsStale() {
        transcriptCache.markStale(except: transcriptStore.conversationID)
    }

    private func receiveConversationEvent(
        _ notification: AgentNotification,
        backend: DiffReviewModel
    ) {
        let event: ConversationEventDeltaParams
        do {
            event = try JSONDecoder().decode(ProtocolNotification<ConversationEventDeltaParams>.self,
                from: notification.payload).params
        } catch {
            Self.logger.error("Could not decode conversation.eventDelta; refreshing selected conversation")
            if let selectedConversationID {
                Task { await loadDetailIfSelected(selectedConversationID, backend: backend) }
            }
            return
        }
        guard event.workspaceID == workspaceID else { return }
        if let lastSequence = lastEventSequenceByRun[event.runID], event.sequence <= lastSequence {
            return
        }
        lastEventSequenceByRun[event.runID] = event.sequence
        switch event.eventKind {
        case .runStatus:
            let rawStatus = event.payload.objectString("status")
            guard let status = rawStatus.flatMap(ConversationRunStatus.init(rawValue:)) else {
                // A status this build doesn't know (a newer backend) must not tear the
                // run down: marking it terminal would drop every subsequent event.
                Self.logger.warning("ignoring unknown run status \(rawStatus ?? "<missing>", privacy: .public)")
                return
            }
            if status == .queued || status == .running {
                guard !terminalRunIDs.contains(event.runID) else { return }
                withLiveRun(
                    event.runID,
                    conversationID: event.conversationID,
                    status: status
                ) { run in
                    run.status = ChatRunStatusReconciler.reconcile(
                        current: run.status,
                        observed: status
                    )
                    if let origin = event.payload.objectString("origin").flatMap(RunOrigin.init(rawValue:)) {
                        run.origin = origin
                    }
                }
            } else {
                // Remember every finished run: `submit` checks this to catch a run that
                // finished while startConversationRun was still in flight, and the event
                // handlers above use it to drop stale queued/update events.
                terminalRunIDs.insert(event.runID)
                transcriptStore(for: event.conversationID)?.endSessionActivity(event.runID)
                liveRuns.removeValue(forKey: event.runID)
                // Out of view, a finished run starts its conversation's idle
                // clock over, and frees it for eviction.
                if event.conversationID != transcriptStore.conversationID {
                    transcriptCache.markIdle(event.conversationID)
                    evictTranscripts()
                }
                resolvePermissions(runID: event.runID, outcome: .cancelled)
                // Only banner failures for the conversation the user is looking at;
                // a background conversation's error must not surface on another thread.
                if status == .failed,
                   event.conversationID == selectedConversationID,
                   let message = event.payload.objectValue("error")?.objectString("message") {
                    errorText = message + (visionHint(forFailedRun: event.runID) ?? "")
                }
                // A run that stopped for want of credentials has just changed the
                // engine's auth state; the list this pane is holding still says
                // the engine was fine, which is precisely the state in which no
                // sign-in is offered. A run refused because its model/mode/effort
                // is no longer offered is the same shape: the handshake that
                // refused it also refreshed what the engine advertises, and
                // re-listing is what drops the stale pick from the composer.
                let failureKind = status == .failed
                    ? event.payload.objectValue("error")?.objectString("kind")
                    : nil
                let engineListStale = failureKind == "authRequired"
                    || failureKind == "selectionUnavailable"
                // Claimed before the first await; see `claimDrain`.
                let drains = claimDrain(for: event.conversationID, after: status)
                Task {
                    await loadDetailIfSelected(event.conversationID, backend: backend)
                    if engineListStale {
                        await refreshEngines(backend: backend)
                    }
                    if let workspaceID,
                       let list = try? await backend.listConversations(workspaceID: workspaceID) {
                        apply(conversations: list.conversations)
                    }
                    // Last, so the finished turn is already in the transcript
                    // before the run answering the next message starts under it.
                    if drains {
                        await drainQueue(for: event.conversationID, backend: backend)
                    }
                }
            }
        case .permissionRequested, .permissionResolved:
            Task { await loadDetailIfSelected(event.conversationID, backend: backend) }
        case .steerRequested, .steerDelivered, .runInterrupted:
            Task { await loadDetailIfSelected(event.conversationID, backend: backend) }
        case .usage:
            // A stopped run's late answer: patch its trailer in place.
            guard let usage = try? JSONDecoder().decode(ConversationRunUsage.self,
                    from: JSONEncoder().encode(event.payload)),
                  selectedDetail?.summary.conversationID == event.conversationID
            else { break }
            Self.applyRunUsage(usage, runID: event.runID, to: &selectedDetail!.latestRunPage.runs)
        case .sessionMetadata:
            do {
                let incoming = try JSONDecoder().decode(ConversationSessionMetadata.self,
                    from: JSONEncoder().encode(event.payload))
                if let title = incoming.title {
                    applyHarnessTitle(title, conversationID: event.conversationID)
                }
                if let modeID = incoming.currentModeId {
                    confirmMode(modeID, conversationID: event.conversationID)
                }
                if let usage = incoming.usage {
                    recordContextUsage(.init(used: usage.used, size: usage.size),
                                       conversationID: event.conversationID)
                }
                if let commands = incoming.commands {
                    // The event names no engine. A run the detail doesn't
                    // list yet is read back from its session after a reload.
                    if let engineID = selectedDetail?.runs.first(where: { $0.runID == event.runID })?.engineID {
                        recordCommands(commands, engineID: engineID, conversationID: event.conversationID)
                    } else {
                        Task { await loadDetailIfSelected(event.conversationID, backend: backend) }
                    }
                }
            } catch {
                Self.logger.error("Could not decode typed session metadata")
            }
        }
    }

    /// Start time from the loaded detail, for a live run first seen through a
    /// notification rather than its start response.
    private func persistedStartedAt(_ runID: ConversationRunID) -> String? {
        selectedDetail?.runs.first(where: { $0.runID == runID })?.startedAt
    }

    /// A run first seen through an update rather than its status event: the
    /// detail says whether the agent started it on its own.
    private func persistedOrigin(_ runID: ConversationRunID) -> RunOrigin {
        selectedDetail?.runs.first(where: { $0.runID == runID })?.origin ?? .user
    }

    /// Merge an RPC snapshot into the notification-owned live state. Core can
    /// publish `running` and agent updates before the original start RPC returns
    /// its queued snapshot; preserving the existing timeline and reconciling the
    /// phase prevents that older response from making the UI say it is starting.
    private func mergeActiveRun(_ summary: ConversationRunSummary) {
        guard (summary.status == .queued || summary.status == .running),
              !terminalRunIDs.contains(summary.runID)
        else { return }
        withLiveRun(
            summary.runID,
            conversationID: summary.conversationID,
            status: summary.status
        ) { run in
            run.status = ChatRunStatusReconciler.reconcile(
                current: run.status,
                observed: summary.status
            )
            run.startedAt = run.startedAt ?? summary.startedAt
            run.origin = summary.origin
        }
    }

    /// Adopts a harness-generated session title into the sidebar row, the toolbar
    /// title, and the selected detail without waiting for a full reload.
    private func applyHarnessTitle(_ title: String, conversationID: ConversationID) {
        if let index = conversations.firstIndex(where: { $0.conversationID == conversationID }) {
            conversations[index].title = title
        }
        if selectedDetail?.summary.conversationID == conversationID {
            selectedDetail?.summary.title = title
        }
    }

    private static func harnessCleanupNotice(
        _ cleanup: [HarnessSessionCleanup]
    ) -> String? {
        let incomplete = cleanup.filter { $0.status != .deleted }
        guard !incomplete.isEmpty else { return nil }
        return incomplete.compactMap { item in
            switch item.status {
            case .unsupported:
                return "\(item.engineID.rawValue) does not support deleting its harness session."
            case .failed:
                let detail = item.message.map { ": \($0)" } ?? "."
                return "Could not remove the \(item.engineID.rawValue) harness session\(detail)"
            case .deleted:
                return nil
            }
        }
        .joined(separator: " ")
    }

    /// Removes and resolves every pending permission matching `predicate`. Single
    /// home for the filter/remove/resolve sequence the callers below all shared.
    private func resolvePermissions(
        outcome: AgentPermissionOutcome,
        matching predicate: (ChatPendingPermission) -> Bool
    ) {
        let matching = pendingPermissions.filter(predicate)
        guard !matching.isEmpty else { return }
        pendingPermissions.removeAll(where: predicate)
        for permission in matching { permission.resolve(outcome) }
    }

    /// Also cancels the run's questions: whatever ends a run's approvals ends
    /// its forms too.
    private func resolvePermissions(runID: ConversationRunID, outcome: AgentPermissionOutcome) {
        resolvePermissions(outcome: outcome) { $0.request.runID == runID }
        cancelQuestions { $0.request.runID == runID }
    }

    private func resolvePermissions(conversationID: ConversationID, outcome: AgentPermissionOutcome) {
        resolvePermissions(outcome: outcome) { $0.request.conversationID == conversationID }
        cancelQuestions { $0.request.conversationID == conversationID }
    }

    private func resolvePermission(id: UUID, outcome: AgentPermissionOutcome) {
        resolvePermissions(outcome: outcome) { $0.id == id }
    }

    private func resolveAllPermissions(_ outcome: AgentPermissionOutcome) {
        resolvePermissions(outcome: outcome) { _ in true }
        cancelQuestions { _ in true }
    }

    private func load(workspaceID: WorkspaceID, backend: DiffReviewModel) async {
        isLoading = true
        errorText = nil
        do {
            let engines = try await backend.listEngines(workspaceID: workspaceID).engines
            let uninstalled = try? await backend.uninstalledEngineIDs()
            guard self.workspaceID == workspaceID, !Task.isCancelled else { return }
            if let uninstalled { uninstalledEngineIDs = uninstalled }
            self.engines = engines
            if selectedEngineID == nil {
                // Readiness-ranked, not id-ranked: on a fresh machine only the
                // adapter the user just got is any use, and a pill defaulting
                // to a disabled row reads as the wrong agent being picked.
                selectedEngineID = EngineSummary.defaultSelection(from: engines)?.engineID
            }
            selectReadyLandingEngineIfNeeded()
            restoreEngineSelections()

            let conversations = try await backend.listConversations(workspaceID: workspaceID).conversations
            guard self.workspaceID == workspaceID, !Task.isCancelled else { return }
            apply(conversations: conversations)
            if let selectedConversationID {
                await loadDetail(selectedConversationID, backend: backend)
            } else {
                selectedDetail = nil
            }
            didCompleteInitialLoad = true
            isLoading = false
        } catch {
            guard self.workspaceID == workspaceID, !Task.isCancelled else { return }
            isLoading = false
            errorText = error.localizedDescription
            Self.logger.error("failed to load Chat workspace: \(error.localizedDescription, privacy: .private)")
        }
    }

    /// A missed delta can revise an old tool or plan at any position. Walk all
    /// position pages; revision merging preserves updates received during the scan.
    /// Call outside the ordered notification reader: waiting for an RPC in that
    /// reader prevents it from routing the response needed to finish this scan.
    private func resyncTranscript(_ conversationID: ConversationID, backend: DiffReviewModel) async {
        guard selectedConversationID == conversationID, let workspaceID else { return }
        do {
            try await transcriptStore.resync(conversationID) { cursor in
                try await backend.listConversationTranscript(workspaceID: workspaceID,
                    conversationID: conversationID, cursor: cursor)
            }
            transcriptCache.session(for: conversationID)?.needsResync = false
            guard selectedConversationID == conversationID, self.workspaceID == workspaceID else { return }
            transcriptLoadError = nil
            transcriptStore.loadBodies(workspaceID: workspaceID, backend: backend)
        } catch {
            guard selectedConversationID == conversationID else { return }
            transcriptLoadError = error.localizedDescription
        }
    }

    func retryTranscript(backend: DiffReviewModel) async {
        guard let selectedConversationID else { return }
        await resyncTranscript(selectedConversationID, backend: backend)
    }

    private static func captureTranscriptLoad(
        _ read: () async throws -> ConversationListTranscriptResult
    ) async -> Result<ConversationListTranscriptResult, Error> {
        do { return .success(try await read()) }
        catch { return .failure(error) }
    }

    /// A transcript failure must not discard independently available inputs
    /// or lifecycle state. Never reconstruct assistant history from raw events.
    /// A nil transcript keeps the store as it is.
    func applyLoadedDetail(_ detail: ConversationDetail,
                           transcript: Result<ConversationListTranscriptResult, Error>?) {
        guard selectedConversationID == detail.conversation.conversationID else { return }
        apply(detail: detail)
        isLoadingDetail = false
        switch transcript {
        case .success(let page)?:
            transcriptStore.merge(page, conversationID: detail.conversation.conversationID)
            transcriptLoadError = nil
        case .failure(let error)?:
            transcriptLoadError = error.localizedDescription
        case nil:
            break
        }
    }

    /// `transcript` false leaves the store as it is: a cached return, whose
    /// store kept merging deltas while out of view.
    private func loadDetail(_ conversationID: ConversationID, backend: DiffReviewModel, transcript: Bool = true) async {
        guard !Task.isCancelled else { return }
        guard let workspaceID else {
            isLoadingDetail = false
            return
        }
        errorText = nil
        do {
            async let detailResult = backend.getConversation(
                workspaceID: workspaceID,
                conversationID: conversationID
            )
            showTranscript(of: conversationID)
            let requestedInstance = transcriptStore.coreInstanceID
            async let initialTranscriptResult = transcript ? Self.captureTranscriptLoad {
                try await backend.listConversationTranscript(
                    workspaceID: workspaceID, conversationID: conversationID
                )
            } : nil
            var detail = try await detailResult.conversation
            do {
                while let cursor = detail.latestRunInputPage.nextCursor {
                    let page = try await backend.listConversationRunInputs(
                        workspaceID: workspaceID,
                        conversationID: conversationID,
                        cursor: cursor
                    ).page
                    detail.latestRunInputPage.runInputs.append(contentsOf: page.runInputs)
                    detail.latestRunInputPage.nextCursor = page.nextCursor
                }
                while let cursor = detail.latestRunPage.nextCursor {
                    let page = try await backend.listConversationRuns(
                        workspaceID: workspaceID,
                        conversationID: conversationID,
                        cursor: cursor
                    ).page
                    detail.latestRunPage.runs.append(contentsOf: page.runs)
                    detail.latestRunPage.nextCursor = page.nextCursor
                }
            } catch {
                // Keep the bounded collections already returned by getConversation.
                guard !Task.isCancelled, selectedConversationID == conversationID,
                      self.workspaceID == workspaceID else { return }
                errorText = error.localizedDescription
            }
            guard !Task.isCancelled else { return }
            let transcript = await initialTranscriptResult
            guard !Task.isCancelled else { return }
            if case .success(let page)? = transcript {
                guard transcriptStore.coreInstanceID == requestedInstance || transcriptStore.coreInstanceID == page.coreInstanceID else { return }
            }
            // A slow response must not clobber a newer selection.
            guard selectedConversationID == conversationID,
                  self.workspaceID == workspaceID
            else { return }
            applyLoadedDetail(detail, transcript: transcript)
            transcriptStore.loadBodies(workspaceID: workspaceID, backend: backend)
        } catch {
            guard !Task.isCancelled else { return }
            guard selectedConversationID == conversationID,
                  self.workspaceID == workspaceID
            else { return }
            isLoadingDetail = false
            errorText = error.localizedDescription
            // A cached return keeps its transcript on screen, but its old
            // detail must not stand in again: the next return loads fresh.
            transcriptCache.session(for: conversationID)?.detail = nil
            Self.logger.error("failed to load Chat detail: \(error.localizedDescription, privacy: .private)")
        }
    }

    private func loadDetailIfSelected(_ conversationID: ConversationID, backend: DiffReviewModel) async {
        guard selectedConversationID == conversationID else { return }
        await loadDetail(conversationID, backend: backend)
    }

    func apply(conversations: [ConversationSummary]) {
        self.conversations = conversations
        // Keep an existing selection if it's still present; otherwise fall back to the
        // composer-first landing (no auto-selected conversation) rather than opening the
        // most recent one. Chat is a home you return to, not a resumed thread.
        if let selectedConversationID,
           !conversations.contains(where: { $0.conversationID == selectedConversationID }) {
            self.selectedConversationID = nil
        }
    }

    /// Applies lifecycle and input delivery detail independently of transcript pages.
    private func apply(detail: ConversationDetail) {
        selectedDetail = detail
        showsCachedDetail = false
        // A run rebuilt from notifications alone (after a workspace switch or
        // core replacement) never saw a start response; the detail knows when
        // it started. Only fills existing live runs — a stale `running` row
        // in the detail must not conjure one.
        for run in detail.runs {
            guard let live = liveRuns[run.runID], live.startedAt == nil else { continue }
            withLiveRun(run.runID, conversationID: live.conversationID, status: live.status) {
                $0.startedAt = run.startedAt
            }
        }
        selectedConversationID = detail.summary.conversationID
        if let latestEngineID = detail.runs.first?.engineID {
            selectedEngineID = latestEngineID
        }
        let activeSession = Self.latestRunSession(
            runs: detail.runs, sessions: detail.latestAgentSessionPage.sessions
        )
        noteActiveAgentSession(activeSession?.agentSessionID, conversationID: detail.summary.conversationID)
        if let modeID = activeSession?.currentModeID {
            confirmMode(modeID, conversationID: detail.summary.conversationID)
        }
        if let activeSession, let commands = activeSession.commands {
            recordCommands(commands, engineID: activeSession.engineID, conversationID: detail.summary.conversationID)
        }
        // The active session's value, or none: a new session (another engine,
        // a failed restore) must not show the previous one's context.
        recordContextUsage(activeSession?.contextUsage, conversationID: detail.summary.conversationID)
    }

    /// Records usage that arrived after its run's summary loaded. A run not on
    /// the loaded page picks it up with the next load.
    static func applyRunUsage(
        _ usage: ConversationRunUsage, runID: ConversationRunID, to runs: inout [ConversationRunSummary]
    ) {
        guard let index = runs.firstIndex(where: { $0.runID == runID }) else { return }
        runs[index].usage = usage
        runs[index].usageUnavailable = false
    }

    /// The active session of the latest run's engine, newest first. The newest
    /// active session overall is wrong after an engine switch back (A → B → A),
    /// where B's session is newer but A's is in use. A run's `agentSessionID`
    /// is the adapter's external id, so it cannot be matched here; Core makes
    /// the exact match when it resolves `conversation.setMode`.
    static func latestRunSession(
        runs: [ConversationRunSummary], sessions: [AgentSessionSummary]
    ) -> AgentSessionSummary? {
        guard let run = runs.first else { return nil }
        return sessions.first { $0.state == .active && $0.engineID == run.engineID }
    }

    private func markUnavailable() {
        cancelDetailLoad()
        didCompleteInitialLoad = false
        engines = []
        conversations = []
        selectedDetail = nil
        selectedConversationID = nil
        noticeText = nil
        errorText = "Open a project before using Chat."
    }

    /// Clears the flag as well as the task. Every early return in `loadDetail`
    /// is a `Task.isCancelled` guard, so a cancelled load never reaches the
    /// assignment that would lower it — and now that the thread and the composer
    /// are both gated on `isLoadingDetail`, a leaked one hides them until the
    /// user reselects. Callers that go on to start a load raise it after this.
    private func cancelDetailLoad() {
        detailLoadTask?.cancel()
        detailLoadTask = nil
        isLoadingDetail = false
    }
}

private extension JSONValue {
    func objectString(_ key: String) -> String? {
        guard case .object(let values) = self,
              case .string(let value)? = values[key]
        else { return nil }
        return value
    }

    func objectValue(_ key: String) -> JSONValue? {
        guard case .object(let values) = self else { return nil }
        return values[key]
    }

    var arrayFirst: JSONValue? {
        guard case .array(let values) = self else { return nil }
        return values.first
    }
}
