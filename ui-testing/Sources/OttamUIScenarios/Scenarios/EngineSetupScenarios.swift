import OttamAccessibility
import MacAutomationKit
import ScenarioKit

extension UIScenarioDefinition {
    /// The setup landing when no engine can take a message.
    ///
    /// The harness opts out of adapter installs so every other scenario gets a
    /// usable engine; this one opts back in against the run home, which is
    /// wiped before every run and therefore has no adapter installed. That is
    /// the whole fixture — both built-in agents report their pinned adapter
    /// missing, so nothing is usable.
    ///
    /// What this proves over `EngineSetupStateTests` and `PresentationLogicTests`:
    /// the view removes the entire composer and offers setup instead.
    public static let chatNoUsableEngine = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.no-usable-engine",
            description: "Capture the first-agent setup landing with no composer.",
            tags: ["chat", "engines", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: [
                // Deliberately back on: this scenario is the uninstalled state.
                "OTTAM_ADAPTER_INSTALLS": "on",
                "OTTAM_UI_TEST_APPEARANCE": "light",
            ],
            fixtures: ["no installed adapters (empty run home, adapter store on)"]
        ),
        preparation: waitForSetUpAgents
    ) { context in
        // Present-and-disabled would still satisfy the capture below, so assert
        // the send button is genuinely gone.
        try await context.waitFor(
            .notExists(context.element(id: AccessibilityID.chatSubmit)),
            timeout: 10
        )
        try await context.waitFor(
            .notExists(context.element(id: AccessibilityID.chatComposer)),
            timeout: 10
        )
        try await context.capture(
            routeCapture(
                id: "chat-no-usable-engine",
                required: [
                    AccessibilityID.chatSurface,
                    AccessibilityID.chatSetUpAgents,
                ],
                requiredActions: [
                    AXRequiredAction(
                        identifier: AccessibilityID.chatSetUpAgents,
                        actions: ["AXPress"]
                    )
                ]
            )
        )
        // Nothing installed means no sign-in to finish: the landing points at
        // Settings rather than reopening setup.
        try await context.assert(.labelContains(context.element(id: AccessibilityID.chatSetUpAgents), "Open Agent Settings"))
    }
    /// The picker while a runtime probe outruns the list budget, and the flip
    /// when it answers.
    ///
    /// A fixture login shell holds Node resolution open until the scenario
    /// releases it. Core's resolver gives that shell two 10s attempts, so the
    /// release has to land inside 20s of the first list; nothing here sleeps.
    public static let chatEngineChecking = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.engine-checking",
            description: "Keep Chat usable while a runtime probe runs, then re-read when it answers.",
            tags: ["chat", "engines", "runtime", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["login shell held until the scenario releases it"],
            fixtureRequirements: [.testACP, .heldLoginShell]
        ),
        preparation: { context in
            // The composer only mounts once a list came back, which is the
            // whole point: under the held shell that list says "checking".
            try await context.waitFor(
                .exists(context.element(id: AccessibilityID.chatComposer)),
                timeout: 15
            )
        }
    ) { context in
        let picker = context.element(id: AccessibilityID.chatEngineMenu)
        let checking = context.element(id: AccessibilityID.chatEngineChecking)
        // Also the wait for app preparation: its last step makes the window
        // key, which would dismiss a popover opened before it.
        try await context.capture(
            routeCapture(
                id: "chat-engine-checking-composer",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatEngineMenu]
            )
        )
        try await context.press(picker)
        try await context.waitFor(.exists(checking), timeout: 5)
        try await context.capture(
            routeCapture(
                id: "chat-engine-checking",
                scope: AccessibilityID.chatEngineMenuPopover,
                required: [AccessibilityID.chatEngineChecking],
                auditExceptions: [
                    AXAuditException(
                        rule: .duplicateIdentifier,
                        identifier: AccessibilityID.chatEngineChecking,
                        reason: "Every Node harness waits on the same resolver, so each row is checking."
                    )
                ]
            )
        )
        try await context.pressControl(named: "Codex, Checking…", waitForText: "Codex")
        try await context.waitFor(.labelContains(picker, "Codex"), timeout: 5)
        try await context.publishACPMarker("release-login-shell")
        // No action from here on: the rows have to change on `engine.changed`.
        try await context.waitFor(.notExists(checking), timeout: 15)
        try await context.assert(.labelContains(picker, "Codex"))
        try await context.capture(
            routeCapture(
                id: "chat-engine-settled",
                scope: AccessibilityID.chatEngineMenuPopover,
                required: []
            )
        )
    }

}

/// Wait until detection resolves and the setup action is available.
private func waitForSetUpAgents(_ context: any UIScenarioContext) async throws {
    try await context.waitFor(
        .exists(context.element(id: AccessibilityID.chatSetUpAgents)),
        timeout: 30
    )
}
