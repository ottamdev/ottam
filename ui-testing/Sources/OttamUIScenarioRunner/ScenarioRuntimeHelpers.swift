import Foundation
import MacAutomationKit
import ScenarioKit

// A condition reads a route-scoped AX element through the same process that
// produces route evidence. Reuse that named budget so an animated semantic
// transition is not held to a shorter, unrelated timeout.
private let conditionInspectionTimeout =
    SemanticSnapshotPolicy.route.timeoutSeconds
private let conditionDriverTimeout: TimeInterval = conditionInspectionTimeout + 1

extension ScenarioRuntimeContext {
    func waitForWindowID(
        _ name: String,
        timeout: TimeInterval
    ) async throws -> UInt32 {
        let url = paths.control.appendingPathComponent("\(name).window-id")
        let workspacePreparationURL = paths.control
            .appendingPathComponent("fixtures", isDirectory: true)
            .appendingPathComponent("workspace-preparation.json")
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let data = try? Data(contentsOf: workspacePreparationURL),
               let object = try? JSONSerialization.jsonObject(with: data)
                    as? [String: Any],
               let status = object["status"] as? String,
               status != "passed"
            {
                throw UIScenarioFailure.fixture(
                    object["message"] as? String
                        ?? "workspace preparation failed"
                )
            }
            if let raw = try? String(contentsOf: url, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines),
                let windowID = UInt32(raw)
            {
                return windowID
            }
            guard ProcessIdentity.forPID(launched.processIdentity.pid)
                    == launched.processIdentity
            else {
                throw ScenarioRuntimeError.applicationExited(
                    launched.processIdentity.pid
                )
            }
            try? await Task.sleep(for: .milliseconds(100))
        }
        throw ScenarioRuntimeError.waitTimedOut(name)
    }

    func evaluate(_ condition: UICondition) throws -> Bool {
        let outputURL = paths.temporary
            .appendingPathComponent("condition-\(UUID().uuidString.lowercased()).json")
        switch condition {
        case let .identifierCountEquals(element, expectedCount):
            let response = try client.invoke(
                command: "semantic-tree",
                parameters: [
                    "pid": launched.processIdentity.pid,
                    "outputPath": outputURL.path,
                    "maxDepth": 12,
                    "maxNodes": 500,
                    "maxValueLength": 2_000,
                    "timeout": conditionInspectionTimeout,
                ],
                timeout: conditionDriverTimeout
            )
            guard response.status == "passed" else {
                throw scenarioDriverFailure(
                    response,
                    fallback: "semantic condition inspection failed"
                )
            }
            let capture = try decode(AXTreeCapture.self, from: response.result)
            return countIdentifiers(
                element.identifier,
                in: capture.root
            ) == expectedCount
        case let .distinctIdentifierLabelCountEquals(element, expectedCount):
            let response = try client.invoke(
                command: "semantic-tree",
                parameters: [
                    "pid": launched.processIdentity.pid,
                    "outputPath": outputURL.path,
                    "maxDepth": 12,
                    "maxNodes": 500,
                    "maxValueLength": 2_000,
                    "timeout": conditionInspectionTimeout,
                ],
                timeout: conditionDriverTimeout
            )
            guard response.status == "passed" else {
                throw scenarioDriverFailure(
                    response,
                    fallback: "semantic condition inspection failed"
                )
            }
            let capture = try decode(AXTreeCapture.self, from: response.result)
            return Set(
                identifierLabels(element.identifier, in: capture.root)
            ).count == expectedCount
        default:
            break
        }
        let response = try client.invoke(
            command: "semantic-tree",
            parameters: [
                "pid": launched.processIdentity.pid,
                "outputPath": outputURL.path,
                "scopeIdentifier": condition.element.identifier,
                "maxDepth": 2,
                "maxNodes": 50,
                "maxValueLength": 2_000,
                "timeout": conditionInspectionTimeout,
            ],
            timeout: conditionDriverTimeout
        )
        guard response.status == "passed" else {
            if response.errorCode == .elementNotFound {
                if case .notExists = condition {
                    return true
                }
                return false
            }
            throw scenarioDriverFailure(
                response,
                fallback: "semantic condition inspection failed"
            )
        }
        let capture = try decode(AXTreeCapture.self, from: response.result)
        switch condition {
        case .exists:
            return true
        case .notExists:
            return false
        case .identifierCountEquals:
            return false
        case .distinctIdentifierLabelCountEquals:
            return false
        case let .enabled(_, expected):
            return capture.root.enabled == expected
        case let .focused(_, expected):
            return capture.root.focused == expected
        case let .labelContains(_, expected):
            return capture.root.label?.contains(expected) == true
        case let .valueEquals(_, expected):
            return capture.root.value == expected
        case let .valueContains(_, expected):
            return capture.root.value?.contains(expected) == true
        }
    }

    func ensureApplicationIsAlive() throws {
        guard ProcessIdentity.forPID(launched.processIdentity.pid)
                == launched.processIdentity
        else {
            throw ScenarioRuntimeError.applicationExited(
                launched.processIdentity.pid
            )
        }
    }

    func describe(_ condition: UICondition) -> String {
        switch condition {
        case let .exists(element):
            return "\(element.identifier) exists"
        case let .notExists(element):
            return "\(element.identifier) does not exist"
        case let .identifierCountEquals(element, expectedCount):
            return "\(element.identifier) count == \(expectedCount)"
        case let .distinctIdentifierLabelCountEquals(element, expectedCount):
            return "\(element.identifier) distinct label count == \(expectedCount)"
        case let .enabled(element, expected):
            return "\(element.identifier) enabled == \(expected)"
        case let .focused(element, expected):
            return "\(element.identifier) focused == \(expected)"
        case let .labelContains(element, expected):
            return "\(element.identifier) label contains \(expected)"
        case let .valueEquals(element, expected):
            return "\(element.identifier) value == \(expected)"
        case let .valueContains(element, expected):
            return "\(element.identifier) value contains \(expected)"
        }
    }

    func refreshChildren() {
        observedChildren.formUnion(
            ProcessIdentity.descendants(of: launched.processIdentity.pid)
        )
    }

    func acpPromptCount() -> Int {
        let url = paths.logs.appendingPathComponent("acp-requests.jsonl")
        guard let contents = try? String(contentsOf: url, encoding: .utf8) else {
            return 0
        }
        return contents.components(
            separatedBy: "\"method\":\"session/prompt\""
        ).count - 1
    }

    func countIdentifiers(
        _ identifier: String,
        in root: NormalizedAXNode
    ) -> Int {
        var count = root.identifier == identifier ? 1 : 0
        for child in root.children {
            count += countIdentifiers(identifier, in: child)
        }
        return count
    }

    func identifierLabels(
        _ identifier: String,
        in root: NormalizedAXNode
    ) -> [String] {
        var labels: [String] = []
        if root.identifier == identifier,
           let label = root.label?.trimmingCharacters(in: .whitespacesAndNewlines),
           !label.isEmpty
        {
            labels.append(label)
        }
        for child in root.children {
            labels.append(contentsOf: identifierLabels(identifier, in: child))
        }
        return labels
    }

    func appendFixtureEvent(
        _ event: String,
        fields: [String: Any]
    ) throws {
        let url = paths.logs.appendingPathComponent("fixture-events.jsonl")
        var object = fields
        object["timestamp"] = scenarioISO(Date())
        object["event"] = event
        let data = try JSONSerialization.data(
            withJSONObject: object,
            options: [.sortedKeys, .withoutEscapingSlashes]
        ) + Data([0x0A])
        if !FileManager.default.fileExists(atPath: url.path) {
            try data.write(to: url, options: .atomic)
        } else {
            let handle = try FileHandle(forWritingTo: url)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: data)
        }
    }

    func decode<T: Decodable>(
        _ type: T.Type,
        from object: [String: Any]
    ) throws -> T {
        let data = try JSONSerialization.data(withJSONObject: object)
        return try JSONDecoder().decode(type, from: data)
    }
}
