import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    /// No usable Node and no adapter installed: Codex's and Claude Code's Get
    /// waits for Node with a pointer at the Runtime section, while the
    /// prebuilt binaries (OpenCode, Oh-My-Pi) stay installable. `OTTAM_NODE`
    /// at a missing path makes the missing Node deterministic on any host.
    static let settingsAgentInstallNeedsNode = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "settings.agent-install-needs-node",
            description: "Gate Get on Node for npm adapters in Settings, not for prebuilt binaries.",
            tags: ["settings", "agents", "runtime", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            // No agent can start, so Chat shows its setup landing; the run
            // reaches Settings through that landing's button, as a user would.
            routes: "chat",
            capturesSettings: true,
            environment: [
                // The uninstalled state is the subject, so the store is on.
                "OTTAM_ADAPTER_INSTALLS": "on",
                "OTTAM_NODE": "/nonexistent/ottam-ui-node",
                "OTTAM_UI_TEST_APPEARANCE": "light",
            ],
            fixtures: ["No usable Node, no adapter installed"],
            fixtureRequirements: [.deterministicProject]
        )
    ) { context in
        let action = context.element(id: AccessibilityID.settingsAgentAdapterAction)
        let setUpAgents = context.element(id: AccessibilityID.chatSetUpAgents)
        try await context.waitFor(.exists(setUpAgents), timeout: 30)
        try await context.press(setUpAgents)
        try await context.waitFor(
            .exists(context.element(id: AccessibilityID.settingsWindow)),
            timeout: 30
        )
        try await context.selectListRow(named: "Agents", waitForText: "Install Node.js")
        try await context.capture(
            needsNodeCapture(id: "settings-agents-node-missing", requiredTexts: ["Node.js", "Install Node.js"])
        )
        for (provider, needsNode) in [
            ("Codex", true), ("Claude Code", true), ("OpenCode", false), ("Oh-My-Pi", false),
        ] {
            try await context.pressControl(named: provider, waitForText: "Instances")
            try await context.pressControl(named: "Ottam profile", waitForText: "Compatibility")
            try await context.assert(.enabled(action, !needsNode))
            let slug = provider.lowercased().replacingOccurrences(of: " ", with: "-")
            try await context.capture(
                needsNodeCapture(
                    id: "settings-agent-\(slug)-get",
                    requiredTexts: needsNode
                        ? ["Get", "Needs Node.js. Install it under Runtime below."]
                        : ["Get", "Prebuilt binary"]
                )
            )
            try await context.pressControl(named: "Back", waitForText: "Instances")
            try await context.pressControl(named: "Back", waitForText: "Install Node.js")
        }
    }
}

private func needsNodeCapture(id: String, requiredTexts: [String]) -> ScenarioCaptureRequest {
    ScenarioCaptureRequest(
        controlName: "settings-window",
        captureID: id,
        scopeIdentifier: AccessibilityID.settingsWindow,
        snapshotPolicy: .route,
        auditPolicy: .route(
            requiredIdentifiers: [AccessibilityID.settingsWindow],
            requiredTexts: requiredTexts,
            uniqueIdentifiers: AccessibilityID.all
        )
    )
}
