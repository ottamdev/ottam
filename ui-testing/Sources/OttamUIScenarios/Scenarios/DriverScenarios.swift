import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let driverReadinessDelay = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "driver.readiness-delay",
            description: "Prove route capture waits for delayed readiness publication.",
            tags: ["driver", "readiness", "focused"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_READINESS_DELAY_MS": "750"],
            fixtures: ["delayed Chat readiness publication"]
        ),
        preparation: { context in
            let composer = context.element(id: AccessibilityID.chatComposer)
            try await context.waitFor(.exists(composer), timeout: 20)
        }
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        try await context.assert(.exists(composer))
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "route-chat",
                captureID: "readiness-delay",
                scopeIdentifier: AccessibilityID.workspaceDetail,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.workspaceDetail,
                        AccessibilityID.chatComposer,
                    ]
                )
            )
        )
    }

    static let driverForcedFailure = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "driver.forced-failure",
            description: "Exercise retained driver-failure evidence.",
            tags: ["diagnostic"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat"
        )
    ) { context in
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "route-chat",
                captureID: "failure",
                scopeIdentifier: nil,
                snapshotPolicy: .wholeApplicationDiagnostic,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.workspaceDetail,
                        AccessibilityID.chatComposer,
                        AccessibilityID.chatSubmit,
                    ]
                )
            )
        )
        try await context.forceAssertionFailure()
    }
}
