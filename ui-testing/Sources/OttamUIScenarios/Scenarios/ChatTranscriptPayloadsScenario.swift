import AppKit
import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatTranscriptPayloads = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-payloads",
            description: "Expand captured output, requested edits, reasoning and images, then reload offline.",
            tags: ["chat", "transcript", "payloads"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            environment: ["OTTAM_TEST_PAYLOAD_READ_DELAY_MS": "1500"],
            fixtureRequirements: [.testACP, .coreLifecycle]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let title = "Check captured tool details"
        let command = "printf 'first line'\nprintf 'second line'"
        try context.armACP(.transcriptPayloads, reply: "Captured details.")
        try await context.setValue(title, on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        let group = context.element(id: AccessibilityID.chatActivityGroup)
        try await context.waitFor(.exists(group), timeout: 20)
        try await context.waitFor(.labelContains(group, "Run payload fixture"), timeout: 30)
        try await context.press(group)
        try await context.waitFor(.exists(context.element(id: "chat.plan.activity")), timeout: 5)
        try await context.assert(.notExists(context.element(id: "chat.payload.detail")))
        try await context.capture(routeCapture(id: "payload-plan", required: ["chat.plan.activity"]))
        try await context.pressControl(named: "Run payload fixture", waitForText: "Output")
        try await context.waitFor(.labelContains(context.element(id: "chat.payload.detail"), "loading"), timeout: 5)
        try await context.capture(routeCapture(id: "payload-loading", required: ["chat.payload.detail"]))
        try await context.waitFor(.labelContains(context.element(id: "chat.payload.detail"), "unsaved"), timeout: 10)
        try await context.waitFor(.valueEquals(context.element(id: "chat.payload.input.text"), command), timeout: 10)
        try await context.assert(.exists(context.element(id: "chat.payload.copy.input")))
        try await context.assert(.exists(context.element(id: "chat.payload.copy.output")))
        try await context.capture(routeCapture(id: "payload-unsaved", required: ["chat.payload.detail"]))
        try await context.pressControl(named: "Update greeting", waitForText: "Requested changes")
        try await context.waitFor(.labelContains(context.element(id: "chat.payload.detail"), "available"), timeout: 10)
        try await context.capture(routeCapture(id: "payload-requested-changes", required: ["chat.payload.detail"]))
        try await context.pressControl(named: "Check the saved details", waitForText: "Reasoning")
        try await context.capture(routeCapture(id: "payload-reasoning", required: ["chat.payload.detail"]))
        try await context.pressControl(named: "Inspect captured image", waitForText: "Output")
        try await context.waitFor(.exists(context.element(id: "chat.payload.image")), timeout: 10)
        try await context.capture(routeCapture(id: "payload-image", required: ["chat.payload.detail"]))
        try await context.pressControl(named: "Inspect image reference", waitForText: "The agent supplied a reference without image data.")
        try await context.capture(routeCapture(id: "payload-unavailable", required: ["chat.payload.detail"]))
        try await context.pressControl(named: "Inspect partial output", waitForText: "Output")
        try await context.waitFor(.labelContains(context.element(id: "chat.payload.detail"), "partial"), timeout: 10)
        try await context.capture(routeCapture(id: "payload-partial", required: ["chat.payload.detail"]))
        try await context.pressControl(named: "Inspect partial output", waitForText: "Run payload fixture")
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 60)
        // A finished file read is a direct file link, without either disclosure.
        try await context.capture(routeCapture(id: "payload-single-collapsed", required: [AccessibilityID.chatComposer]))
        try context.setACPExecutableAvailable(false)
        try await context.terminateOwnedCore(expectReplacement: true)
        try await waitForChat(context)
        // Terminal now lives in Tools. Leave the conversation to reset its
        // transcript before reloading the saved details without the adapter.
        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
        try await context.waitFor(.notExists(group), timeout: 10)
        try await context.selectListRow(named: title, waitForText: title)
        try await context.waitFor(.exists(group), timeout: 20)
        try await context.press(group)
        try await context.pressControl(named: "Run payload fixture", waitForText: "Output")
        try await context.waitFor(.valueEquals(context.element(id: "chat.payload.text"), "First line of locally captured output.\nFinal snapshot saved successfully."), timeout: 10)
        try await context.waitFor(.valueEquals(context.element(id: "chat.payload.input.text"), command), timeout: 10)
        let copyInput = context.element(id: "chat.payload.copy.input")
        let copyOutput = context.element(id: "chat.payload.copy.output")
        try await context.press(copyInput)
        try await context.waitFor(.valueEquals(copyInput, "copied"), timeout: 5)
        guard NSPasteboard.general.string(forType: .string) == command else {
            throw UIScenarioFailure.assertion("Copy command must contain only the command, without arguments JSON or output.")
        }
        try await context.press(copyOutput)
        try await context.waitFor(.valueEquals(copyOutput, "copied"), timeout: 5)
        guard NSPasteboard.general.string(forType: .string) == "First line of locally captured output.\nFinal snapshot saved successfully." else {
            throw UIScenarioFailure.assertion("Copy output must contain only the captured result, without the command.")
        }
        try await context.capture(routeCapture(id: "payload-offline-reloaded", required: ["chat.payload.detail", "chat.payload.copy.input", "chat.payload.copy.output", AccessibilityID.chatComposer]))
        try context.setACPExecutableAvailable(true)
    }
}
