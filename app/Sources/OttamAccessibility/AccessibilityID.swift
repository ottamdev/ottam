/// Stable semantic identifiers shared by Ottam and its macOS UI validation
/// system. Add identifiers only for product surfaces that automation or
/// assistive technologies must locate; visible labels remain user-facing.
public enum AccessibilityID {
    public static let workspaceRoot = "workspace.root"
    public static let workspaceSidebar = "workspace.sidebar"
    public static let workspaceDetail = "workspace.detail"
    public static let workspaceChooser = "workspace.chooser"
    public static let chooserCoreFailure = "workspace.chooser.core-failure"
    public static let chooserCoreRetry = "workspace.chooser.core-retry"
    public static let coreDiagnosticsToggle = "core.diagnostics-toggle"
    public static let chooserNewWorkspace = "workspace.chooser.new-workspace"

    public static let routeChat = "route.chat"

    public static let chatSurface = "chat.surface"
    /// The transcript's scroll view itself, as distinct from `chatSurface` —
    /// which names a SwiftUI container macOS coalesces away, so it never
    /// reaches the accessibility tree. A gesture needs a real scrollable
    /// element to land on, and this is it. The SwiftUI renderer must carry the
    /// same identifier, or a scroll-driven measurement cannot address both
    /// sides of the A/B.
    public static let chatTranscript = "chat.transcript"
    public static let chatQuoteReplyButton = "chat.quote-reply.button"
    public static let chatQuoteReplyCopy = "chat.quote-reply.copy"
    public static let chatQuoteReplyReveal = "chat.quote-reply.reveal"
    public static let chatQuoteReplyField = "chat.quote-reply.field"
    public static let chatQuoteReplyAdd = "chat.quote-reply.add"
    /// Repeated once per staged reply in the composer, so a scenario asserts
    /// on the count rather than on distinct identifiers.
    public static let chatQuoteReplyRemove = "chat.quote-reply.remove"
    /// A measurement-only control that scrolls the transcript back one
    /// viewport per press. Present only while a UI-test preparation request is
    /// active, and only in DEBUG builds.
    public static let chatScrollBackProbe = "chat.scroll-back-probe"
    public static let chatScrollBottomProbe = "chat.scroll-bottom-probe"
    public static let chatScrollStateProbe = "chat.scroll-state-probe"
    public static let chatSelectionCopyProbe = "chat.selection-copy-probe"
    public static let chatResizeProbe = "chat.resize-probe"
    public static let chatComposer = "chat.composer"
    public static let chatSubmit = "chat.submit"
    public static let chatRunStatus = "chat.run-status"
    public static let chatSessionNotice = "chat.session-notice"
    public static let chatStop = "chat.stop"
    public static let chatPartialOutput = "chat.partial-output"
    public static let chatRunFailure = "chat.run-failure"
    public static let chatRunFailureDetails = "chat.run-failure.details"
    public static let chatRunFailureRetry = "chat.run-failure.retry"
    /// The composer's context gauge; its value is "used of size tokens (n%)".
    public static let chatContextUsage = "chat.context-usage"
    /// The composer's subscription usage meter; its value lists each window,
    /// such as "5h 12% used, wk 15% used".
    public static let chatPlanUsage = "chat.plan-usage"
    /// The plan usage popover's refresh button.
    public static let chatPlanUsageRefresh = "chat.plan-usage.refresh"
    /// The run trailer's token count, or "unavailable".
    public static let chatTurnUsage = "chat.turn-usage"
    /// The run trailer's count of background tasks still running.
    public static let chatBackgroundTaskCount = "chat.background-task-count"
    /// A backgrounded tool's task state ("Running in background", "Stopped").
    public static let chatBackgroundTaskState = "chat.background-task-state"
    public static let chatBackgroundTaskStop = "chat.background-task-stop"
    /// The divider above a run the agent started on its own.
    public static let chatContinuedAutomatically = "chat.continued-automatically"
    /// The divider above a turn a different agent answered, or whose new
    /// session was sent the earlier turns; its value is the label.
    public static let chatEngineSwitched = "chat.engine-switched"
    /// A "Received message" row in a parent; its value is the child's
    /// state and, for a stopped child, why.
    public static let chatDelegationReceived = "chat.delegation-received"
    /// The row of reports and messages no turn has carried; its value is
    /// their count.
    public static let chatDelegationWaiting = "chat.delegation-waiting"
    public static let chatDelegationContinue = "chat.delegation-continue"
    /// Send to Parent on a delegated child's latest turn, while its report
    /// waits in the parent without waking it.
    public static let chatDelegationSendToParent = "chat.delegation-send-to-parent"
    /// The child card for a `delegate` call; its value is the agent, the
    /// child's state and its work so far.
    public static let chatDelegated = "chat.delegated"
    /// The top of a delegated child: who started it, and its Task card.
    public static let chatDelegationTask = "chat.delegation-task"
    public static let chatDelegationPrompt = "chat.delegation-prompt"
    public static let chatDelegationContext = "chat.delegation-context"
    /// The parent's title in a child's Task header, which opens the parent.
    public static let chatDelegationOpen = "chat.delegation-open"
    /// A subagent's thread card headline; its value is the thread's state.
    public static let chatSubagentThread = "chat.subagent-thread"
    /// The turn trailer's count of running subagents.
    public static let chatSubagentCount = "chat.subagent-count"
    /// The Subagent tab's title, the subagent it shows; its value is that
    /// thread's id.
    public static let chatSubagentTab = "chat.subagent-tab"
    /// The Subagent tab's path from the top-level subagent to this one; its
    /// value is that path's thread ids, outermost first.
    public static let chatSubagentBreadcrumb = "chat.subagent-breadcrumb"
    /// The Subagent tab's transcript, drawn by the main transcript's renderer.
    public static let chatSubagentTranscript = "chat.subagent-transcript"
    /// The pending permission card's label naming the subagent that asks.
    public static let chatPermissionSubagent = "chat.permission-subagent"
    /// The raw-code Copy button in a fenced code block's header.
    public static let chatCodeBlockCopy = "chat.code-block.copy"
    /// The "Show all N lines" / "Show less" control under a long code block.
    public static let chatCodeBlockExpand = "chat.code-block.expand"
    public static let chatDeliveryRecovery = "chat.delivery-recovery"
    public static let chatDeliveryRetry = "chat.delivery-retry"
    public static let chatDeliveryMarkDelivered = "chat.delivery-mark-delivered"
    public static let chatPendingRetryStart = "chat.pending-retry-start"
    public static let chatMarkedDelivered = "chat.marked-delivered"
    /// The approval card, rendered inline in the thread at the tool call that
    /// raised it. Also the anchor for asserting the composer stays reachable
    /// while an approval is outstanding.
    public static let chatPermissionCard = "chat.permission-card"
    /// The command card above the composer while the draft is a `/` query;
    /// its value is the highlighted row.
    public static let chatCommandPanel = "chat.command-panel"
    /// One mode or command row in the command card.
    public static let chatCommandRow = "chat.command-row"
    /// The question panel above the composer while an agent waits on answers;
    /// its value is the page, such as "1 of 3".
    public static let chatQuestionPanel = "chat.question-panel"
    /// The plan panel above the composer while an agent waits on a plan's
    /// approval; its value is the plan's first line.
    public static let chatPlanPanel = "chat.plan-panel"
    /// The plan panel's Revise button: keep planning, then type feedback.
    public static let chatPlanRevise = "chat.plan-revise"
    /// The plan panel's control that selects the Plan tab.
    public static let chatPlanOpen = "chat.plan-open"
    /// The plan panel's control that sends the selected option.
    public static let chatPlanSubmit = "chat.plan-submit"
    /// The plan panel's collapse and expand control.
    public static let chatPlanCollapse = "chat.plan-collapse"
    /// The Plan tab in the tools pane; its value is the plan's markdown.
    public static let chatPlanTab = "chat.plan-tab"
    public static let chatFileTab = "chat.file-tab"
    public static let chatFileTabUnavailable = "chat.file-tab-unavailable"
    public static let chatFileFrontmatter = "chat.file-frontmatter"
    /// The question panel's label naming the subagent that asks.
    public static let chatQuestionSubagent = "chat.question-subagent"
    /// The question panel's collapse and expand control.
    public static let chatQuestionCollapse = "chat.question-collapse"
    /// The free-text answer of a question that allows one.
    public static let chatQuestionOther = "chat.question-other"
    public static let chatQuestionBack = "chat.question-back"
    public static let chatQuestionNext = "chat.question-next"
    public static let chatQuestionSkip = "chat.question-skip"
    public static let chatQuestionSubmit = "chat.question-submit"
    /// What became of a tool call's question, on its transcript row; its
    /// value is the record's state.
    public static let chatQuestionRecord = "chat.question-record"
    /// The pill above the composer that appears when an approval is pending and
    /// the reader has scrolled away from it. Navigates to the card; never decides.
    public static let chatPermissionJump = "chat.permission-jump"
    /// The permanent transcript row for a resolved approval.
    public static let chatPermissionRecord = "chat.permission-record"
    /// A burst of tool calls in an assistant turn; value "active" while the
    /// burst is still in flight, "finished" once it has settled.
    public static let chatActivityGroup = "chat.activity-group"
    public static let chatAuthRecovery = "chat.auth-recovery"
    public static let chatAuthSignIn = "chat.auth-sign-in"
    public static let chatAuthSendAgain = "chat.auth-send-again"
    /// A server the agent called needs a sign-in; its value is the phase.
    public static let chatMcpSignIn = "chat.mcp-sign-in"
    public static let chatMcpSignInButton = "chat.mcp-sign-in-button"
    /// The engine chooser in the composer. Its rows carry the runtime state, so
    /// this is the anchor for asserting a Node-less machine's picker.
    public static let chatEngineMenu = "chat.engine-menu"
    /// The chooser popover's own root, which a capture scopes to.
    public static let chatEngineMenuPopover = "chat.engine-menu.popover"
    /// An engine row whose runtime probe is still running. Only those rows
    /// carry it, so its absence means every probe has answered.
    public static let chatEngineChecking = "chat.engine-checking"
    /// The composer pill showing how many messages wait for the working turn.
    public static let chatQueuedMessages = "chat.queued-messages"
    /// The popover's own root. A popover is its own window, so a capture
    /// scoped to the Workspace cannot see it; this is what it scopes to
    /// instead of walking the whole application tree.
    public static let chatQueuedMessagesPopover = "chat.queued-messages.popover"
    /// Repeated once per row inside the queue popover, so a scenario asserts on
    /// the count rather than on distinct identifiers.
    public static let chatQueuedMessageSendNow = "chat.queued-message.send-now"
    public static let chatQueuedMessageRemove = "chat.queued-message.remove"
    public static let chatRuntimeRecovery = "chat.runtime-recovery"
    public static let chatRuntimeCheckAgain = "chat.runtime-check-again"
    public static let chatSelectionRecovery = "chat.selection-recovery"
    public static let chatSelectionSendAgain = "chat.selection-send-again"
    /// The "Set Up Agents" capsule that replaces the send button when no
    /// engine is usable. Navigation, not submission — never reuse chatSubmit.
    public static let chatSetUpAgents = "chat.set-up-agents"

