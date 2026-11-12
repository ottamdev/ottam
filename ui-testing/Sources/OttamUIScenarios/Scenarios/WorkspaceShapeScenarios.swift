import OttamAccessibility
import MacAutomationKit
import ScenarioKit

/// Layout evidence for the sidebar's 0/1/2-project shapes in both appearances,
/// plus a two-window capture proving that each sidebar exposes its Workspace
/// identity independently.
public extension UIScenarioDefinition {
    static let workspaceZeroProjectShapeLight = zeroProjectShapeScenario(appearance: "light")
    static let workspaceZeroProjectShapeDark = zeroProjectShapeScenario(appearance: "dark")

    static let workspaceSingleProjectShapeLight = singleProjectShapeScenario(appearance: "light")
    static let workspaceSingleProjectShapeDark = singleProjectShapeScenario(appearance: "dark")

    static let workspaceMultiProjectShapeLight = multiProjectShapeScenario(appearance: "light")
    static let workspaceMultiProjectShapeDark = multiProjectShapeScenario(appearance: "dark")

    static let workspaceTwoWindowIdentityCapture = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "workspace.two-window-identity-capture",
            description: "Capture the sidebar identity block from two distinctly-named Workspace windows at once.",
            tags: ["workspace", "sidebar", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "real Rust ottam-core"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_WORKSPACE_COUNT": "2"],
            fixtures: ["two distinctly-named Workspace windows"],
            fixtureRequirements: [.workspaceWindows]
        ),
        preparation: waitForChat
    ) { context in
        let workspace = context.element(id: AccessibilityID.workspaceRoot)
        let identity = context.element(id: AccessibilityID.sidebarIdentityBlock)
        try await context.waitFor(.identifierCountEquals(workspace, 2), timeout: 20)
        try await context.waitFor(.identifierCountEquals(identity, 2), timeout: 20)
        try await context.waitFor(
            .distinctIdentifierLabelCountEquals(identity, 2),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "workspace-two-window-identity",
                scope: nil,
                windowGroupFixtureName: "workspace-window-lifecycle",
                required: [
                    AccessibilityID.workspaceRoot,
                    AccessibilityID.sidebarIdentityBlock,
                    AccessibilityID.chatComposer,
                ],
                auditExceptions: workspaceWindowAuditExceptions
            )
        )
    }
}

private func zeroProjectShapeScenario(appearance: String) -> UIScenarioDefinition {
    UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "workspace.zero-project-shape-\(appearance)",
            description: "Capture the 0-project sidebar shape in \(appearance) appearance: "
                + "seeded History list and identity block, with no per-project sections.",
            tags: ["workspace", "sidebar", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: shapeLaunchConfiguration(
            environment: ["OTTAM_SCREENSHOT_EMPTY_WORKSPACE": "1"],
            appearance: appearance
        )
    ) { context in
        try await context.waitFor(
            .exists(context.element(id: AccessibilityID.sidebarIdentityBlock)),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "workspace-zero-project-shape-\(appearance)",
                scope: AccessibilityID.workspaceRoot,
                required: [
                    AccessibilityID.workspaceSidebar,
                    AccessibilityID.sidebarIdentityBlock,
                    AccessibilityID.sidebarHistorySection,
                ]
            )
        )
    }
}

private func singleProjectShapeScenario(appearance: String) -> UIScenarioDefinition {
    UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "workspace.single-project-shape-\(appearance)",
            description: "Capture the 1-project sidebar shape in \(appearance) appearance: "
                + "flat seeded History list, no working-root chooser.",
            tags: ["workspace", "sidebar", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: shapeLaunchConfiguration(environment: [:], appearance: appearance),
        preparation: waitForChat
    ) { context in
        try await context.waitFor(
            .notExists(context.element(id: AccessibilityID.chatWorkingRootMenu)),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "workspace-single-project-shape-\(appearance)",
                scope: AccessibilityID.workspaceRoot,
                required: [
                    AccessibilityID.workspaceSidebar,
                    AccessibilityID.sidebarIdentityBlock,
                    AccessibilityID.sidebarHistorySection,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }
}

private func multiProjectShapeScenario(appearance: String) -> UIScenarioDefinition {
    UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "workspace.multi-project-shape-\(appearance)",
            description: "Capture the 2-project sidebar shape in \(appearance) appearance: "
                + "seeded project sections, trailing Chats bucket, and working-root chooser.",
            tags: ["workspace", "sidebar", "acceptance"],
            requirements: ["Accessibility", "Screen Recording"]
        ),
        launchConfiguration: shapeLaunchConfiguration(
            environment: [:],
            appearance: appearance,
            fixtureRequirements: [.additionalProject]
        ),
        preparation: waitForChat
    ) { context in
        try await context.waitFor(
            .exists(context.element(id: AccessibilityID.chatWorkingRootMenu)),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "workspace-multi-project-shape-\(appearance)",
                scope: AccessibilityID.workspaceRoot,
                required: [
                    AccessibilityID.workspaceSidebar,
                    AccessibilityID.sidebarIdentityBlock,
                    AccessibilityID.sidebarChatsSection,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }
}

private func shapeLaunchConfiguration(
    environment: [String: String],
    appearance: String,
    fixtureRequirements: [ScenarioFixtureRequirement] = []
) -> ScenarioLaunchConfiguration {
    var resolved = environment
    resolved["OTTAM_UI_TEST_APPEARANCE"] = appearance
    resolved["OTTAM_SCREENSHOT_CONVERSATION"] = "1"
    return ScenarioLaunchConfiguration(
        validationProfile: "focused",
        routes: "chat",
        environment: resolved,
        fixtures: fixtureRequirements.isEmpty
            ? []
            : ["a second Git-backed project on the primary Workspace"],
        fixtureRequirements: fixtureRequirements
    )
}
