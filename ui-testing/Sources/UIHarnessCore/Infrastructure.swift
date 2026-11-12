import CryptoKit
import Darwin
import Foundation

public enum AutomationFailureCode: String, Codable, Sendable {
    case accessibilityPermissionMissing = "accessibility_permission_missing"
    case screenRecordingPermissionMissing = "screen_recording_permission_missing"
    case invalidBundle = "invalid_bundle"
    case launchFailed = "launch_failed"
    case processIdentityUnavailable = "process_identity_unavailable"
    case runningApplicationUnavailable = "running_application_unavailable"
    case terminationFailed = "termination_failed"
    case elementNotFound = "element_not_found"
    case unsupportedAction = "unsupported_action"
    case valueRequired = "value_required"
    case accessibilityFailure = "accessibility_failure"
    case semanticCaptureTimedOut = "semantic_capture_timed_out"
    case elementSearchTimedOut = "element_search_timed_out"
    case windowNotFound = "window_not_found"
    case displayNotFound = "display_not_found"
    case missingCapturedImage = "missing_captured_image"
    case cannotWriteCapture = "cannot_write_capture"
    case cannotReadCapture = "cannot_read_capture"
    case occlusionMarkerNotVisible = "occlusion_marker_not_visible"
    case occlusionIsolationFailed = "occlusion_isolation_failed"
}

public final class ActionRecorder: @unchecked Sendable {
    private let url: URL
    private let lock = NSLock()

    public init(url: URL) {
        self.url = url
    }

    public func append(event: String, fields: [String: Any] = [:]) throws {
        lock.lock()
        defer { lock.unlock() }
        var payload = fields
        payload["event"] = event
        payload["timestamp"] = ISO8601DateFormatter().string(from: Date())
        let data = try JSONSerialization.data(
            withJSONObject: payload,
            options: [.sortedKeys, .withoutEscapingSlashes]
        ) + Data([0x0A])
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        let handle = try FileHandle(forWritingTo: url)
        defer { try? handle.close() }
        try handle.seekToEnd()
        try handle.write(contentsOf: data)
    }
}

public enum EvidenceWriter {
    public static func writeJSON(_ value: Any, to url: URL) throws {
        let data = try JSONSerialization.data(
            withJSONObject: value,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        )
        try data.write(to: url, options: .atomic)
    }

    public static func writeEncodable<T: Encodable>(
        _ value: T,
        to url: URL
    ) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(value)
        try data.write(to: url, options: .atomic)
    }

    public static func object<T: Encodable>(_ value: T) throws -> [String: Any] {
        let data = try JSONEncoder().encode(value)
        guard let object = try JSONSerialization.jsonObject(with: data)
                as? [String: Any]
        else {
            throw UIHarnessCoreError.invalidEvidenceObject
        }
        return object
    }

    public static func gitMetadata(worktreeURL: URL) -> [String: Any] {
        let commit = (try? Command.output([
            "/usr/bin/git", "-C", worktreeURL.path, "rev-parse", "HEAD",
        ]))?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "unknown"
        let dirty = (try? Command.output([
            "/usr/bin/git", "-C", worktreeURL.path, "status", "--short",
        ])) ?? ""
        return [
            "sourceCommit": commit,
            "dirtyState": dirty.split(separator: "\n").map(String.init),
        ]
    }
}

public enum Command {
    public static func run(_ arguments: [String]) throws {
        _ = try output(arguments)
    }

    public static func output(
        _ arguments: [String],
        timeout: TimeInterval? = nil
    ) throws -> String {
        guard let executable = arguments.first else {
            throw UIHarnessCoreError.emptyCommand
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = Array(arguments.dropFirst())
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        try process.run()
        let collector = ProcessOutputCollector(pipe: pipe)
        if let timeout {
            let deadline = Date().addingTimeInterval(timeout)
            while process.isRunning, Date() < deadline {
                Thread.sleep(forTimeInterval: 0.05)
            }
            if process.isRunning {
                terminateProcess(process)
                throw UIHarnessCoreError.commandTimedOut(arguments, timeout)
            }
        }
        process.waitUntilExit()
        let output = String(decoding: collector.finish(), as: UTF8.self)
        guard process.terminationStatus == 0 else {
            throw UIHarnessCoreError.commandFailed(
                arguments,
                process.terminationStatus,
                output
            )
        }
        return output
    }
}

public func fileSHA256(_ url: URL) throws -> String {
    let data = try Data(contentsOf: url, options: .mappedIfSafe)
    return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

public enum UIHarnessCoreError: Error, CustomStringConvertible {
    case emptyCommand
    case commandTimedOut([String], TimeInterval)
    case commandFailed([String], Int32, String)
    case invalidEvidenceObject
    case invalidXCTestRun

    public var description: String {
        switch self {
        case .emptyCommand:
            "empty subprocess command"
        case let .commandTimedOut(command, timeout):
            "command \(command.joined(separator: " ")) timed out after \(timeout) seconds"
        case let .commandFailed(command, status, output):
            "command \(command.joined(separator: " ")) exited \(status): \(output)"
        case .invalidEvidenceObject:
            "encoded evidence was not a JSON object"
        case .invalidXCTestRun:
            "XCTest run file does not contain a configurable test target"
        }
    }
}

public final class ProcessOutputCollector: @unchecked Sendable {
    private let handle: FileHandle
    private let completion = DispatchGroup()
    private var data = Data()

    public init(pipe: Pipe) {
        handle = pipe.fileHandleForReading
        completion.enter()
        DispatchQueue.global(qos: .utility).async { [self] in
            data = handle.readDataToEndOfFile()
            completion.leave()
        }
    }

    public func finish() -> Data {
        completion.wait()
        return data
    }
}

public func terminateProcess(_ process: Process) {
    guard process.isRunning else { return }
    process.terminate()
    let deadline = Date().addingTimeInterval(1)
    while process.isRunning, Date() < deadline {
        Thread.sleep(forTimeInterval: 0.02)
    }
    if process.isRunning {
        _ = kill(process.processIdentifier, SIGKILL)
    }
    process.waitUntilExit()
}
