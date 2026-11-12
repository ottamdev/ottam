import Foundation

public enum AXAuditRule: String, Codable, CaseIterable, Hashable, Sendable {
    case requiredIdentifier = "required_identifier"
    case requiredText = "required_text"
    case duplicateIdentifier = "duplicate_identifier"
    case missingAccessibleName = "missing_accessible_name"
    case requiredAction = "required_action"
    case focusedElement = "focused_element"
}

public struct AXAuditException: Codable, Equatable, Sendable {
    public let rule: AXAuditRule
    public let identifier: String?
    public let role: String?
    public let reason: String

    public var isNarrowlyDocumented: Bool {
        identifier?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false
            && !reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    public init(
        rule: AXAuditRule,
        identifier: String? = nil,
        role: String? = nil,
        reason: String
    ) {
        self.rule = rule
        self.identifier = identifier
        self.role = role
        self.reason = reason
    }
}

public struct AXRequiredAction: Codable, Equatable, Sendable {
    public let identifier: String
    public let actions: [String]

    public init(identifier: String, actions: [String]) {
        self.identifier = identifier
        self.actions = actions
    }
}

public struct AXAuditPolicy: Codable, Equatable, Sendable {
    public let name: String
    public let enabledRules: [AXAuditRule]
    public let requiredIdentifiers: [String]
    public let requiredTexts: [String]
    public let uniqueIdentifiers: [String]
    public let requiredActions: [AXRequiredAction]
    public let requiredFocusedIdentifier: String?
    public let exceptions: [AXAuditException]

    public init(
        name: String,
        enabledRules: Set<AXAuditRule>,
        requiredIdentifiers: [String] = [],
        requiredTexts: [String] = [],
        uniqueIdentifiers: [String] = [],
        requiredActions: [AXRequiredAction] = [],
        requiredFocusedIdentifier: String? = nil,
        exceptions: [AXAuditException] = []
    ) {
        self.name = name
        self.enabledRules = enabledRules.sorted { $0.rawValue < $1.rawValue }
        self.requiredIdentifiers = requiredIdentifiers
        self.requiredTexts = requiredTexts
        self.uniqueIdentifiers = uniqueIdentifiers
        self.requiredActions = requiredActions
        self.requiredFocusedIdentifier = requiredFocusedIdentifier
        self.exceptions = exceptions
    }

    public static let diagnostic = AXAuditPolicy(
        name: "diagnostic",
        enabledRules: []
    )

    public static func route(
        requiredIdentifiers: [String],
        requiredTexts: [String] = [],
        uniqueIdentifiers: [String] = [],
        requiredActions: [AXRequiredAction] = [],
        requiredFocusedIdentifier: String? = nil,
        exceptions: [AXAuditException] = []
    ) -> AXAuditPolicy {
        AXAuditPolicy(
            name: "route",
            enabledRules: [
                .requiredIdentifier,
                .requiredText,
                .duplicateIdentifier,
                .missingAccessibleName,
                .requiredAction,
                .focusedElement,
            ],
            requiredIdentifiers: requiredIdentifiers,
            requiredTexts: requiredTexts,
            uniqueIdentifiers:
                uniqueIdentifiers.isEmpty ? requiredIdentifiers : uniqueIdentifiers,
            requiredActions: requiredActions,
            requiredFocusedIdentifier: requiredFocusedIdentifier,
            exceptions: exceptions
        )
    }
}

public struct AXAuditIssue: Codable, Equatable, Sendable {
    public let rule: AXAuditRule
    public let identifier: String?
    public let role: String?
    public let message: String
    public let excepted: Bool
    public let exceptionReason: String?
}

public struct AXAuditReport: Codable, Equatable, Sendable {
    public let schemaVersion: Int
    public let policyName: String
    public let inspectedNodes: Int
    public let issues: [AXAuditIssue]
    public let blockingIssueCount: Int
    public let exceptionCount: Int

