import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let settingsExistingSetups = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.existing-setups",
            description: "Offer Add My CLI Setup on the Codex and Claude provider pages in Settings.",
            tags: ["settings", "agents", "onboarding", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat", capturesSettings: true,
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["Settings panes and agent setup"], fixtureRequirements: [.deterministicProject]
        )
    ) { context in
        try await context.capture(routeCapture(id: "route-chat", required: [AccessibilityID.chatComposer]))
        try await context.waitFor(.exists(context.element(id: AccessibilityID.settingsWindow)), timeout: 15)
        try await context.selectListRow(named: "Agents", waitForText: "Check Again")
        try await context.capture(ScenarioCaptureRequest(
            controlName: "settings-window", captureID: "agent-providers",
            scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
            auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                requiredTexts: ["Codex", "Claude Code"], uniqueIdentifiers: AccessibilityID.all)
        ))
        // Adding a CLI setup would attach the machine's real vendor home, so
        // the scenario stops at the offer; the instance page owns the rest.
        for (provider, captureID) in [("Codex", "codex-instances"), ("Claude Code", "claude-instances")] {
            try await context.pressControl(named: provider, waitForText: "Instances")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: captureID,
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                    requiredTexts: ["Add My CLI Setup…", "Create another profile…"],
                                    uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Back", waitForText: "Check Again")
        }
    }
}
