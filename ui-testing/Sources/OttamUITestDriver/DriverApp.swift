import AppKit
import Foundation
import MacAutomationKit
import ScreenCaptureKit

@main
enum OttamUITestDriver {
    @MainActor
    static func main() async {
        NSApplication.shared.setActivationPolicy(.accessory)
        do {
            let arguments = CommandLine.arguments
            guard let requestPath = arguments.value(after: "--request"),
                  let responsePath = arguments.value(after: "--response")
            else {
                throw DriverError.usage
            }
            let request = try DriverRequest.load(from: requestPath)
            guard request.schemaVersion == 1 else {
                throw DriverError.unsupportedSchema(request.schemaVersion)
            }
            let result = try await execute(request)
            try writeResponse(
                [
                    "schemaVersion": 1,
                    "status": "passed",
                    "command": request.command,
                    "result": result,
                ],
                to: responsePath
            )
        } catch {
            let responsePath = CommandLine.arguments.value(after: "--response")
            let classification = classification(for: error)
            let payload: [String: Any] = [
                "schemaVersion": 1,
                "status": classification,
                "errorCode": (error as? AutomationError)?.failureCode.rawValue
                    ?? "driver_error",
                "message": String(describing: error),
                "setupInstructions": classification == "environment_unavailable"
                    ? environmentInstructions(for: error)
                    : NSNull(),
            ]
            if let responsePath {
                try? writeResponse(payload, to: responsePath)
            }
            FileHandle.standardError.write(Data("UI test driver failed: \(error)\n".utf8))
            exit(1)
        }
    }

    @MainActor
    private static func execute(_ request: DriverRequest) async throws -> Any {
        switch request.command {
        case "preflight":
            return try dictionary(AutomationPermissionState())
        case "request-permissions":
            let accessibilityRequestReturned = AutomationPermissionState.requestAccessibility()
            let screenRecordingRequestReturned =
                AutomationPermissionState.requestScreenRecording()
            return [
                "accessibilityRequestReturned": accessibilityRequestReturned,
                "screenRecordingRequestReturned": screenRecordingRequestReturned,
                "state": try dictionary(AutomationPermissionState()),
                "setupInstructions": permissionInstructions(),
            ]
        case "launch":
            let environment = try request.stringDictionary("environment")
            let launched = try await ApplicationLifecycle.launch(
                bundleURL: URL(fileURLWithPath: try request.string("bundleURL")),
                environment: environment,
                arguments: try request.stringArray("arguments", default: []),
                activates: try request.bool("activates", default: false)
            )
            return try dictionary(launched)
        case "terminate":
            let identity = ProcessIdentity(
                pid: try request.pid("pid"),
                startSeconds: try request.uint64("startSeconds"),
                startMicroseconds: try request.uint64("startMicroseconds")
            )
            let terminatedChildren = try await ApplicationLifecycle.terminate(
                identity: identity,
                knownDescendants: try request.processIdentities(
                    "knownDescendants",
                    default: []
                )
            )
            return [
                "pid": identity.pid,
                "processIdentity": identity.stableValue,
                "exited": ProcessIdentity.forPID(identity.pid) != identity,
                "observedChildProcesses": try terminatedChildren.map(dictionary),
                "childrenExited": terminatedChildren.allSatisfy {
                    ProcessIdentity.forPID($0.pid) != $0
                },
            ]
        case "capture":
            let result = try await WindowCapture.capture(
                pid: try request.pid("pid"),
                windowID: request.optionalUInt32("windowID"),
                outputURL: URL(fileURLWithPath: try request.string("outputPath"))
            )
            return try dictionary(result)
        case "capture-window-group":
            let result = try await WindowCapture.captureGroup(
                pid: try request.pid("pid"),
                windowIDs: try request.uint32Array("windowIDs"),
                outputURL: URL(fileURLWithPath: try request.string("outputPath"))
            )
            return try dictionary(result)
        case "occlusion-proof":
            let result = try await WindowCapture.proveOcclusionIsolation(
                pid: try request.pid("pid"),
                windowID: request.optionalUInt32("windowID"),
                baselineURL: URL(fileURLWithPath: try request.string("baselineOutputPath")),
                displayURL: URL(fileURLWithPath: try request.string("displayOutputPath")),
                isolatedURL: URL(fileURLWithPath: try request.string("isolatedOutputPath"))
            )
            return try dictionary(result)
        case "semantic-tree":
            let outputPath = try request.string("outputPath")
            let capture = try AXAutomation.captureTree(
                pid: try request.pid("pid"),
                scopeIdentifier: request.optionalString("scopeIdentifier"),
                limits: AXCaptureLimits(
                    maxDepth: try request.int("maxDepth", default: 12),
                    maxNodes: try request.int("maxNodes", default: 500),
                    maxValueLength: try request.int("maxValueLength", default: 2_000),
                    timeout: try request.double("timeout", default: 3)
                )
            )
            let data = try JSONEncoder.pretty.encode(capture)
            try data.write(
                to: URL(fileURLWithPath: outputPath),
                options: .atomic
            )
            return try dictionary(capture)
        case "action":
            try AXAutomation.perform(
                action: try request.string("action"),
                identifier: try request.string("identifier"),
                pid: try request.pid("pid"),
                value: request.optionalString("value"),
                timeout: try request.double("timeout", default: 3)
            )
            return [
                "action": try request.string("action"),
                "identifier": try request.string("identifier"),
                "performed": true,
            ]
        case "select-list-row":
            let name = try request.string("name")
            try AXAutomation.selectListRow(
                named: name,
                waitForText: try request.string("waitForText"),
                pid: try request.pid("pid"),
                timeout: try request.double("timeout", default: 3)
            )
            return ["name": name, "selected": true]
        case "press-control":
            let name = try request.string("name")
            try AXAutomation.pressControl(
                named: name,
                waitForText: try request.string("waitForText"),
                pid: try request.pid("pid"),
                timeout: try request.double("timeout", default: 3)
            )
            return ["name": name, "performed": true]
        case "overlay":
            let overlay = VirtualCursorOverlay(
                point: CGPoint(
                    x: try request.double("x"),
                    y: try request.double("y")
                )
            )
            overlay.show()
            try await Task.sleep(
                for: .seconds(try request.double("duration", default: 1))
            )
            let ignored = overlay.ignoresMouseEvents
            overlay.close()
            return [
                "ignoredMouseEvents": ignored,
                "visibleIntervalEnded": true,
            ]
        default:
            throw DriverError.unsupportedCommand(request.command)
        }
    }