    public init(
        schemaVersion: Int,
        policyName: String,
        inspectedNodes: Int,
        issues: [AXAuditIssue]
    ) {
        self.schemaVersion = schemaVersion
        self.policyName = policyName
        self.inspectedNodes = inspectedNodes
        self.issues = issues
        blockingIssueCount = issues.lazy.filter { !$0.excepted }.count
        exceptionCount = issues.lazy.filter(\.excepted).count
    }
}

public enum AXAccessibilityAudit {
    public static func audit(
        _ capture: AXTreeCapture,
        policy: AXAuditPolicy
    ) -> AXAuditReport {
        let nodes = flattened(capture.root)
        var issues: [AXAuditIssue] = []
        let identifiers = Dictionary(grouping: nodes.compactMap { node in
            node.identifier.map { ($0, node) }
        }, by: \.0)

        if policy.enabledRules.contains(.requiredIdentifier) {
            for identifier in policy.requiredIdentifiers
            where identifiers[identifier]?.isEmpty != false {
                issues.append(
                    issue(
                        rule: .requiredIdentifier,
                        identifier: identifier,
                        role: nil,
                        message: "required identifier \(identifier) is missing",
                        policy: policy
                    )
                )
            }
        }

        if policy.enabledRules.contains(.requiredText) {
            let visibleTexts = Set(nodes.flatMap { node in
                [node.label, node.title, node.value, node.placeholder].compactMap { $0 }
            })
            for requiredText in policy.requiredTexts
            where !visibleTexts.contains(where: { $0.contains(requiredText) }) {
                issues.append(
                    issue(
                        rule: .requiredText,
                        identifier: requiredText,
                        role: nil,
                        message: "required accessible text \(requiredText) is missing",
                        policy: policy
                    )
                )
            }
        }

        if policy.enabledRules.contains(.duplicateIdentifier) {
            for (identifier, matches) in identifiers
            where matches.count > 1
                && policy.uniqueIdentifiers.contains(identifier)
                && !identifier.hasSuffix(":") {
                issues.append(
                    issue(
                        rule: .duplicateIdentifier,
                        identifier: identifier,
                        role: nil,
                        message: "identifier \(identifier) appears \(matches.count) times",
                        policy: policy
                    )
                )
            }
        }

        if policy.enabledRules.contains(.missingAccessibleName) {
            let interactiveRoles: Set<String> = [
                "AXButton", "AXCheckBox", "AXComboBox", "AXLink",
                "AXPopUpButton", "AXRadioButton", "AXTextField",
            ]
            for node in nodes
            where node.role.map(interactiveRoles.contains) == true
                && node.identifier?.contains(".") == true
                && [node.label, node.title, node.value, node.placeholder]
                .allSatisfy({ ($0 ?? "").isEmpty })
            {
                issues.append(
                    issue(
                        rule: .missingAccessibleName,
                        identifier: node.identifier,
                        role: node.role,
                        message: "interactive \(node.role ?? "element") has no accessible name",
                        policy: policy
                    )
                )
            }
        }

        if policy.enabledRules.contains(.requiredAction) {
            for requirement in policy.requiredActions {
                guard let node = identifiers[requirement.identifier]?.first?.1 else {
                    continue
                }
                let actions = Set(node.actions ?? [])
                for action in requirement.actions where !actions.contains(action) {
                    issues.append(
                        issue(
                            rule: .requiredAction,
                            identifier: requirement.identifier,
                            role: node.role,
                            message: "\(requirement.identifier) does not expose \(action)",
                            policy: policy
                        )
                    )
                }
            }
        }

        if policy.enabledRules.contains(.focusedElement),
           let requiredFocusedIdentifier = policy.requiredFocusedIdentifier,
           identifiers[requiredFocusedIdentifier]?.contains(where: {
               $0.1.focused == true
           }) != true
        {
            issues.append(
                issue(
                    rule: .focusedElement,
                    identifier: requiredFocusedIdentifier,
                    role: nil,
                    message: "\(requiredFocusedIdentifier) is not focused",
                    policy: policy
                )
            )
        }

        return AXAuditReport(
            schemaVersion: 1,
            policyName: policy.name,
            inspectedNodes: nodes.count,
            issues: issues
        )
    }

    private static func flattened(_ root: NormalizedAXNode) -> [NormalizedAXNode] {
        var result: [NormalizedAXNode] = []
        var pending = [root]
        while let node = pending.popLast() {
            result.append(node)
            pending.append(contentsOf: node.children.reversed())
        }
        return result
    }

    private static func issue(
        rule: AXAuditRule,
        identifier: String?,
        role: String?,
        message: String,
        policy: AXAuditPolicy
    ) -> AXAuditIssue {
        let exception = policy.exceptions.first {
            $0.isNarrowlyDocumented
                && $0.rule == rule
                && ($0.identifier == nil || $0.identifier == identifier)
                && ($0.role == nil || $0.role == role)
        }
        return AXAuditIssue(
            rule: rule,
            identifier: identifier,
            role: role,
            message: message,
            excepted: exception != nil,
            exceptionReason: exception?.reason
        )
    }
}
