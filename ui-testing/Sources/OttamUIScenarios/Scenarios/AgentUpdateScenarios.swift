import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    /// A moved pin with the older install still on disk: chat keeps its
    /// composer, and Settings → Agents reports each background upgrade as an
    /// update — the managed Node download failing with Retry, then Claude's
    /// adapter Updating and, once its npm fails, Retry with the old version
    /// still named as working. The Node shim and the offline dist base keep
    /// the run off the network; a system Node 22+ on the host would take
    /// precedence over the seeded managed runtime and hide its Update row.
    static let settingsAgentUpdate = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.agent-update",
            description: "Capture the background pin-upgrade states for the managed "
                + "Node runtime and the Claude Code adapter.",
            tags: ["settings", "agents", "runtime", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            capturesSettings: true,
            environment: [
                // The upgrade is the subject, so the adapter store is on.
                "OTTAM_ADAPTER_INSTALLS": "on",
                "OTTAM_NODE_DIST_BASE": "http://127.0.0.1:9/dist",
                "OTTAM_NODE_DIST_SHA256": String(repeating: "0", count: 64),
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                "OTTAM_UI_TEST_APPEARANCE": "light",
            ],
            fixtures: ["Claude adapter and managed Node older than their pins, offline"],
            fixtureRequirements: [.deterministicProject, .supersededAgentInstalls]
        )
    ) { context in
        // The stand-in install keeps Claude usable, so chat offers the
        // composer rather than first-agent setup.
        try await context.capture(
            routeCapture(id: "route-chat", required: [AccessibilityID.chatComposer])
        )
        try await context.waitFor(
            .exists(context.element(id: AccessibilityID.settingsWindow)),
            timeout: 15
        )
        // Waits match a whole accessible text, so they key on the Retry button
        // titles; the captures check the version-free parts of the details.
        try await context.selectListRow(named: "Agents", waitForText: "Retry")
        try await context.capture(
            settingsCapture(
                id: "settings-agents-runtime-update-failed",
                requiredIdentifiers: ["settings.agents.runtimeUpdate"],
                requiredTexts: ["Couldn’t update to Node", "Retry", "keep using the current version"]
            )
        )
        try await context.pressControl(named: "Claude Code", waitForText: "Instances")
        try await context.pressControl(named: "Ottam profile", waitForText: "Compatibility")
        try await context.capture(
            settingsCapture(
                id: "settings-agent-adapter-updating",
                requiredTexts: ["Adapter", "Updating to", "0.0.9 →"]
            )
        )
        try context.publishACPMarker("release-npm")
        try await context.pressControl(named: "Back", waitForText: "Instances")
        // "Retry" alone would also match Node's row; the capture's ECONNREFUSED
        // is what pins the failure to the adapter.
        try await context.pressControl(named: "Ottam profile", waitForText: "Retry")
        try await context.capture(
            settingsCapture(
                id: "settings-agent-adapter-update-failed",
                requiredTexts: ["Couldn’t update to", "ECONNREFUSED", "Version 0.0.9 keeps working"]
            )
        )
    }
}

private func settingsCapture(
    id: String,
    requiredIdentifiers: [String] = [],
    requiredTexts: [String]
) -> ScenarioCaptureRequest {
    ScenarioCaptureRequest(
        controlName: "settings-window",
        captureID: id,
        scopeIdentifier: AccessibilityID.settingsWindow,
        snapshotPolicy: .route,
        auditPolicy: .route(
            requiredIdentifiers: [AccessibilityID.settingsWindow] + requiredIdentifiers,
            requiredTexts: requiredTexts,
            uniqueIdentifiers: AccessibilityID.all
        )
    )
}