    /// The changed-files list beside a diff.
    public static let gitFileList = "working-tree.file-list"

    public static let terminalSurface = "terminal.surface"

    /// The tools pane beside a conversation, mounted only while it shows.
    public static let conversationTools = "conversation-tools"
    /// The Review tool's diff surface, in each of its states.
    public static let conversationToolsReview = "conversation-tools.review"
    /// The tab strip above the tools pane.
    public static let conversationToolsTabs = "conversation-tools.tabs"
    /// Each tab in the strip; its label is the tool's title.
    public static let conversationToolsTab = "conversation-tools.tab"
    /// A tab's close button, exposed while it shows.
    public static let conversationToolsTabClose = "conversation-tools.tab.close"
    /// The tool picker's rows, shown while a conversation has no tabs.
    public static let conversationToolsPickReview = "conversation-tools.pick.review"
    public static let conversationToolsPickTerminal = "conversation-tools.pick.terminal"
    public static let conversationToolsPickPlan = "conversation-tools.pick.plan"

    /// The Workspace-naming popup button at the foot of the sidebar. Singular
    /// per window because each native window presents exactly one Workspace.
    public static let sidebarIdentityBlock = "sidebar.identity-block"
    /// The monogram-tint swatch row inside the identity block's menu. Mounted
    /// only once the Workspace summary supplies an ID to key the choice by.
    public static let sidebarIdentityColorPicker = "sidebar.identity-color-picker"
    /// The trailing "Chats" section header, present only in the 2+-project
    /// shape. A stable marker because per-project section titles are not.
    public static let sidebarChatsSection = "sidebar.chats-section"
    /// The "History" section header over the flat list, present only in the
    /// collapsed (0- or 1-project) shape. Its presence and the Chats header's
    /// are mutually exclusive, so either one identifies the shape.
    public static let sidebarHistorySection = "sidebar.history-section"
    /// The composer's working-root chooser, mounted only in the 2+-project
    /// shape — its absence in the collapsed shape is itself the assertion.
    public static let chatWorkingRootMenu = "chat.working-root-menu"