    private static func dictionary<T: Encodable>(_ value: T) throws -> [String: Any] {
        let data = try JSONEncoder().encode(value)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw DriverError.invalidResponse
        }
        return object
    }

    private static func writeResponse(_ value: [String: Any], to path: String) throws {
        let data = try JSONSerialization.data(
            withJSONObject: value,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        )
        try data.write(to: URL(fileURLWithPath: path), options: .atomic)
    }

    private static func permissionInstructions() -> String {
        """
        Enable Ottam UI Test Driver in System Settings > Privacy & Security \
        > Accessibility and Screen & System Audio Recording, then relaunch the \
        stable driver and run scripts/ui-test.sh preflight.
        """
    }

    private static func classification(for error: Error) -> String {
        switch error {
        case AutomationError.accessibilityPermissionMissing,
             AutomationError.screenRecordingPermissionMissing:
            return "environment_unavailable"
        case AutomationError.semanticCaptureTimedOut,
             AutomationError.elementSearchTimedOut:
            return "timed_out"
        case AutomationError.processIdentityUnavailable,
             AutomationError.runningApplicationUnavailable:
            return "app_crashed"
        case AutomationError.terminationFailed:
            return "cleanup_failed"
        default:
            let nsError = error as NSError
            if nsError.domain == SCStreamErrorDomain,
               [
                   -3801, -3802, -3803, -3804, -3805, -3806,
                   -3811, -3813, -3814, -3815, -3817, -3818,
                   -3820, -3821,
               ].contains(nsError.code)
            {
                return "environment_unavailable"
            }
            return "failed_assertion"
        }
    }

    private static func environmentInstructions(for error: Error) -> String {
        let nsError = error as NSError
        if nsError.domain == SCStreamErrorDomain {
            return """
            Ensure the macOS login session is logged in and unlocked with an \
            available display, then retry. If capture remains unavailable on \
            hosted CI, use the documented self-hosted capture profile.
            """
        }
        return permissionInstructions()
    }
}

private struct DriverRequest {
    let schemaVersion: Int
    let command: String
    let parameters: [String: Any]

