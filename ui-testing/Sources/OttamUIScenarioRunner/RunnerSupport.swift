import AppKit
import OttamAccessibility
import OttamUIScenarios
import Foundation
import MacAutomationKit
import ScenarioKit
struct RunnerArguments {
    let values: [String]

    init(_ values: [String]) {
        self.values = values
    }

    var command: String {
        values.indices.contains(1) ? values[1] : ""
    }

    var scenarioID: String? {
        command == "run" && values.indices.contains(2) ? values[2] : nil
    }

    var root: String {
        value(after: "--root") ?? FileManager.default.currentDirectoryPath
    }

    var driverApp: String {
        value(after: "--driver-app") ?? ""
    }

    var sourceApp: String {
        value(after: "--source-app") ?? ""
    }

    var coordinator: String {
        value(after: "--coordinator") ?? ""
    }

    var driverTopology: DriverLaunchTopology {
        DriverLaunchTopology(
            rawValue: value(after: "--driver-topology") ?? "launchServices"
        ) ?? .launchServices
    }

    var isHosted: Bool {
        ProcessInfo.processInfo.environment["GITHUB_ACTIONS"] == "true"
    }

    func value(after flag: String) -> String? {
        guard let index = values.firstIndex(of: flag),
              values.indices.contains(index + 1)
        else {
            return nil
        }
        return values[index + 1]
    }
}

func identityParameters(
    _ identity: ProcessIdentity,
    knownDescendants: Set<ProcessIdentity> = []
) -> [String: Any] {
    [
        "pid": identity.pid,
        "startSeconds": identity.startSeconds,
        "startMicroseconds": identity.startMicroseconds,
        "knownDescendants": knownDescendants
            .sorted { $0.pid < $1.pid }
            .map {
                [
                    "pid": $0.pid,
                    "startSeconds": $0.startSeconds,
                    "startMicroseconds": $0.startMicroseconds,
                ]
            },
    ]
}

func publicCommandArguments() -> [String] {
    guard let raw = ProcessInfo.processInfo.environment[
        "OTTAM_UI_TEST_PUBLIC_COMMAND_JSON"
    ],
    let data = raw.data(using: .utf8),
    let arguments = try? JSONDecoder().decode([String].self, from: data),
    !arguments.isEmpty
    else {
        return CommandLine.arguments
    }
    return arguments
}

func interval(
    _ tier: String,
    _ name: String,
    _ started: Date,
    _ ended: Date
) -> [String: Any] {
    [
        "tier": tier,
        "name": name,
        "startedAt": iso(started),
        "endedAt": iso(ended),
        "durationMilliseconds": ended.timeIntervalSince(started) * 1_000,
    ]
}

func iso(_ date: Date) -> String {
    ISO8601DateFormatter().string(from: date)
}

func canonicalApplicationIdentities() -> [[String: String]] {
    NSRunningApplication.runningApplications(withBundleIdentifier: "dev.ottam.Ottam")
        .compactMap { application -> [String: String]? in
            guard let identity = ProcessIdentity.forPID(application.processIdentifier) else {
                return nil
            }
            return [
                "pid": String(identity.pid),
                "processIdentity": identity.stableValue,
                "bundleURL": application.bundleURL?.path ?? "unavailable",
            ]
        }
        .sorted { ($0["pid"] ?? "") < ($1["pid"] ?? "") }
}

struct ForegroundIdentity: Equatable {
    let pid: Int32
    let bundleIdentifier: String

    var dictionary: [String: Any] {
        ["pid": pid, "bundleIdentifier": bundleIdentifier]
    }
}

func frontmostApplication() -> ForegroundIdentity? {
    guard let application = NSWorkspace.shared.frontmostApplication else { return nil }
    return ForegroundIdentity(
        pid: application.processIdentifier,
        bundleIdentifier: application.bundleIdentifier ?? "unknown"
    )
}

func runFailure(for error: Error, phase: String) -> RunFailure {
    let classification: RunClassification
    if let scenarioFailure = error as? UIScenarioFailure {
        classification = scenarioFailure.classification
    } else if case let ScenarioRuntimeError.classified(driverClassification, _) = error {
        classification = driverClassification
    } else {
        switch error {
        case ScenarioRuntimeError.environmentUnavailable,
             RunnerError.environmentUnavailable:
            classification = .environmentUnavailable
        case ScenarioRuntimeError.waitTimedOut,
             RunnerError.waitTimedOut,
             ScenarioKitError.driverTimedOut,
             UIHarnessCoreError.commandTimedOut:
            classification = .timedOut
        case ScenarioRuntimeError.applicationExited,
             RunnerError.applicationExited:
            classification = .appCrashed
        case ScenarioRuntimeError.cleanupFailed:
            classification = .cleanupFailed
        case ScenarioRuntimeError.canonicalChanged,
             RunnerError.canonicalChanged:
            classification = .isolationFailed
        case ScenarioKitError.invalidDisplayName,
             ScenarioKitError.invalidRunID,
             ScenarioKitError.invalidStageSource,
             ScenarioKitError.stageAlreadyExists,
             ScenarioKitError.invalidStagedCleanupPath:
            classification = .isolationFailed
        case UIHarnessCoreError.commandFailed:
            classification = .buildFailed
        case CoordinatorLeaseError.timedOut:
            classification = .queueTimedOut
        case is CoordinatorLeaseError:
            classification = .isolationFailed
        default:
            classification = .failedAssertion
        }
    }
    return RunFailure(
        classification: classification,
        phase: phase,
        message: String(describing: error)
    )
}

func failureEvidence(_ failure: RunFailure?) -> Any {
    guard let failure else { return NSNull() }
    return failureEvidence(failure)
}

func failureEvidence(_ failure: RunFailure) -> [String: Any] {
    [
        "classification": failure.classification.rawValue,
        "phase": failure.phase,
        "message": failure.message,
    ]
}

func recordEvidenceFailure(
    _ failures: inout RunFailureState,
    phase: String,
    error: Error
) {
    let failure = RunFailure(
        classification: .evidenceFailed,
        phase: phase,
        message: String(describing: error)
    )
    if failures.primaryFailure == nil {
        failures.recordPrimary(failure)
    } else {
        failures.recordSecondary(failure)
    }
}

enum RunnerError: Error, CustomStringConvertible {
    case usage
    case unknownScenario(String)
    case environmentUnavailable(String)
    case driver(String)
    case waitTimedOut(String)
    case applicationExited(pid_t)
    case foregroundChanged(ForegroundIdentity, ForegroundIdentity?)
    case canonicalChanged
    case evidenceCompactionFailed(String)

    var description: String {
        switch self {
        case .usage:
            return """
            usage: OttamUIScenarioRunner \
            list|preflight|request-permissions|smoke|run <id> [options]
            """
        case let .unknownScenario(id):
            return "unknown scenario: \(id)"
        case let .environmentUnavailable(message):
            return "environment_unavailable: \(message)"
        case let .driver(message):
            return "driver failure: \(message)"
        case let .waitTimedOut(condition):
            return "timed out waiting for \(condition)"
        case let .applicationExited(pid):
            return "application pid \(pid) exited before readiness"
        case let .foregroundChanged(before, after):
            return "quiet validation changed foreground application from \(before) to \(String(describing: after))"
        case .canonicalChanged:
            return "canonical Ottam process identity changed during isolated validation"
        case let .evidenceCompactionFailed(directory):
            return "could not remove transient run directory \(directory)"
        }
    }
}