    /// Matches the native SwiftUI scene identifier exposed on the AXWindow.
    public static let settingsWindow = "settings"
    /// DEBUG-only markdown engine lab window (parity gallery + the cards spike).
    public static let markdownEngineLab = "markdown-engine-lab"
    /// Lab Cards tab: the label that reports the last hosted-card decision.
    public static let markdownEngineLabCardResolved = "markdown-engine-lab.card-resolved"
    public static let coreRecoverySheet = "core.recovery-sheet"
    public static let coreRecoveryRetry = "core.recovery-retry"

    /// Identifiers that are unique on screen — the UI-test audit's uniqueness
    /// list. Per-row identifiers (`chatPermissionCard`, `chatPermissionRecord`,
    /// `chatActivityGroup`, the background-task state, Stop and count,
    /// `chatContinuedAutomatically`, `chatEngineSwitched`, and the subagent
    /// thread and count) repeat
    /// once per transcript entry, and per-tab ones
    /// (`conversationToolsTab`, `conversationToolsTabClose`) once per tool, so
    /// they are deliberately not here.
    public static let all: [String] = [
        workspaceRoot,
        workspaceSidebar,
        workspaceDetail,
        workspaceChooser,
        chooserCoreFailure,
        chooserCoreRetry,
        coreDiagnosticsToggle,
        chooserNewWorkspace,
        routeChat,
        chatSurface,
        chatTranscript,
        chatQuoteReplyButton,
        chatQuoteReplyCopy,
        chatQuoteReplyReveal,
        chatQuoteReplyField,
        chatQuoteReplyAdd,
        chatScrollBackProbe,
        chatScrollBottomProbe,
        chatScrollStateProbe,
        chatSelectionCopyProbe,
        chatResizeProbe,
        chatComposer,
        chatSubmit,
        chatRunStatus,
        chatSessionNotice,
        chatStop,
        chatContextUsage,
        chatPlanUsage,
        chatPlanUsageRefresh,
        chatPartialOutput,
        chatDeliveryRecovery,
        chatDeliveryRetry,
        chatDeliveryMarkDelivered,
        chatPendingRetryStart,
        chatMarkedDelivered,
        chatPermissionJump,
        chatAuthRecovery,
        chatAuthSignIn,
        chatAuthSendAgain,
        chatMcpSignIn,
        chatMcpSignInButton,
        chatEngineMenu,
        chatEngineMenuPopover,
        chatEngineChecking,
        chatQueuedMessages,
        chatQueuedMessagesPopover,
        chatQueuedMessageSendNow,
        chatQueuedMessageRemove,
        chatRuntimeRecovery,
        chatRuntimeCheckAgain,
        chatSelectionRecovery,
        chatSelectionSendAgain,
        chatSetUpAgents,
        gitFileList,
        terminalSurface,
        conversationTools,
        conversationToolsReview,
        conversationToolsTabs,
        conversationToolsPickReview,
        conversationToolsPickTerminal,
        conversationToolsPickPlan,
        sidebarIdentityBlock,
        sidebarIdentityColorPicker,
        sidebarChatsSection,
        sidebarHistorySection,
        chatWorkingRootMenu,
        settingsWindow,
        markdownEngineLab,
        markdownEngineLabCardResolved,
        coreRecoverySheet,
        coreRecoveryRetry,
    ]
}