    static func load(from path: String) throws -> DriverRequest {
        let data = try Data(contentsOf: URL(fileURLWithPath: path))
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let schemaVersion = object["schemaVersion"] as? Int,
              let command = object["command"] as? String,
              let parameters = object["parameters"] as? [String: Any]
        else {
            throw DriverError.invalidRequest
        }
        return DriverRequest(
            schemaVersion: schemaVersion,
            command: command,
            parameters: parameters
        )
    }

    func string(_ key: String) throws -> String {
        guard let value = parameters[key] as? String, !value.isEmpty else {
            throw DriverError.missing(key)
        }
        return value
    }

    func optionalString(_ key: String) -> String? {
        parameters[key] as? String
    }

    func bool(_ key: String, default defaultValue: Bool) throws -> Bool {
        guard let value = parameters[key] else { return defaultValue }
        guard let boolean = value as? Bool else {
            throw DriverError.invalidValue(key)
        }
        return boolean
    }

    func int(_ key: String, default defaultValue: Int) throws -> Int {
        guard let value = parameters[key] else { return defaultValue }
        guard let number = value as? NSNumber else {
            throw DriverError.invalidValue(key)
        }
        return number.intValue
    }

    func double(_ key: String, default defaultValue: Double? = nil) throws -> Double {
        guard let value = parameters[key] else {
            if let defaultValue { return defaultValue }
            throw DriverError.missing(key)
        }
        guard let number = value as? NSNumber else {
            throw DriverError.invalidValue(key)
        }
        return number.doubleValue
    }

    func uint64(_ key: String) throws -> UInt64 {
        guard let number = parameters[key] as? NSNumber else {
            throw DriverError.invalidValue(key)
        }
        return number.uint64Value
    }

    func pid(_ key: String) throws -> pid_t {
        guard let number = parameters[key] as? NSNumber, number.int32Value > 1 else {
            throw DriverError.invalidValue(key)
        }
        return number.int32Value
    }

    func optionalUInt32(_ key: String) -> UInt32? {
        (parameters[key] as? NSNumber)?.uint32Value
    }

    func uint32Array(_ key: String) throws -> [UInt32] {
        guard let values = parameters[key] as? [Any] else {
            throw DriverError.invalidValue(key)
        }
        let result = values.compactMap { ($0 as? NSNumber)?.uint32Value }
        guard result.count == values.count else {
            throw DriverError.invalidValue(key)
        }
        return result
    }

    func stringDictionary(_ key: String) throws -> [String: String] {
        guard let dictionary = parameters[key] as? [String: String] else {
            throw DriverError.invalidValue(key)
        }
        return dictionary
    }

    func stringArray(_ key: String, default defaultValue: [String]) throws -> [String] {
        guard let value = parameters[key] else { return defaultValue }
        guard let array = value as? [String] else {
            throw DriverError.invalidValue(key)
        }
        return array
    }

    func processIdentities(
        _ key: String,
        default defaultValue: [ProcessIdentity]
    ) throws -> [ProcessIdentity] {
        guard let value = parameters[key] else { return defaultValue }
        guard JSONSerialization.isValidJSONObject(value) else {
            throw DriverError.invalidValue(key)
        }
        let data = try JSONSerialization.data(withJSONObject: value)
        do {
            return try JSONDecoder().decode([ProcessIdentity].self, from: data)
        } catch {
            throw DriverError.invalidValue(key)
        }
    }
}

private extension Array where Element == String {
    func value(after flag: String) -> String? {
        guard let index = firstIndex(of: flag), indices.contains(index + 1) else {
            return nil
        }
        return self[index + 1]
    }
}

private extension JSONEncoder {
    static var pretty: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return encoder
    }
}

private enum DriverError: Error, CustomStringConvertible {
    case usage
    case invalidRequest
    case unsupportedSchema(Int)
    case unsupportedCommand(String)
    case missing(String)
    case invalidValue(String)
    case invalidResponse

    var description: String {
        switch self {
        case .usage:
            return "usage: OttamUITestDriver --request PATH --response PATH"
        case .invalidRequest:
            return "invalid driver request"
        case let .unsupportedSchema(version):
            return "unsupported driver schema \(version)"
        case let .unsupportedCommand(command):
            return "unsupported driver command \(command)"
        case let .missing(key):
            return "missing request parameter \(key)"
        case let .invalidValue(key):
            return "invalid request parameter \(key)"
        case .invalidResponse:
            return "driver could not encode its response"
        }
    }
}
