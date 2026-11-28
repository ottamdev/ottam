import OttamAccessibility
import ScenarioKit

public extension UIScenarioDefinition {
    /// The branch name ends in random hex, and the driver waits only for exact
    /// text, so the subtitle (`<project> · <branch> · Worktree`) is evidence in
    /// the window capture rather than an assertion here.
    static let chatWorktreeConversation = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.worktree-conversation",
            description: "Tick Worktree under a new chat's composer and send: the conversation opens in its "
                + "own worktree, a terminal opens there, and the next new chat keeps the checkbox.",
            tags: ["chat", "worktree", "focused"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        // The run's own fixture repository, not this checkout: the worktree
        // and its branch must not land in the repository under test.
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            fixtures: ["deterministic Rust ACP adapter", "deterministic Git project"],
            fixtureRequirements: [.testACP, .deterministicProject]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let worktree = context.element(id: AccessibilityID.chatWorktreeToggle)
        let worktreeBaseBranch = context.element(id: AccessibilityID.chatWorktreeBaseBranchMenu)
        let pickTerminal = context.element(id: AccessibilityID.conversationToolsPickTerminal)
        let terminal = context.element(id: AccessibilityID.terminalSurface)

        // A project root offers the checkbox, unticked; the base-branch picker
        // stays hidden until Worktree is ticked.
        try await context.waitFor(.exists(worktree), timeout: 10)
        try await context.assert(.valueEquals(worktree, "0"))
        try await context.assert(.notExists(worktreeBaseBranch))
        try await context.press(worktree)
        try await context.waitFor(.valueEquals(worktree, "1"), timeout: 5)
        try await context.waitFor(.exists(worktreeBaseBranch), timeout: 5)
        try await context.capture(routeCapture(
            id: "chat-worktree-draft",
            required: [
                AccessibilityID.chatComposer,
                AccessibilityID.chatWorktreeToggle,
                AccessibilityID.chatWorktreeBaseBranchMenu,
            ]
        ))

        try context.armACP(.complete, reply: "Working in the worktree.")
        try await context.setValue("Worktree A", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 30)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        // The conversation is open: the landing composer and its checkbox are gone.
        try await context.assert(.notExists(worktree))
        try await context.capture(routeCapture(
            id: "chat-worktree-conversation",
            scope: nil,
            required: [AccessibilityID.chatComposer]
        ))

        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.exists(pickTerminal), timeout: 10)
        try await context.press(pickTerminal)
        try await context.waitFor(.exists(terminal), timeout: 10)
        try await context.capture(routeCapture(
            id: "chat-worktree-terminal",
            scope: nil,
            required: [AccessibilityID.conversationTools, AccessibilityID.terminalSurface]
        ))

        // The checkbox is remembered for the next draft.
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await waitForChat(context)
        try await context.waitFor(.valueEquals(worktree, "1"), timeout: 10)
    }
}
