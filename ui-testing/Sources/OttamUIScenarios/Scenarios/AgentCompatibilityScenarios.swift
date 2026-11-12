import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    /// Settings → Agents → Claude Code → Compatibility → Details…: the ACP
    /// compatibility sheet. The adapter is never started in a UI run, so the
    /// Agent column is empty and every verdict reads Unknown; the capture is
    /// evidence for the table, its sections, and the Ottam column.
    static let settingsAgentCompatibility = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.agent-compatibility",
            description: "Open the Claude Code compatibility sheet from the Adapter section "
                + "and capture the feature table.",
            tags: ["settings", "agents", "acp", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            capturesSettings: true,
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["Settings panes and agent detail"],
            fixtureRequirements: [.deterministicProject]
        )
    ) { context in
        // The app opens Settings only after the route handshake completes, so
        // the chat route is captured first even though it is not the subject.
        try await context.capture(
            routeCapture(id: "route-chat", required: [AccessibilityID.chatComposer])
        )
        try await context.waitFor(
            .exists(context.element(id: AccessibilityID.settingsWindow)),
            timeout: 15
        )
        try await context.selectListRow(named: "Agents", waitForText: "Check Again")
        try await context.pressControl(named: "Claude Code", waitForText: "Instances")
        try await context.pressControl(named: "Ottam profile", waitForText: "Compatibility")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-agent-compatibility-row",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.settingsWindow],
                    requiredTexts: ["Adapter", "Compatibility", "Details…"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.pressControl(named: "Details…", waitForText: "ACP SDK")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-agent-compatibility-sheet",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .sheet,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.settingsWindow],
                    requiredTexts: ["Claude Code Compatibility", "loadSession", "Done"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
    }
}
