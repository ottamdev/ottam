import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let smoke = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "smoke",
            description: "Capture every available top-level product page in one app lifetime.",
            tags: ["smoke", "quiet"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "smoke",
            executionMode: .backgroundIsolated,
            routes: "chat",
            capturesChooser: true,
            capturesSettings: true,
            environment: [
                "OTTAM_UI_TEST_APPEARANCE": "light",
            ],
            fixtures: [
                "deterministic chooser, Git fixture, Settings panes, and agent detail",
            ],
            fixtureRequirements: [.deterministicProject]
        )
    ) { context in
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "workspace-chooser",
                captureID: "workspace-chooser",
                scopeIdentifier: nil,
                snapshotPolicy: .wholeApplicationDiagnostic,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.workspaceChooser],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "route-chat",
                captureID: "route-chat",
                scopeIdentifier: AccessibilityID.workspaceDetail,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.workspaceDetail,
                        AccessibilityID.chatComposer,
                        AccessibilityID.chatSubmit,
                    ],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-app",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.settingsWindow,
                    ],
                    requiredTexts: ["Theme"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.selectListRow(named: "Agents", waitForText: "Check Again")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-agents",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.settingsWindow,
                    ],
                    requiredTexts: ["Check Again", "Claude Code"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.pressControl(named: "Claude Code", waitForText: "Instances")
        try await context.pressControl(named: "Ottam profile", waitForText: "Adapter")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-agent-claude",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.settingsWindow,
                    ],
                    requiredTexts: ["Claude Code", "Adapter", "Compatibility", "Runtime", "Account"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.selectListRow(named: "MCP Servers", waitForText: "Add Server…")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-mcp-servers",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.settingsWindow],
                    requiredTexts: ["Servers", "Add Server…", "No MCP servers yet."],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.selectListRow(named: "Terminal", waitForText: "Ghostty Overlay")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-terminal",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.settingsWindow,
                    ],
                    requiredTexts: ["Ghostty Overlay", "Reload"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.selectListRow(named: "Storage", waitForText: "Tool Result Cache")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "settings-window",
                captureID: "settings-storage",
                scopeIdentifier: AccessibilityID.settingsWindow,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.settingsWindow],
                    requiredTexts: ["Tool Result Cache", "Cache limit", "Clear Cache…"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
    }
}
