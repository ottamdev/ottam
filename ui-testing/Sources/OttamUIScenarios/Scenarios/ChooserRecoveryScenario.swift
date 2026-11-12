import OttamAccessibility
import ScenarioKit

public extension UIScenarioDefinition {
    static let chooserStartupError = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "core.chooser-startup-error",
            description: "Show a real pre-initialize database error and its recovery details in the chooser.",
            tags: ["core", "chooser", "startup"],
            requirements: ["Accessibility", "Screen Recording", "real Rust ottam-core"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "none",
            capturesChooser: true,
            fixtureRequirements: [.incompatibleStorage]
        )
    ) { context in
        try await context.waitFor(.exists(context.element(id: AccessibilityID.chooserCoreFailure)), timeout: 25)
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "workspace-chooser",
                captureID: "chooser-startup-error",
                scopeIdentifier: nil,
                snapshotPolicy: .wholeApplicationDiagnostic,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.workspaceChooser, AccessibilityID.chooserCoreFailure, AccessibilityID.chooserCoreRetry],
                    requiredTexts: ["the database was created by a newer Ottam version"],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        let diagnostics = context.element(id: AccessibilityID.coreDiagnosticsToggle)
        try await context.press(diagnostics)
        try await context.waitFor(.valueEquals(diagnostics, "true"), timeout: 5)
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "workspace-chooser",
                captureID: "chooser-startup-error-expanded",
                scopeIdentifier: nil,
                snapshotPolicy: .wholeApplicationDiagnostic,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.chooserCoreFailure, AccessibilityID.coreDiagnosticsToggle],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.press(diagnostics)
        try await context.waitFor(.valueEquals(diagnostics, "false"), timeout: 5)
    }

    static let chooserCoreRecovery = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "core.chooser-recovery",
            description: "Replace the workspace chooser with a bounded failure view, then restore it on retry.",
            tags: ["core", "chooser", "recovery"],
            requirements: ["Accessibility", "Screen Recording", "real Rust ottam-core"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "none",
            capturesChooser: true,
            fixtureRequirements: [.coreFailure]
        )
    ) { context in
        let failure = context.element(id: AccessibilityID.chooserCoreFailure)
        let retry = context.element(id: AccessibilityID.chooserCoreRetry)
        try await context.waitFor(.exists(context.element(id: AccessibilityID.workspaceChooser)), timeout: 20)
        try context.setBundledCoreAvailable(false)
        try await context.terminateOwnedCore(expectReplacement: false)
        try await context.waitFor(.exists(failure), timeout: 25)
        try await context.assert(.notExists(context.element(id: AccessibilityID.coreRecoverySheet)))
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "workspace-chooser",
                captureID: "chooser-core-failure",
                scopeIdentifier: nil,
                snapshotPolicy: .wholeApplicationDiagnostic,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.workspaceChooser, AccessibilityID.chooserCoreFailure, AccessibilityID.chooserCoreRetry],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try context.setBundledCoreAvailable(true)
        try await context.press(retry)
        try await context.waitFor(.notExists(failure), timeout: 20)
        let newWorkspace = context.element(id: AccessibilityID.chooserNewWorkspace)
        try await context.waitFor(.exists(newWorkspace), timeout: 20)
        try await context.assert(.enabled(newWorkspace, true))
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "workspace-chooser",
                captureID: "chooser-restored",
                scopeIdentifier: nil,
                snapshotPolicy: .wholeApplicationDiagnostic,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.workspaceChooser, AccessibilityID.chooserNewWorkspace],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
    }
}
