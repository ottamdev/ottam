import OttamAccessibility
import MacAutomationKit
import ScenarioKit

/// Native-renderer evidence: the DEBUG lab window renders each fixture through
/// the production AppKit transcript controller. Every fixture is captured in
/// both appearances, so the review covers each projected block kind rather than
/// only the kitchen sink.
public extension UIScenarioDefinition {
    static let markdownEngineLabKitchenSinkLight =
        markdownLabGalleryScenario(fixture: "kitchen-sink", appearance: "light")
    static let markdownEngineLabKitchenSinkDark =
        markdownLabGalleryScenario(fixture: "kitchen-sink", appearance: "dark")
    static let markdownEngineLabCodeHeavyLight =
        markdownLabGalleryScenario(fixture: "code-heavy", appearance: "light")
    static let markdownEngineLabCodeHeavyDark =
        markdownLabGalleryScenario(fixture: "code-heavy", appearance: "dark")
    static let markdownEngineLabTableHeavyLight =
        markdownLabGalleryScenario(fixture: "table-heavy", appearance: "light")
    static let markdownEngineLabTableHeavyDark =
        markdownLabGalleryScenario(fixture: "table-heavy", appearance: "dark")
    static let markdownEngineLabListsAndQuotesLight =
        markdownLabGalleryScenario(fixture: "lists-and-quotes", appearance: "light")
    static let markdownEngineLabListsAndQuotesDark =
        markdownLabGalleryScenario(fixture: "lists-and-quotes", appearance: "dark")
    static let markdownEngineLabImagesLight =
        markdownLabGalleryScenario(fixture: "images", appearance: "light")
    static let markdownEngineLabImagesDark =
        markdownLabGalleryScenario(fixture: "images", appearance: "dark")
    static let markdownEngineLabUserInlineLight =
        markdownLabGalleryScenario(fixture: "user-inline", appearance: "light")
    static let markdownEngineLabUserInlineDark =
        markdownLabGalleryScenario(fixture: "user-inline", appearance: "dark")
}

private func markdownLabGalleryScenario(
    fixture: String, appearance: String
) -> UIScenarioDefinition {
    UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "markdown-lab.\(fixture)-\(appearance)",
            description: "Capture the markdown engine lab's gallery for the "
                + "\(fixture) fixture in \(appearance) appearance (native AppKit renderer).",
            tags: ["markdown", "engine", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "DEBUG app build"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            // "none" parses to an empty route list: the workspace window opens
            // but publishes no route captures; only the lab window is captured.
            routes: "none",
            environment: [
                "OTTAM_UI_TEST_CAPTURE_MARKDOWN_LAB": "1",
                "OTTAM_UI_TEST_MARKDOWN_LAB_FIXTURE": fixture,
                "OTTAM_UI_TEST_APPEARANCE": appearance,
            ],
            fixtures: ["in-app markdown fixture corpus (\(fixture))"]
        )
    ) { context in
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "markdown-lab-window",
                captureID: "markdown-lab-\(fixture)-\(appearance)",
                scopeIdentifier: AccessibilityID.markdownEngineLab,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.markdownEngineLab],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
    }
}

extension UIScenarioDefinition {
    /// A SwiftUI `ChatPermissionCard` hosted as a native collection item through
    /// `NSHostingView`. The AX audit proves its accessibility identifiers
    /// survive production hosting, and pressing Allow Once proves its action
    /// reaches the SwiftUI lab state.
    static let markdownEngineLabCards = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "markdown-lab.cards",
            description: "Capture the markdown engine lab's Cards tab: a hosted SwiftUI "
                + "permission card inside the native AppKit transcript, then approve it.",
            tags: ["markdown", "engine", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "DEBUG app build"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "none",
            environment: [
                "OTTAM_UI_TEST_CAPTURE_MARKDOWN_LAB": "1",
                "OTTAM_UI_TEST_MARKDOWN_LAB_TAB": "cards",
                "OTTAM_UI_TEST_APPEARANCE": "dark",
            ],
            fixtures: ["in-app preview permission (lab)"]
        )
    ) { context in
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "markdown-lab-window",
                captureID: "markdown-lab-cards-pending",
                scopeIdentifier: AccessibilityID.markdownEngineLab,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [
                        AccessibilityID.markdownEngineLab,
                        AccessibilityID.chatPermissionCard,
                        AccessibilityID.markdownEngineLabCardResolved,
                    ],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
        try await context.pressControl(named: "Allow Once", waitForText: "Resolved: allow_once")
        try await context.capture(
            ScenarioCaptureRequest(
                controlName: "markdown-lab-window",
                captureID: "markdown-lab-cards-resolved",
                scopeIdentifier: AccessibilityID.markdownEngineLab,
                snapshotPolicy: .route,
                auditPolicy: .route(
                    requiredIdentifiers: [AccessibilityID.markdownEngineLabCardResolved],
                    uniqueIdentifiers: AccessibilityID.all
                )
            )
        )
    }
}
