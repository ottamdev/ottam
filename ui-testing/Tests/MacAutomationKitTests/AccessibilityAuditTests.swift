import MacAutomationKit
import XCTest

final class AccessibilityAuditTests: XCTestCase {
    func testRequiredAccessibleTextIsCheckedExactly() {
        let capture = auditCapture(
            children: [auditNode(label: "Theme picker")]
        )
        let policy = AXAuditPolicy(
            name: "page",
            enabledRules: [.requiredText],
            requiredTexts: ["Theme", "Ghostty Overlay"]
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertEqual(report.blockingIssueCount, 1)
        XCTAssertEqual(report.issues.first?.rule, .requiredText)
        XCTAssertEqual(report.issues.first?.identifier, "Ghostty Overlay")
    }

    func testRequiredAndDuplicateIdentifiersAreBlocking() {
        let capture = auditCapture(
            children: [
                auditNode(identifier: "chat.submit"),
                auditNode(identifier: "chat.submit"),
            ]
        )
        let policy = AXAuditPolicy(
            name: "chat",
            enabledRules: [.requiredIdentifier, .duplicateIdentifier],
            requiredIdentifiers: ["chat.surface", "chat.submit"],
            uniqueIdentifiers: ["chat.surface", "chat.submit"]
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertEqual(report.blockingIssueCount, 2)
        XCTAssertTrue(report.issues.contains { $0.rule == .requiredIdentifier })
        XCTAssertTrue(report.issues.contains { $0.rule == .duplicateIdentifier })
    }

    func testMissingNameCanHaveNarrowDocumentedException() {
        let capture = auditCapture(
            children: [
                auditNode(role: "AXButton", identifier: "terminal.internal-button"),
            ]
        )
        let policy = AXAuditPolicy(
            name: "terminal",
            enabledRules: [.missingAccessibleName],
            exceptions: [
                AXAuditException(
                    rule: .missingAccessibleName,
                    identifier: "terminal.internal-button",
                    role: "AXButton",
                    reason: "Ghostty exposes this internal control without a user action."
                ),
            ]
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertEqual(report.blockingIssueCount, 0)
        XCTAssertEqual(report.exceptionCount, 1)
        XCTAssertEqual(
            report.issues.first?.exceptionReason,
            "Ghostty exposes this internal control without a user action."
        )
    }

    func testRequiredActionAndFocusRulesUseNormalizedState() {
        let capture = auditCapture(
            children: [
                auditNode(
                    role: "AXButton",
                    identifier: "chat.submit",
                    label: "Send",
                    focused: true,
                    actions: ["AXPress"]
                ),
            ]
        )
        let policy = AXAuditPolicy(
            name: "chat-action",
            enabledRules: [.requiredAction, .focusedElement],
            requiredActions: [
                AXRequiredAction(identifier: "chat.submit", actions: ["AXPress"]),
            ],
            requiredFocusedIdentifier: "chat.submit"
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertEqual(report.blockingIssueCount, 0)
    }

    func testBroadOrUndocumentedExceptionDoesNotSuppressIssue() {
        let capture = auditCapture(
            children: [
                auditNode(role: "AXButton", identifier: "chat.submit"),
            ]
        )
        let policy = AXAuditPolicy(
            name: "invalid-exception",
            enabledRules: [.missingAccessibleName],
            exceptions: [
                AXAuditException(
                    rule: .missingAccessibleName,
                    reason: ""
                ),
            ]
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertEqual(report.blockingIssueCount, 1)
        XCTAssertEqual(report.exceptionCount, 0)
    }

    func testRoutePolicyChecksContractDuplicatesAndNamedInteractiveElements() {
        let capture = auditCapture(
            children: [
                auditNode(
                    role: "AXButton",
                    identifier: "chat.submit",
                    label: "Send",
                    actions: ["AXPress"]
                ),
                auditNode(
                    role: "AXButton",
                    identifier: "chat.submit",
                    label: "Send",
                    actions: ["AXPress"]
                ),
                auditNode(
                    role: "AXButton",
                    identifier: "chat.retry"
                ),
                auditNode(
                    role: "AXButton"
                ),
            ]
        )
        let policy = AXAuditPolicy.route(
            requiredIdentifiers: ["chat.submit", "chat.retry"],
            uniqueIdentifiers: ["chat.submit", "chat.retry"]
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertTrue(report.issues.contains { $0.rule == .duplicateIdentifier })
        XCTAssertTrue(
            report.issues.contains {
                $0.rule == .missingAccessibleName
                    && $0.identifier == "chat.retry"
            }
        )
        XCTAssertFalse(
            report.issues.contains {
                $0.rule == .missingAccessibleName && $0.identifier == nil
            }
        )
    }

    func testRoutePolicyCanDocumentAnIntentionalPerWindowDuplicate() {
        let capture = auditCapture(
            children: [
                auditNode(identifier: "workspace.root"),
                auditNode(identifier: "workspace.root"),
            ]
        )
        let policy = AXAuditPolicy.route(
            requiredIdentifiers: ["workspace.root"],
            uniqueIdentifiers: ["workspace.root"],
            exceptions: [
                AXAuditException(
                    rule: .duplicateIdentifier,
                    identifier: "workspace.root",
                    reason: "One root is expected in each Workspace window."
                ),
            ]
        )

        let report = AXAccessibilityAudit.audit(capture, policy: policy)

        XCTAssertEqual(report.blockingIssueCount, 0)
        XCTAssertEqual(report.exceptionCount, 1)
    }
}

private func auditCapture(children: [NormalizedAXNode]) -> AXTreeCapture {
    AXTreeCapture(
        schemaVersion: 1,
        targetPID: 1,
        scopeIdentifier: nil,
        maxDepth: 12,
        maxNodes: 500,
        capturedNodes: children.count + 1,
        truncatedNodes: 0,
        omittedValues: 0,
        durationMilliseconds: 1,
        root: auditNode(children: children)
    )
}

private func auditNode(
    role: String? = "AXGroup",
    identifier: String? = nil,
    label: String? = nil,
    focused: Bool? = false,
    actions: [String] = [],
    children: [NormalizedAXNode] = []
) -> NormalizedAXNode {
    NormalizedAXNode(
        role: role,
        subrole: nil,
        identifier: identifier,
        label: label,
        title: nil,
        value: nil,
        placeholder: nil,
        frame: nil,
        enabled: true,
        selected: false,
        focused: focused,
        actions: actions,
        children: children
    )
}
