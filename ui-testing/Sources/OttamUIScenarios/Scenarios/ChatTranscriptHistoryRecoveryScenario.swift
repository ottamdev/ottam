import OttamAccessibility
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatTranscriptHistoryRecovery = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-history-recovery",
            description: "Return beside a tools pane, load four collapsed history pages, resize, and reopen after Core replacement.",
            tags: ["chat", "transcript", "recovery", "layout"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat", fixtureRequirements: [.testACP, .coreLifecycle]
        ), preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let pane = context.element(id: AccessibilityID.conversationTools)
        let oldest = context.element(id: "chat.plan.activity")
        let title = "Restore transcript history"
        try context.armACP(.transcriptHistory, reply: "The final retained response also remains readable.")
        try await context.setValue(title, on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.exists(pane), timeout: 10)
        try await Task.sleep(for: .seconds(1))
        try await context.selectListRow(named: "New Chat", waitForText: "New Chat")
        try await context.selectListRow(named: title, waitForText: title)
        try await waitForChat(context)
        // The plan precedes 1,200 collapsed calls: it is absent from the first
        // 1,000-record page. No down-scroll between pages is allowed here.
        try await context.press(context.element(id: AccessibilityID.chatScrollBackProbe))
        try await context.waitFor(.exists(oldest), timeout: 20)
        try await context.waitFor(.exists(pane), timeout: 10)
        try await Task.sleep(for: .seconds(8))
        try await context.capture(routeCapture(id: "history-returned-with-pane",
            required: [AccessibilityID.chatTranscript, AccessibilityID.conversationTools, "chat.plan.activity"]))
        try await context.pressControl(named: "Tools", waitForText: "Tools")
        try await context.waitFor(.notExists(pane), timeout: 10)
        // AX removes the pane before the captured backing surfaces settle.
        // A later capture must include all retained rows without resizing.
        try await Task.sleep(for: .seconds(8))
        try await context.capture(routeCapture(id: "history-full-width", required: [AccessibilityID.chatTranscript, "chat.plan.activity"]))
        try await context.press(context.element(id: AccessibilityID.chatResizeProbe))
        try await context.capture(routeCapture(id: "history-resized", required: [AccessibilityID.chatTranscript, "chat.plan.activity"]))
        try await context.terminateOwnedCore(expectReplacement: true)
        try await waitForChat(context)
        try await context.selectListRow(named: "New Chat", waitForText: "New Chat")
        try await context.selectListRow(named: title, waitForText: title)
        try await waitForChat(context)
        try await context.press(context.element(id: AccessibilityID.chatScrollBackProbe))
        try await context.waitFor(.exists(oldest), timeout: 20)
        try await Task.sleep(for: .seconds(1))
        try await context.capture(routeCapture(id: "history-core-replaced", required: [AccessibilityID.chatTranscript, "chat.plan.activity"]))
    }
}
