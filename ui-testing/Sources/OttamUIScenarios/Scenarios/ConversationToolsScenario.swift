import OttamAccessibility
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatToolsPane = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.tools-pane",
            description: "Show a conversation's tools pane beside a tall transcript, pick Review, "
                + "close its tab, open two terminals, switch conversations, then hide it.",
            tags: ["chat", "layout", "focused"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            fixtures: ["deterministic Rust ACP adapter", "workspace Git working tree"],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let pane = context.element(id: AccessibilityID.conversationTools)
        let review = context.element(id: AccessibilityID.conversationToolsReview)
        let tab = context.element(id: AccessibilityID.conversationToolsTab)
        let close = context.element(id: AccessibilityID.conversationToolsTabClose)
        let terminal = context.element(id: AccessibilityID.terminalSurface)
        let tabs = context.element(id: AccessibilityID.conversationToolsTabs)
        let pickReview = context.element(id: AccessibilityID.conversationToolsPickReview)
        let pickTerminal = context.element(id: AccessibilityID.conversationToolsPickTerminal)

        // Tall enough that the pinned-to-bottom transcript runs under the
        // toolbar, so captures show the band split at the pane.
        try context.armACP(.complete, reply: tallReply(sections: 12))
        try await context.setValue("Tools A", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try await context.assert(.notExists(pane))

        // Toolbar items carry no AX identifier; the harness finds them by label.
        // A conversation without tabs shows the tool picker, and no tabs.
        try await context.focus(composer)
        try await context.waitFor(.focused(composer, true), timeout: 5)
        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.exists(pickReview), timeout: 10)
        try await context.assert(.focused(composer, true))
        try await context.assert(.notExists(tabs))
        try await context.capture(routeCapture(
            id: "chat-tools-picker",
            required: [
                AccessibilityID.conversationTools,
                AccessibilityID.conversationToolsPickReview,
                AccessibilityID.conversationToolsPickTerminal,
            ]
        ))

        // Picking Review opens it as the first tab.
        try await context.press(pickReview)
        try await context.waitFor(.exists(review), timeout: 10)
        try await context.assert(.labelContains(tab, "Review"))
        // The tabs sit in the toolbar, outside the route capture's scope.
        try await context.waitFor(.exists(tabs), timeout: 5)
        // Review diffs the fixture's working tree, a local checkout's own
        // changes, so its rows can run past any node budget.
        try await context.capture(routeCapture(
            id: "chat-tools-review",
            scope: AccessibilityID.conversationTools,
            required: [
                AccessibilityID.conversationTools,
                AccessibilityID.conversationToolsReview,
            ],
            snapshotPolicy: reviewSnapshot
        ))

        // Shown again, the pane slides open over a loader, and Review joins
        // it after the slide.
        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.notExists(pane), timeout: 10)
        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.exists(review), timeout: 10)
        try await context.waitFor(.exists(tabs), timeout: 5)

        // Closing the last tab keeps the pane, back at its picker, and the
        // composer keeps focus.
        try await context.press(close)
        try await context.waitFor(.exists(pickReview), timeout: 10)
        try await context.waitFor(.notExists(tabs), timeout: 5)
        try await context.assert(.focused(composer, true))

        // Picking Terminal opens a selected terminal tab that takes keyboard
        // focus; New Terminal (the menu command) adds another, and numbering
        // starts at the second.
        try await context.press(pickTerminal)
        try await context.waitFor(.exists(terminal), timeout: 10)
        try await context.waitFor(.focused(composer, false), timeout: 5)
        try await context.pressControl(named: "New Terminal", waitForText: "Terminal 2")
        try await context.waitFor(.identifierCountEquals(tab, 2), timeout: 10)
        try await context.waitFor(.exists(terminal), timeout: 10)
        try await context.capture(routeCapture(
            id: "chat-tools-terminals",
            required: [
                AccessibilityID.conversationTools,
                AccessibilityID.terminalSurface,
            ]
        ))

        // Closing the selected terminal, which holds keyboard focus, ends it
        // and mounts its neighbor.
        try await context.press(close)
        try await context.waitFor(.identifierCountEquals(tab, 1), timeout: 10)
        try await context.waitFor(.exists(terminal), timeout: 10)

        // A new chat has no tools; the first conversation's pane comes back with it.
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await waitForChat(context)
        try await context.waitFor(.notExists(pane), timeout: 10)
        try await context.selectListRow(named: "Tools A", waitForText: "Tools A")
        try await waitForChat(context)
        try await context.waitFor(.exists(terminal), timeout: 10)
        try await context.assert(.identifierCountEquals(tab, 1))

        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.notExists(pane), timeout: 10)
        try await context.waitFor(.notExists(tabs), timeout: 5)
        try await context.capture(routeCapture(
            id: "chat-tools-hidden",
            required: [AccessibilityID.chatComposer]
        ))
    }
}

/// A route capture of the tools pane that keeps its first rows when the diff
/// runs long.
private let reviewSnapshot = SemanticSnapshotPolicy(
    name: "review",
    maxDepth: SemanticSnapshotPolicy.route.maxDepth,
    maxNodes: SemanticSnapshotPolicy.route.maxNodes,
    maxValueLength: SemanticSnapshotPolicy.route.maxValueLength,
    timeoutSeconds: SemanticSnapshotPolicy.route.timeoutSeconds,
    maxDurationMilliseconds: SemanticSnapshotPolicy.route.maxDurationMilliseconds,
    allowsTruncation: true
)
