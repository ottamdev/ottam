import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import OttamWorkspaceLogic
import SwiftUI

/// A child this conversation delegated to, as the conversation list has it.
struct ChatDelegatedChild: Equatable {
    var conversationID: ConversationID
    var agentName: String
    /// The task's title, as core keeps it apart from the child's title.
    var title: String
    var activity: DelegationActivity?
    var isRunning = false
    var needsApproval = false
    /// The child's tile.
    var avatar: AgentAvatar?
    /// The parent's turn that delegated it, and when it started, for a
    /// card in that turn when no call there is recognizably `delegate`.
    var fromRunID: ConversationRunID?
    var createdAt: String?

    init(
        conversationID: ConversationID, agentName: String, title: String,
        activity: DelegationActivity? = nil, isRunning: Bool = false, needsApproval: Bool = false,
        avatar: AgentAvatar? = nil, fromRunID: ConversationRunID? = nil, createdAt: String? = nil
    ) {
        self.conversationID = conversationID
        self.agentName = agentName
        self.title = title
        self.activity = activity
        self.isRunning = isRunning
        self.needsApproval = needsApproval
        self.avatar = avatar
        self.fromRunID = fromRunID
        self.createdAt = createdAt
    }

    init(_ summary: ConversationSummary, isRunning: Bool = false, needsApproval: Bool = false) {
        self.init(
            conversationID: summary.conversationID, agentName: summary.delegatedAgentName ?? "Agent",
            title: summary.taskTitle,
            activity: summary.delegationActivity, isRunning: isRunning, needsApproval: needsApproval,
            avatar: summary.delegatedAvatar, fromRunID: summary.delegatedFromRunID,
            createdAt: summary.createdAt
        )
    }
}

extension DelegationStopCause {
    /// Why a child stopped, after "stopped" or "Stopped". `agent` names
    /// the parent's agent, when it stopped the child.
    func phrase(agent: String? = nil) -> String {
        switch self {
        case .user: "by you"
        case .parentAgent: "by \(agent ?? "the agent")"
        case .parentStopped: "with this turn"
        case .quit: "when Ottam quit"
        }
    }
}

extension DelegationActivity {
    /// "10 commands, 4 reads, 2 subagents"; nil before the first tool call.
    var summary: String? {
        let counts: [(Int, String, String)] = [
            (commands, "command", "commands"), (reads, "read", "reads"), (edits, "edit", "edits"),
            (searches, "search", "searches"), (fetches, "fetch", "fetches"), (other, "other tool", "other tools"),
            (subagents, "subagent", "subagents"),
        ]
        let parts = counts.filter { $0.0 > 0 }.map { "\($0.0) \($0.0 == 1 ? $0.1 : $0.2)" }
        return parts.isEmpty ? nil : parts.joined(separator: ", ")
    }
}

/// The top of a delegated child: who started it, from where, and the task.
struct ChatDelegationTaskPresentation: Equatable {
    var task: DelegationTask
    /// The parent's agent, by its display name, named when the parent is
    /// gone.
    var startedBy: String?
    var parentConversationID: ConversationID?
    var parentTitle: String?
    /// This child's tile.
    var avatar: AgentAvatar?

    /// The parent's title as "Started by" shows it, on one line.
    var parentTitleLabel: String? {
        guard let title = parentTitle?.cappedTitle(), !title.isEmpty else { return nil }
        return title
    }
}

/// Delegation in a transcript: a `delegate` call's child card and the
/// reports waiting in the parent, and the task atop the child.
struct ChatDelegationPresentation: Equatable {
    enum Kind: Equatable {
        /// A `delegate` call; `failed` when the tool refused it. The child
        /// is there once the conversation list has it.
        case delegated(agent: String, failed: Bool, child: ChatDelegatedChild?, stopCause: DelegationStopCause?)
        /// Reports and messages no turn has carried while the conversation
        /// is idle: Continue delivers them now. `messages` when none is a
        /// child's final report.
        case waiting(count: Int, messages: Bool)
        case task(ChatDelegationTaskPresentation)
    }

    var kind: Kind
    var isMutating = false
    /// This conversation's agent, for "Stopped by Claude Code".
    var agentName: String?

    /// The tile the card shows: a child's, once it exists.
    var avatar: AgentAvatar? {
        switch kind {
        case .delegated(_, _, let child, _): child?.avatar
        case .waiting: nil
        case .task(let task): task.avatar
        }
    }

    /// The conversation the card opens.
    var openConversationID: ConversationID? {
        switch kind {
        case .delegated(_, _, let child, _): child?.conversationID
        case .waiting: nil
        case .task(let task): task.parentConversationID
        }
    }

