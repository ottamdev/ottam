import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    /// M9's walkthrough against the scripted agent: the composer's context
    /// gauge, a turn's tokens on its trailer, none for a draft, and a stopped
    /// turn's tokens arriving after Stop. Switching back to a chat and a
    /// relaunch are checked in the real window: the harness can't select a
    /// sidebar row.
    static let chatUsage = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.usage",
            description: "Show the context gauge and a turn's tokens, none for a draft, and a stopped turn's late usage.",
            tags: ["chat", "transcript", "usage"],
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
        let stop = context.element(id: AccessibilityID.chatStop)
        let gauge = context.element(id: AccessibilityID.chatContextUsage)
        let tokens = context.element(id: AccessibilityID.chatTurnUsage)

        // A finished turn: the gauge and the turn's tokens.
        try context.armUsageACP(.usage, contextUsed: 24_100)
        try await context.setValue("Count my tokens", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.waitFor(.valueEquals(gauge, "24.1K of 200K tokens (12%)"), timeout: 20)
        try await context.waitFor(.valueEquals(tokens, "12.3K tokens"), timeout: 20)
        try await context.capture(routeCapture(
            id: "usage-turn",
            required: [AccessibilityID.chatContextUsage, AccessibilityID.chatTurnUsage]
        ))

        // A draft has no gauge.
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await context.waitFor(.notExists(gauge), timeout: 10)

        // Stop finalizes at once; the agent's cancelled answer brings the
        // turn's tokens afterwards.
        try context.armUsageACP(.usageCancelled, contextUsed: 152_000)
        try await context.setValue("Stop after counting", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(2, timeout: 20)
        try await context.waitFor(.valueEquals(gauge, "152K of 200K tokens (76%)"), timeout: 20)
        try await context.press(stop)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.waitFor(.valueEquals(tokens, "12.3K tokens"), timeout: 20)
        try await context.capture(routeCapture(
            id: "usage-stopped",
            required: [AccessibilityID.chatContextUsage, AccessibilityID.chatTurnUsage]
        ))
    }
}
