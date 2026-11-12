import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let settingsProfileDeleteProbe = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.profile-delete-probe",
            description: "Cancel profile deletion while catalog initialization is pending.",
            tags: ["settings", "profiles", "cancellation"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat", capturesSettings: true,
            environment: ["OTTAM_MODEL_CATALOG_PROBE": "on"],
            fixtureRequirements: [.testACP, .deterministicProject]
        )
    ) { context in
        try await context.waitFor(.exists(context.element(id: AccessibilityID.settingsWindow)), timeout: 15)
        try await context.selectListRow(named: "Agents", waitForText: "Check Again")
        try await context.pressControl(named: "Codex", waitForText: "Instances")
        try context.holdACPInitialization()
        try await context.pressControl(named: "Create another profile…", waitForText: "Create Codex Profile")
        try await context.setValue("Pending probe", on: context.element(id: "agent.profile.label"))
        try await context.pressControl(named: "Create", waitForText: "Recheck Profile")
        try await context.pressControl(named: "Delete Profile…", waitForText: "Also remove this profile’s home")
        try await context.pressControl(named: "Delete Profile", waitForText: "Cancel Deletion")
        try await context.capture(ScenarioCaptureRequest(
            controlName: "settings-window", captureID: "delete-probe-wait",
            scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
            auditPolicy: .route(requiredIdentifiers: ["agent.profile.deleting", "agent.profile.cancelDeletion"],
                                requiredTexts: ["Deleting profile…", "Cancel Deletion"], uniqueIdentifiers: AccessibilityID.all)
        ))
        try await context.pressControl(named: "Cancel Deletion", waitForText: "Recheck Profile")
        try await context.waitFor(.notExists(context.element(id: "agent.profile.cancelDeletion")), timeout: 10)
        try context.releaseACPInitialization()
        try await context.capture(ScenarioCaptureRequest(
            controlName: "settings-window", captureID: "delete-probe-cancelled",
            scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
            auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow],
                                requiredTexts: ["Pending probe", "Delete Profile…"], uniqueIdentifiers: AccessibilityID.all)
        ))
    }

    static let settingsProfiles = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.profiles",
            description: "Create, rename and delete managed profiles for both providers in the native Settings window.",
            tags: ["settings", "agents", "profiles", "acceptance"],
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
        for (provider, prefix) in [("Codex", "codex"), ("Claude Code", "claude")] {
            try await context.pressControl(named: provider, waitForText: "Instances")
            try await context.pressControl(named: "Create another profile…", waitForText: "Create \(provider) Profile")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(prefix)-create",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .sheet,
                auditPolicy: .route(requiredIdentifiers: ["agent.profile.label"], requiredTexts: ["Cancel", "Create"], uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Cancel", waitForText: "Instances")
            try await context.pressControl(named: "Create another profile…", waitForText: "Create \(provider) Profile")
            try await context.setValue("Work", on: context.element(id: "agent.profile.label"))
            try await context.pressControl(named: "Create", waitForText: "Recheck Profile")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(prefix)-profile",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow], requiredTexts: ["Work", "Rename…", "Delete Profile…"], uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Rename…", waitForText: "Rename Profile")
            try await context.setValue("Office", on: context.element(id: "agent.profile.label"))
            try await context.pressControl(named: "Rename", waitForText: "Office")
            try await context.pressControl(named: "Delete Profile…", waitForText: "Also remove this profile’s home")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(prefix)-delete",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .sheet,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow], requiredTexts: ["Delete Profile", "Cancel", "Also remove this profile’s home"], uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Cancel", waitForText: "Recheck Profile")
            try await context.pressControl(named: "Delete Profile…", waitForText: "Also remove this profile’s home")
            try await context.pressControl(named: "Delete Profile", waitForText: "This agent was removed")
            try await context.capture(ScenarioCaptureRequest(
                controlName: "settings-window", captureID: "\(prefix)-removed",
                scopeIdentifier: AccessibilityID.settingsWindow, snapshotPolicy: .route,
                auditPolicy: .route(requiredIdentifiers: [AccessibilityID.settingsWindow], requiredTexts: ["This agent was removed"], uniqueIdentifiers: AccessibilityID.all)
            ))
            try await context.pressControl(named: "Back", waitForText: "Instances")
            try await context.pressControl(named: "Back", waitForText: "Check Again")
        }
    }
}