    var title: String {
        switch kind {
        case .delegated(let agent, let failed, let child, _):
            failed ? "Could not delegate to \(agent)" : child?.title ?? "Delegated to \(agent)"
        case .waiting(let count, let messages):
            "\(count) \(messages ? "message" : "report")\(count == 1 ? "" : "s") waiting"
        case .task(let task): task.task.title
        }
    }

    /// The child's state: Working, Approval required, Finished, Failed or
    /// Stopped and why.
    var state: String? {
        guard case .delegated(_, let failed, let child, let stopCause) = kind, !failed else { return nil }
        guard let child else { return "Starting" }
        if child.needsApproval { return "Approval required" }
        if child.isRunning { return "Working" }
        switch child.activity?.latestRunStatus {
        case nil, .queued, .running: return "Working"
        case .completed: return "Finished"
        case .failed: return "Failed"
        // An interrupted run is over, as core's note says.
        case .canceled, .interrupted: return stopCause.map { "Stopped \($0.phrase(agent: agentName))" } ?? "Stopped"
        }
    }

    /// "review-bot · Working · 10 commands, 4 reads"
    var subtitle: String? {
        guard case .delegated(let agent, let failed, let child, _) = kind, !failed else { return nil }
        return ([child?.agentName ?? agent, state] + [child?.activity?.summary]).compactMap { $0 }.joined(separator: " · ")
    }

    var isWorking: Bool {
        ["Starting", "Working", "Approval required"].contains(state)
    }

    var accessibilityValue: String {
        switch kind {
        case .delegated(let agent, let failed, _, _): failed ? "failed: \(agent)" : subtitle ?? agent
        case .waiting(let count, _): "\(count)"
        case .task(let task): task.task.title
        }
    }

    var accessibilityIdentifier: String {
        switch kind {
        case .delegated: AccessibilityID.chatDelegated
        case .waiting: AccessibilityID.chatDelegationWaiting
        case .task: AccessibilityID.chatDelegationTask
        }
    }
}

struct DelegationCardPayload: HostedCardPayload {
    struct Actions {
        var onOpen: () -> Void
        var onContinue: () -> Void
        var onToggleFullPrompt: () -> Void = {}
        var onToggleContext: () -> Void = {}
    }

    var presentation: ChatDelegationPresentation
    /// The Task card's prompt, past its first lines.
    var showsFullPrompt = false
    /// The Task card's instructions.
    var showsContext = false

    var signature: AnyHashable {
        "\(presentation.title)|\(presentation.subtitle ?? "")|\(presentation.state ?? "")|\(presentation.openConversationID?.rawValue ?? "")|\(presentation.isMutating)|\(showsFullPrompt)|\(showsContext)|\(presentation.avatar?.seed.description ?? "")"
    }

    var heightSignature: AnyHashable {
        if case .task(let task) = presentation.kind {
            return AnyHashable([task.task.prompt, task.task.instructions, task.parentTitle ?? "", "\(showsFullPrompt)", "\(showsContext)"])
        }
        return signature
    }

    func makeBody(actions: Actions) -> some View {
        ChatDelegationCard(
            presentation: presentation, showsFullPrompt: showsFullPrompt, showsContext: showsContext,
            open: actions.onOpen, continueAction: actions.onContinue,
            toggleFullPrompt: actions.onToggleFullPrompt, toggleContext: actions.onToggleContext
        )
        .frame(maxWidth: 560, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    mutating func adopt(_ fresh: Self) {
        let (showsFullPrompt, showsContext) = (self.showsFullPrompt, self.showsContext)
        self = fresh
        self.showsFullPrompt = showsFullPrompt
        self.showsContext = showsContext
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: presentation.accessibilityIdentifier,
            value: presentation.accessibilityValue
        )
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        actions.onToggleFullPrompt = { [weak card] in card?.updatePayload { $0.showsFullPrompt.toggle() } }
        actions.onToggleContext = { [weak card] in card?.updatePayload { $0.showsContext.toggle() } }
    }
}

typealias HostedDelegationCard = HostedCard<DelegationCardPayload>

struct ChatDelegationCard: View {
    let presentation: ChatDelegationPresentation
    var showsFullPrompt = false
    var showsContext = false
    let open: () -> Void
    let continueAction: () -> Void
    var toggleFullPrompt: () -> Void = {}
    var toggleContext: () -> Void = {}

    /// The prompt's lines before Show more.
    private static let promptLines = 6

