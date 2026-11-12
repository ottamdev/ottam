import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let settingsExistingSetups = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.existing-setups",
            description: "Open the native existing-setup choices for Codex and Claude from Settings.",
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
        for (provider, captureID) in [("Codex", "codex-setup-choice"), ("Claude Code", "claude-setup-choice")] {
            try await context.pressControl(named: provider, waitForText: "Instances")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(captureID)-instances",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                    requiredTexts: ["Ottam profile", "Set Up \(provider)…"], uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Set Up \(provider)…", waitForText: "Choose a profile")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: captureID,
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .sheet,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                    requiredTexts: ["Set Up \(provider)", "Use my CLI setup", "Use existing Ottam profile, Recommended", "Continue"],
                                    uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Use my CLI setup", waitForText: "Your CLI setup")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(captureID)-existing",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .sheet,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                    requiredTexts: ["Your CLI setup", "Continue"], uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Profile, Use existing Ottam profile, Recommended", waitForText: "Choose a profile")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(captureID)-separate",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .sheet,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                    requiredTexts: ["Use existing Ottam profile, Recommended", "Continue"],
                                    uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Continue", waitForText: "Your Ottam profile is selected.")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(captureID)-reused",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .sheet,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                    requiredTexts: ["Your Ottam profile is selected.", "Cancel"],
                                    uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Cancel", waitForText: "Instances")
            try await context.pressControl(named: "Back", waitForText: "Check Again")
        }
    }
}
