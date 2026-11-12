import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    /// M2's walkthrough against Claude 0.85.1's recorded subagent turn: an
    /// asynchronous subagent runs, with a nested one inside it, beside a
    /// launch that fails; the turn's end completes it.
    static let chatSubagentThreads = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.subagent-threads",
            description: "Show subagent threads grouping their calls: running, opened, failed, nested, and completed.",
            tags: ["chat", "transcript", "subagents"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            fixtures: ["deterministic Rust ACP adapter"], fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        // The recording launches the long-running subagent first.
        let thread = context.element(id: AccessibilityID.chatSubagentThread)

        // The turn holds with the subagent working. The trailer's count sits
        // under the live trailer's `chat.run-status` placeholder, so the
        // capture shows it rather than a query.
        try context.armACP(.subagentThreads)
        try await context.setValue("Use a subagent to find where transcript_metadata is defined", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(thread, "running"), timeout: 20)
        try await context.capture(routeCapture(
            id: "subagent-running",
            required: [AccessibilityID.chatSubagentThread]
        ))

        // Clicked, it opens the Subagent tab on the subagent's calls and its
        // nested subagent.
        let tab = context.element(id: AccessibilityID.chatSubagentTab)
        try await context.press(thread)
        try await context.waitFor(.valueEquals(tab, "subagent:toolu_01B7bGspZr9HB4ctXRrxemcW"), timeout: 20)
        try await context.capture(routeCapture(
            id: "subagent-tab",
            required: [AccessibilityID.chatSubagentThread, AccessibilityID.chatSubagentTab]
        ))

        // The turn's end completes the thread it held open.
        try context.publishACPMarker("subagents-complete")
        try await context.waitFor(.valueEquals(thread, "completed"), timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try await context.capture(routeCapture(
            id: "subagent-completed",
            required: [AccessibilityID.chatSubagentThread]
        ))
    }

    /// M3's walkthrough against Claude 0.85.1's recorded native subagent
    /// turn: two subagents, one launching another, and a subagent's
    /// permission request, each subagent's text and calls in its own thread.
    static let chatNativeSubagentThreads = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.native-subagent-threads",
            description: "Show native subagent sessions: a subagent's permission prompt names it, its card opens its conversation in the Subagent tab, and a subagent the parent resumes keeps its thread.",
            tags: ["chat", "transcript", "subagents"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            fixtures: ["deterministic Rust ACP adapter"], fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let thread = context.element(id: AccessibilityID.chatSubagentThread)
        let asker = context.element(id: AccessibilityID.chatPermissionSubagent)
        let tab = context.element(id: AccessibilityID.chatSubagentTab)

        // A subagent asks to write a file: the card says which one.
        try context.armACP(.nativeSubagents)
        try await context.setValue("Use two subagents in parallel, one of which launches another", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.exists(asker), timeout: 20)
        try await context.capture(routeCapture(
            id: "native-permission",
            required: [AccessibilityID.chatPermissionCard, AccessibilityID.chatPermissionSubagent]
        ))
        try await context.pressControl(named: "Yes", waitForText: "Stop")

        // The subagents hold the turn, each a thread card.
        try await context.waitFor(.valueEquals(thread, "running"), timeout: 20)
        try await context.capture(routeCapture(
            id: "native-running",
            required: [AccessibilityID.chatSubagentThread]
        ))

        // A card opens its subagent's conversation in the Subagent tab.
        try await context.press(thread)
        try await context.waitFor(.exists(tab), timeout: 20)
        try await context.capture(routeCapture(
            id: "native-subagent-tab",
            required: [AccessibilityID.chatSubagentTab, AccessibilityID.chatSubagentTranscript]
        ))

        // The subagents end; the turn ends after them.
        try context.publishACPMarker("subagents-complete")
        try await context.waitFor(.valueEquals(thread, "completed"), timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try await context.capture(routeCapture(
            id: "native-completed",
            required: [AccessibilityID.chatSubagentThread, AccessibilityID.chatSubagentTab]
        ))

        // The parent messages a subagent that finished: it resumes in the
        // same thread, whose tab shows the message.
        try context.armACP(.nativeSubagentFirstRun)
        try await context.setValue("Use a subagent to read the README's first line", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(2, timeout: 20)
        try await context.waitFor(.labelContains(asker, "Read README first line"), timeout: 20)
        // The recording goes on either way; "Yes" by now also names earlier
        // transcript rows, which the driver would find first.
        try await context.pressControl(named: "No", waitForText: "Stop")
        try await context.waitFor(.notExists(asker), timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try context.armACP(.nativeSubagentResumed)
        try await context.setValue("Ask the same subagent to count the README's words", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(3, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try await context.pressControl(named: "Read README first line", waitForText: "Read README first line")
        try await context.waitFor(.valueEquals(tab, "subagent:a3700dd2c6acc37d1"), timeout: 20)
        try await context.capture(routeCapture(
            id: "native-follow-up",
            required: [AccessibilityID.chatSubagentTab, AccessibilityID.chatSubagentTranscript]
        ))
    }
}
