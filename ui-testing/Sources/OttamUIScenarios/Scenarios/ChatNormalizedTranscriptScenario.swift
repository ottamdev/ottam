import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatSessionSignals = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.session-signals",
            description: "Inspect typed retry, rate-limit and failure signals, then reload saved observations.",
            tags: ["chat", "transcript", "metadata"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat", fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let title = "Check session signals"
        let stages: [(ACPPromptBehavior, String)] = [
            (.sessionRetry, "retry"), (.sessionLimit, "limit"), (.sessionFailure, "failure")
        ]
        for (index, stage) in stages.enumerated() {
            try context.armACP(stage.0, reply: "Captured answer for \(stage.1).")
            try await context.setValue(index == 0 ? title : "Continue with \(stage.1)", on: composer)
            try await context.press(submit)
            try await context.waitForACPPromptCount(index + 1, timeout: 20)
            if index == 0 {
                try await context.waitFor(.labelContains(context.element(id: AccessibilityID.chatRunStatus), "Agent is retrying"), timeout: 20)
            } else {
                try await context.waitFor(.exists(context.element(id: AccessibilityID.chatSessionNotice)), timeout: 20)
            }
            if index == 2 {
                try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
            }
            try await context.capture(routeCapture(id: "session-\(stage.1)", required: index == 0 ? [AccessibilityID.chatRunStatus, AccessibilityID.chatStop] : [AccessibilityID.chatSessionNotice], auditExceptions: [
                AXAuditException(rule: .duplicateIdentifier, identifier: AccessibilityID.chatPartialOutput, reason: "Each stopped or failed Run has its own outcome trailer.")
            ]))
            if index != 2 { try await context.press(context.element(id: AccessibilityID.chatStop)) }
            try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        }
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
        try await context.selectListRow(named: title, waitForText: title)
        try await waitForChat(context)
        try await context.waitFor(.exists(context.element(id: AccessibilityID.chatSessionNotice)), timeout: 20)
        try await context.capture(routeCapture(id: "session-reloaded", required: [AccessibilityID.chatComposer, AccessibilityID.chatSessionNotice], auditExceptions: [
            AXAuditException(rule: .duplicateIdentifier, identifier: AccessibilityID.chatPartialOutput, reason: "Each stopped or failed Run has its own outcome trailer.")
        ]))
    }

    static let chatTranscriptCaptureRecovery = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-capture-recovery",
            description: "Recover after abrupt Core exit and render positioned capture loss after reload.",
            tags: ["chat", "transcript", "recovery"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            fixtureRequirements: [.testACP, .coreLifecycle]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        try context.armACP(.holdUntilCancel, reply: "This checkpoint survives Core replacement.")
        try await context.setValue("Recover captured output", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        // Allow the periodic checkpoint while the fixture keeps execution open.
        try await Task.sleep(for: .seconds(2))
        try await context.terminateOwnedCore(expectReplacement: true)
        try await waitForChat(context)
        let warning = context.element(id: AccessibilityID.chatActivityGroup)
        try await context.waitFor(.labelContains(warning, "Capture may be incomplete"), timeout: 20)
        try await context.capture(routeCapture(id: "capture-recovered", required: [AccessibilityID.chatComposer, AccessibilityID.chatActivityGroup]))
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
        try await context.selectListRow(named: "Recover captured output", waitForText: "Recover captured output")
        try await context.waitFor(.labelContains(warning, "Capture may be incomplete"), timeout: 20)
        try await context.capture(routeCapture(id: "capture-reloaded", required: [AccessibilityID.chatComposer, AccessibilityID.chatActivityGroup]))
        // The deliberately killed fixture accepted the first prompt. Resolve
        // that admission uncertainty before sending the second test turn.
        try await context.pressControl(named: "Mark Delivered", waitForText: "This checkpoint survives Core replacement.")
        try context.armACP(.captureLoss, reply: "This output precedes the capture gap.")
        try await context.setValue("Check positioned capture loss", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(2, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.capture(routeCapture(id: "capture-gap", required: [AccessibilityID.chatComposer, AccessibilityID.chatActivityGroup]))
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
        try await context.selectListRow(named: "Recover captured output", waitForText: "Recover captured output")
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.capture(routeCapture(id: "capture-gap-reloaded", required: [AccessibilityID.chatComposer, AccessibilityID.chatActivityGroup]))
    }

    static let chatTranscriptNormalized = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-normalized",
            description: "Stream normalized text, expand compact reasoning and tool rows, steer, and reload.",
            tags: ["chat", "transcript", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light", "OTTAM_TEST_ACP_STEERING": "1"],
            fixtures: ["deterministic Rust ACP adapter"], fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let title = "Check normalized transcript"
        try context.armACP(.normalizedTranscript, reply: "Assistant before steering.")
        try await context.setValue(title, on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        let activity = context.element(id: AccessibilityID.chatActivityGroup)
        try await context.waitFor(.exists(activity), timeout: 20)
        try await context.waitFor(.valueEquals(activity, "active"), timeout: 10)
        try await context.capture(routeCapture(id: "normalized-activity-active", required: [AccessibilityID.chatActivityGroup]))
        try await context.press(activity)
        try await context.capture(routeCapture(id: "normalized-streaming", required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatStop]))
        try await context.setValue("Continue after this steering message.", on: composer)
        try await context.press(submit)
        try await context.press(context.element(id: AccessibilityID.chatQueuedMessages))
        let sendNow = context.element(id: AccessibilityID.chatQueuedMessageSendNow)
        try await context.waitFor(.exists(sendNow), timeout: 10)
        try await context.press(sendNow)
        try await Task.sleep(for: .seconds(2))
        try await context.waitFor(.valueEquals(activity, "finished"), timeout: 10)
        try await context.capture(routeCapture(id: "normalized-steered", required: [AccessibilityID.chatComposer]))
        try await context.press(context.element(id: AccessibilityID.chatStop))
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
        try await context.selectListRow(named: title, waitForText: title)
        try await waitForChat(context)
        try await context.waitFor(.exists(activity), timeout: 20)
        try await context.press(activity)
        try await context.capture(routeCapture(id: "normalized-reloaded", required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatComposer]))
    }
}
