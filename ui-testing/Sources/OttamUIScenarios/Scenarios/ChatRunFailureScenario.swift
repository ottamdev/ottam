import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatRunFailure = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.run-failure",
            description: "Read a saved quota error, reopen it, manually continue once, and cancel with partial output preserved.",
            tags: ["chat", "transcript", "recovery"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat", fixtureRequirements: [.testACP, .coreLifecycle]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let failure = context.element(id: AccessibilityID.chatRunFailure)
        let retry = context.element(id: AccessibilityID.chatRunFailureRetry)
        let title = "Recover an interrupted review"
        try context.armACP(.usageLimitFailure, reply: "Partial review saved before the provider limit.")
        try await context.setValue(title, on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.waitFor(.valueContains(failure, "OpenCode Go usage limit exceeded"), timeout: 20)
        try context.assertACPPromptCount(1)
        try await context.capture(routeCapture(
            id: "run-failed", required: [AccessibilityID.chatRunFailure, AccessibilityID.chatRunFailureRetry],
            requiredActions: [.init(identifier: AccessibilityID.chatRunFailureRetry, actions: ["AXPress"])]
        ))
        try await context.press(context.element(id: AccessibilityID.chatRunFailureDetails))
        try await context.capture(routeCapture(id: "failure-details", required: [AccessibilityID.chatRunFailureDetails]))
        try await context.terminateOwnedCore(expectReplacement: true)
        try await waitForChat(context)
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await context.selectListRow(named: title, waitForText: title)
        try await waitForChat(context)
        try await context.waitFor(.exists(retry), timeout: 20)
        try context.assertACPPromptCount(1)
        try await context.capture(routeCapture(id: "failure-reopened", required: [AccessibilityID.chatRunFailure, AccessibilityID.chatRunFailureRetry]))

        try context.armACP(.holdUntilCancel, reply: "Continuing the saved review.")
        try await context.press(retry)
        try await context.waitForACPPromptCount(2, timeout: 20)
        try await context.waitFor(.exists(context.element(id: AccessibilityID.chatStop)), timeout: 20)
        try await context.waitFor(.notExists(retry), timeout: 20)
        try context.assertACPPromptCount(2)
        try await context.capture(routeCapture(id: "failure-retry-running", required: [AccessibilityID.chatRunFailure, AccessibilityID.chatStop]))
        try await context.press(context.element(id: AccessibilityID.chatStop))
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try context.assertACPPromptCount(2)
        try await context.capture(routeCapture(
            id: "failure-retry-stopped", required: [AccessibilityID.chatRunFailure, AccessibilityID.chatComposer],
            auditExceptions: [.init(rule: .duplicateIdentifier, identifier: AccessibilityID.chatPartialOutput,
                                    reason: "The failed attempt and canceled continuation each retain their outcome.")]
        ))
    }
}
