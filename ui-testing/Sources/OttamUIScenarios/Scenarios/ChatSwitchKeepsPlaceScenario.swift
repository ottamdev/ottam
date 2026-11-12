import OttamAccessibility
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatSwitchKeepsPlace = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.switch-keeps-place",
            description: "Open a tool group, switch away and back from the cache, after eviction, and after Core replacement.",
            tags: ["chat", "transcript", "cache"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            // Two cached conversations: a third one evicts the least recent.
            environment: ["OTTAM_DEBUG_CONVERSATION_CACHE": "2"],
            fixtureRequirements: [.testACP, .coreLifecycle]
        ), preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let group = context.element(id: AccessibilityID.chatActivityGroup)
        // Shown only while the group is open.
        let plan = context.element(id: "chat.plan.activity")
        let kept = "Keep my place"
        func start(_ title: String, _ behavior: ACPPromptBehavior, reply: String, prompts: Int) async throws {
            try await context.pressControl(named: "New Chat", waitForText: "New Chat")
            try context.armACP(behavior, reply: reply)
            try await context.setValue(title, on: composer)
            try await context.press(submit)
            try await context.waitForACPPromptCount(prompts, timeout: 20)
            try await context.waitFor(.valueEquals(submit, "ready"), timeout: 60)
        }
        func open(_ title: String) async throws {
            try await context.selectListRow(named: title, waitForText: title)
            try await waitForChat(context)
        }

        try context.armACP(.transcriptPayloads, reply: "Captured details.")
        try await context.setValue(kept, on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.exists(group), timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 60)
        try await context.press(group)
        try await context.waitFor(.exists(plan), timeout: 5)
        try await context.capture(routeCapture(id: "switch-group-open", required: ["chat.plan.activity"]))

        // Cached: the conversation returns as it was left, without a reload.
        try await start("Second conversation", .complete, reply: "A second reply.", prompts: 2)
        try await context.waitFor(.notExists(plan), timeout: 5)
        try await open(kept)
        try await context.waitFor(.exists(plan), timeout: 2)
        try await context.capture(routeCapture(id: "switch-returned-cached", required: ["chat.plan.activity"]))

        // Evicted: a third conversation pushes it out; it reloads from Core
        // and reopens the group the reader left open.
        try await open("Second conversation")
        try await start("Third conversation", .complete, reply: "A third reply.", prompts: 3)
        try await open(kept)
        try await context.waitFor(.exists(plan), timeout: 20)
        try await context.capture(routeCapture(id: "switch-returned-evicted", required: ["chat.plan.activity"]))

        // Core replacement drops every cached store but not what was open.
        try await context.terminateOwnedCore(expectReplacement: true)
        try await waitForChat(context)
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await open(kept)
        try await context.waitFor(.exists(plan), timeout: 20)
        try await context.capture(routeCapture(id: "switch-core-replaced", required: ["chat.plan.activity"]))
    }
}
