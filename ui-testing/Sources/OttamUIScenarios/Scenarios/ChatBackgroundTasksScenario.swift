import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    /// M1's walkthrough against the scripted agent: a backgrounded command
    /// outlives its turn, its completion opens a follow-up marked as
    /// continued automatically, and Stop ends one that is still running.
    static let chatBackgroundTasks = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.background-tasks",
            description: "Show a background task running past its turn, the automatic follow-up when it ends, and Stop.",
            tags: ["chat", "transcript", "background"],
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
        let state = context.element(id: AccessibilityID.chatBackgroundTaskState)
        let stop = context.element(id: AccessibilityID.chatBackgroundTaskStop)
        let count = context.element(id: AccessibilityID.chatBackgroundTaskCount)
        let continued = context.element(id: AccessibilityID.chatContinuedAutomatically)

        // The turn ends with its command still running.
        try context.armACP(.asyncTasksFollowUp, reply: "The command finished and printed done.")
        try await context.setValue("Run sleep 30 in the background", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.waitFor(.valueEquals(state, "running"), timeout: 20)
        try await context.waitFor(.exists(count), timeout: 20)
        try await context.capture(routeCapture(
            id: "background-running",
            required: [AccessibilityID.chatBackgroundTaskState, AccessibilityID.chatBackgroundTaskStop,
                       AccessibilityID.chatBackgroundTaskCount]
        ))

        // Its end wakes the agent, whose answer is a run of its own.
        try context.publishACPMarker("follow-up-complete")
        try await context.waitFor(.exists(continued), timeout: 20)
        try await context.waitFor(.valueEquals(state, "completed"), timeout: 20)
        try await context.waitFor(.notExists(count), timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try await context.capture(routeCapture(
            id: "background-follow-up",
            required: [AccessibilityID.chatContinuedAutomatically, AccessibilityID.chatBackgroundTaskState]
        ))

        // Stop ends a task that is still running.
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await context.waitFor(.notExists(state), timeout: 10)
        try context.armACP(.asyncTasksFollowUp, reply: "Never sent: the task is stopped first.")
        try await context.setValue("Run sleep 300 in the background", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(2, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.waitFor(.valueEquals(state, "running"), timeout: 20)
        try await context.press(stop)
        try await context.waitFor(.valueEquals(state, "stopped"), timeout: 20)
        try await context.waitFor(.notExists(stop), timeout: 10)
        try await context.waitFor(.notExists(count), timeout: 10)
        try await context.capture(routeCapture(
            id: "background-stopped",
            required: [AccessibilityID.chatBackgroundTaskState]
        ))
    }
}
