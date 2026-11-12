import Darwin
import Foundation
import ScenarioKit

enum ScenarioRuntimeError: Error, CustomStringConvertible {
    case waitTimedOut(String)
    case applicationExited(pid_t)
    case environmentUnavailable(String)
    case canonicalChanged
    case foregroundChanged(String)
    case cleanupFailed(String)
    case classified(RunClassification, String)

    var description: String {
        switch self {
        case let .waitTimedOut(condition):
            "timed out waiting for \(condition)"
        case let .applicationExited(pid):
            "application pid \(pid) exited before readiness"
        case let .environmentUnavailable(message):
            "environment_unavailable: \(message)"
        case .canonicalChanged:
            "canonical Ottam process identity changed during isolated validation"
        case let .foregroundChanged(message):
            message
        case let .cleanupFailed(message):
            "cleanup failed: \(message)"
        case let .classified(classification, message):
            "\(classification.rawValue): \(message)"
        }
    }
}

func scenarioDriverFailure(
    _ response: DriverResponse,
    fallback: String
) -> Error {
    var message = response.message ?? fallback
    if response.status == RunClassification.environmentUnavailable.rawValue {
        if let setup = response.raw["setupInstructions"] as? String {
            message += " " + setup
        }
    }
    if let classification = RunClassification(rawValue: response.status),
       classification != .failedAssertion
    {
        return ScenarioRuntimeError.classified(classification, message)
    }
    return UIScenarioFailure.assertion(message)
}

func scenarioInterval(
    _ tier: String,
    _ name: String,
    _ started: Date,
    _ ended: Date
) -> [String: Any] {
    [
        "tier": tier,
        "name": name,
        "startedAt": scenarioISO(started),
        "endedAt": scenarioISO(ended),
        "durationMilliseconds": ended.timeIntervalSince(started) * 1_000,
    ]
}

func scenarioISO(_ date: Date) -> String {
    ISO8601DateFormatter().string(from: date)
}

func scenarioRelative(_ url: URL, to root: URL) -> String {
    let rootPath = root.standardizedFileURL.path + "/"
    let path = url.standardizedFileURL.path
    return path.hasPrefix(rootPath) ? String(path.dropFirst(rootPath.count)) : path
}