    var body: some View {
        switch presentation.kind {
        case .delegated(_, true, _, _):
            Label(presentation.title, systemImage: "exclamationmark.triangle")
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .padding(.vertical, 2)
                .accessibilityIdentifier(AccessibilityID.chatDelegated)
                .accessibilityValue(presentation.accessibilityValue)
        case .delegated:
            childCard
        case .waiting:
            HStack(spacing: 8) {
                Label(presentation.title, systemImage: "tray")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Button("Continue", action: continueAction)
                    .controlSize(.small)
                    .disabled(presentation.isMutating)
                    .help("Hand what is waiting to the agent now.")
                    .accessibilityIdentifier(AccessibilityID.chatDelegationContinue)
            }
            .padding(.vertical, 2)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(AccessibilityID.chatDelegationWaiting)
            .accessibilityValue(presentation.accessibilityValue)
        case .task(let task):
            taskCard(task)
        }
    }

    /// The child a `delegate` call started: its task, agent, state and
    /// work so far. Clicking it opens the child.
    private var childCard: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                // The child's tile leads once it exists, and its state
                // moves to the trailing edge.
                if let avatar = presentation.avatar {
                    // The size the child's own Task card uses.
                    AgentAvatarView(avatar: avatar, size: 22)
                } else {
                    stateMark
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(presentation.title)
                        .appFont(.body, .bold)
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                    if let subtitle = presentation.subtitle {
                        Text(subtitle)
                            .appFont(.callout)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                if presentation.avatar != nil {
                    stateMark
                }
                if presentation.openConversationID != nil {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
            .contentShape(RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        .disabled(presentation.openConversationID == nil)
        .help(presentation.openConversationID == nil ? "" : "Open this agent's conversation")
        .accessibilityElement(children: .ignore)
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel(presentation.title)
        .accessibilityIdentifier(AccessibilityID.chatDelegated)
        .accessibilityValue(presentation.accessibilityValue)
    }

    @ViewBuilder
    private var stateMark: some View {
        switch presentation.state {
        case "Approval required":
            Circle().fill(.orange).frame(width: 8, height: 8).frame(width: 16)
        case "Starting", "Working":
            Circle().fill(Color.accentColor).frame(width: 8, height: 8).frame(width: 16)
        case "Finished":
            Image(systemName: "checkmark.circle").foregroundStyle(.green).frame(width: 16)
        case "Failed":
            Image(systemName: "exclamationmark.triangle").foregroundStyle(.orange).frame(width: 16)
        default:
            Image(systemName: "stop.circle").foregroundStyle(.secondary).frame(width: 16)
        }
    }

    private func taskCard(_ task: ChatDelegationTaskPresentation) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            startedLine(task)
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    if let avatar = task.avatar {
                        AgentAvatarView(avatar: avatar, size: 22)
                    }
                    Text(task.task.title)
                        .appFont(.body, .bold)
                        .lineLimit(2)
                }
                Text(task.task.prompt)
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(showsFullPrompt ? nil : Self.promptLines)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(AccessibilityID.chatDelegationPrompt)
                if task.task.prompt.split(whereSeparator: \.isNewline).count > Self.promptLines || task.task.prompt.count > 480 {
                    Button(showsFullPrompt ? "Show less" : "Show more", action: toggleFullPrompt)
                        .buttonStyle(.link)
                        .controlSize(.small)
                }
                if !task.task.instructions.isEmpty {
                    Button(action: toggleContext) {
                        HStack(spacing: 5) {
                            Text("Context")
                            Image(systemName: "chevron.right")
                                .font(.system(size: 10, weight: .semibold))
                                .rotationEffect(.degrees(showsContext ? 90 : 0))
                        }
                        .appFont(.callout)
                        .foregroundStyle(.secondary)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("The named agent's instructions, sent ahead of the task")
                    .accessibilityIdentifier(AccessibilityID.chatDelegationContext)
                    .accessibilityValue(showsContext ? "expanded" : "collapsed")
                    if showsContext {
                        Text(task.task.instructions)
                            .appFont(.callout)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatDelegationTask)
        .accessibilityValue(task.task.title)
    }

    /// "Started by Claude Code from <parent> · 4 min. ago"
    private func startedLine(_ task: ChatDelegationTaskPresentation) -> some View {
        // The time is on the row under the card, with the message's other
        // actions.
        HStack(spacing: 4) {
            Text("Started by")
            if let parent = task.parentTitleLabel, task.parentConversationID != nil {
                // Dotted, like the parent's "Received message from <child>"
                // link the other way. Cut to fit one line, whole in the
                // tooltip.
                Button(parent, action: open)
                    .buttonStyle(.plain)
                    .underline(pattern: .dot)
                    .pointerStyle(.link)
                    .fixedSize()
                    .help("Open “\(task.parentTitle ?? parent)”")
                    .accessibilityIdentifier(AccessibilityID.chatDelegationOpen)
            } else {
                Text(task.startedBy ?? "another conversation")
            }
        }
        .appFont(.callout)
        .foregroundStyle(.secondary)
        .lineLimit(1)
    }
}
