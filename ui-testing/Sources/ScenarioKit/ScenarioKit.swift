import OttamAccessibility
import CryptoKit
import Darwin
import Foundation
import MacAutomationKit
@_exported import UIHarnessCore

public struct ScenarioDescriptor: Codable, Equatable, Sendable {
    public let id: String
    public let description: String
    public let tags: [String]
    public let requirements: [String]

    public init(
        id: String,
        description: String,
        tags: [String],
        requirements: [String]
    ) {
        self.id = id
        self.description = description
        self.tags = tags
        self.requirements = requirements
    }
}

public struct IsolationIdentity: Codable, Sendable {
    public let worktreePath: String
    public let worktreeID: String
    public let runID: String
    public let bundleIdentifier: String
    public let applicationName: String
    public let executableName: String
    public let displayName: String

    public init(
        worktreeURL: URL,
        runID: String? = nil,
        displayName: String = "Ottam Test"
    ) throws {
        // Command.output merges stderr into stdout. Never allow a shell
        // startup diagnostic (or another malformed value) into bundle metadata.
        let worktreePrefix = "Ottam Test · "
        let hasWorktreeLabel = displayName.hasPrefix(worktreePrefix)
            && !String(displayName.dropFirst(worktreePrefix.count))
                .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let containsControlOrNewline = displayName.unicodeScalars.contains {
            switch $0.properties.generalCategory {
            case .control, .lineSeparator, .paragraphSeparator: true
            default: false
            }
        }
        guard displayName == "Ottam Test" || hasWorktreeLabel,
              !containsControlOrNewline
        else {
            throw ScenarioKitError.invalidDisplayName(displayName)
        }
        let canonical = worktreeURL.resolvingSymlinksInPath().standardizedFileURL.path
        let digest = SHA256.hash(data: Data(canonical.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
        let derivedWorktreeID = "w" + digest.prefix(12)
        let derivedRunID = runID ?? (
            "r" + UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        )
        guard derivedRunID.range(of: #"^r[0-9a-f]{32}$"#, options: .regularExpression) != nil
        else {
            throw ScenarioKitError.invalidRunID(derivedRunID)
        }
        worktreePath = canonical
        worktreeID = String(derivedWorktreeID)
        self.runID = derivedRunID
        bundleIdentifier =
            "dev.ottam.Ottam.ui-test.\(derivedWorktreeID).\(derivedRunID)"
        applicationName = "Ottam UI \(derivedWorktreeID) \(derivedRunID)"
        executableName = "OttamUITest-\(derivedWorktreeID)-\(derivedRunID)"
        self.displayName = displayName
    }
}

public struct RunPaths: Codable, Sendable {
    public let root: URL
    public let home: URL
    public let logs: URL
    public let temporary: URL
    public let control: URL
    public let screenshots: URL
    public let accessibility: URL
    public let diagnostics: URL
    public let stagedApplications: URL
    public let derivedData: URL
    public let actions: URL

    public enum TransientDirectory: String, CaseIterable, Sendable {
        case temporary = "tmp"
        case diagnostics
        case stagedApplications = "staged-app"
        case derivedData = "DerivedData"
    }

    public init(
        worktreeURL: URL,
        identity: IsolationIdentity,
        evidenceRoot: URL? = nil
    ) throws {
        root = evidenceRoot ?? worktreeURL
            .appendingPathComponent(".build/ui-tests")
            .appendingPathComponent("current")
        home = root.appendingPathComponent("home")
        logs = root.appendingPathComponent("logs")
        temporary = root.appendingPathComponent("tmp")
        control = root.appendingPathComponent("control")
        screenshots = root.appendingPathComponent("screenshots")
        accessibility = root.appendingPathComponent("accessibility")
        diagnostics = root.appendingPathComponent("diagnostics")
        stagedApplications = root.appendingPathComponent("staged-app")
        derivedData = root.appendingPathComponent("DerivedData")
        actions = logs.appendingPathComponent("actions.jsonl")
        for directory in [
            root, home, logs, temporary, control, screenshots, accessibility,
            diagnostics, stagedApplications, derivedData,
        ] {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700]
            )
        }
    }

    public func transientDirectoryExists(_ directory: TransientDirectory) -> Bool {
        FileManager.default.fileExists(atPath: transientURL(for: directory).path)
    }

    @discardableResult
    public func removeTransientDirectory(
        _ directory: TransientDirectory,
        onlyIfEmpty: Bool = false
    ) throws -> Bool {
        let url = transientURL(for: directory)
        guard FileManager.default.fileExists(atPath: url.path) else {
            return true
        }
        if onlyIfEmpty {
            let contents = try FileManager.default.contentsOfDirectory(
                at: url,
                includingPropertiesForKeys: nil
            )
            guard contents.isEmpty else {
                return false
            }
        }
        try FileManager.default.removeItem(at: url)
        return !FileManager.default.fileExists(atPath: url.path)
    }

    private func transientURL(for directory: TransientDirectory) -> URL {
        switch directory {
        case .temporary:
            temporary
        case .diagnostics:
            diagnostics
        case .stagedApplications:
            stagedApplications
        case .derivedData:
            derivedData
        }
    }
}

public struct StagedApplication: Codable, Sendable {
    public let bundleURL: URL
    public let bundleIdentifier: String
    public let executableURL: URL
    public let executableSHA256: String

    public static func stage(
        sourceBundleURL: URL,
        identity: IsolationIdentity,
        paths: RunPaths
    ) throws -> StagedApplication {
        guard sourceBundleURL.standardizedFileURL.path
            != paths.stagedApplications.standardizedFileURL.path
        else {
            throw ScenarioKitError.invalidStageSource(sourceBundleURL.path)
        }
        // Name the bundle the caller actually asked for. Without this, a
        // missing or empty --source-app surfaces as a bare Cocoa error 260
        // from copyItem, which reads as a problem with the staged path.
        var sourceIsDirectory: ObjCBool = false
        guard FileManager.default.fileExists(
            atPath: sourceBundleURL.path,
            isDirectory: &sourceIsDirectory
        ), sourceIsDirectory.boolValue else {
            throw ScenarioKitError.invalidStageSource(sourceBundleURL.path)
        }
        let destination = paths.stagedApplications
            .appendingPathComponent(identity.applicationName)
            .appendingPathExtension("app")
        guard !FileManager.default.fileExists(atPath: destination.path) else {
            throw ScenarioKitError.stageAlreadyExists(destination.path)
        }
        try FileManager.default.copyItem(at: sourceBundleURL, to: destination)

        let infoURL = destination.appendingPathComponent("Contents/Info.plist")
        var format = PropertyListSerialization.PropertyListFormat.xml
        let infoData = try Data(contentsOf: infoURL)
        guard var info = try PropertyListSerialization.propertyList(
            from: infoData,
            options: [],
            format: &format
        ) as? [String: Any],
        let sourceExecutableName = info["CFBundleExecutable"] as? String
        else {
            throw ScenarioKitError.invalidStageSource(sourceBundleURL.path)
        }
        let macOSDirectory = destination.appendingPathComponent("Contents/MacOS")
        let sourceExecutable = macOSDirectory.appendingPathComponent(sourceExecutableName)
        let executable = macOSDirectory.appendingPathComponent(identity.executableName)
        try FileManager.default.moveItem(at: sourceExecutable, to: executable)
        info["CFBundleIdentifier"] = identity.bundleIdentifier
        info["CFBundleName"] = identity.displayName
        info["CFBundleDisplayName"] = identity.displayName
        info["CFBundleExecutable"] = identity.executableName
        let updatedInfo = try PropertyListSerialization.data(
            fromPropertyList: info,
            format: .xml,
            options: 0
        )
        try updatedInfo.write(to: infoURL, options: .atomic)
        try Command.run([
            "/usr/bin/codesign", "--force", "--deep", "--sign", "-", destination.path,
        ])
        try Command.run([
            "/usr/bin/codesign", "--verify", "--deep", "--strict", destination.path,
        ])
        return StagedApplication(
            bundleURL: destination,
            bundleIdentifier: identity.bundleIdentifier,
            executableURL: executable,
            executableSHA256: try fileSHA256(executable)
        )
    }

    public func removeAfterVerifiedCleanup(paths: RunPaths) throws {
        let parentPath = bundleURL.deletingLastPathComponent().standardizedFileURL.path
        let stagedRootPath = paths.stagedApplications.standardizedFileURL.path
        let runRootPrefix = paths.root.standardizedFileURL.path + "/"
        guard parentPath == stagedRootPath,
              stagedRootPath.hasPrefix(runRootPrefix),
              bundleURL.pathExtension == "app"
        else {
            throw ScenarioKitError.invalidStagedCleanupPath(bundleURL.path)
        }
        try FileManager.default.removeItem(at: bundleURL)
    }
}

public enum DriverLaunchTopology: String, Codable, Sendable {
    case launchServices
    case direct
}

public struct DriverResponse {
    public let status: String
    public let command: String?
    public let errorCode: AutomationFailureCode?
    public let message: String?
    public let result: [String: Any]
    public let raw: [String: Any]
}

public final class DriverClient: @unchecked Sendable {
    private let driverBundleURL: URL
    private let requestsDirectory: URL
    private let topology: DriverLaunchTopology
    private let actionRecorder: ActionRecorder

    public init(
        driverBundleURL: URL,
        requestsDirectory: URL,
        topology: DriverLaunchTopology,
        actionRecorder: ActionRecorder
    ) {
        self.driverBundleURL = driverBundleURL
        self.requestsDirectory = requestsDirectory
        self.topology = topology
        self.actionRecorder = actionRecorder
    }

    public func invoke(
        command: String,
        parameters: [String: Any] = [:],
        timeout: TimeInterval = 30
    ) throws -> DriverResponse {
        let requestID = UUID().uuidString.lowercased()
        let requestURL = requestsDirectory.appendingPathComponent("\(requestID).request.json")
        let responseURL = requestsDirectory.appendingPathComponent("\(requestID).response.json")
        let payload: [String: Any] = [
            "schemaVersion": 1,
            "command": command,
            "parameters": parameters,
        ]
        let requestData = try JSONSerialization.data(
            withJSONObject: payload,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        )
        try requestData.write(to: requestURL, options: .atomic)
        let started = Date()
        try actionRecorder.append(
            event: "driver_request",
            fields: ["command": command, "requestID": requestID]
        )

        let process = Process()
        switch topology {
        case .launchServices:
            process.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            process.arguments = [
                "-n", "-W", driverBundleURL.path, "--args",
                "--request", requestURL.path,
                "--response", responseURL.path,
            ]
        case .direct:
            let executableName = try bundleExecutableName(driverBundleURL)
            process.executableURL = driverBundleURL
                .appendingPathComponent("Contents/MacOS")
                .appendingPathComponent(executableName)
            process.arguments = [
                "--request", requestURL.path,
                "--response", responseURL.path,
            ]
        }
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        try process.run()
        let collector = ProcessOutputCollector(pipe: output)
        let deadline = Date().addingTimeInterval(timeout)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        if process.isRunning {
            terminateProcess(process)
            throw ScenarioKitError.driverTimedOut(command)
        }
        process.waitUntilExit()
        let driverOutput = String(
            decoding: collector.finish(),
            as: UTF8.self
        )
        guard FileManager.default.fileExists(atPath: responseURL.path) else {
            throw ScenarioKitError.driverFailed(command, driverOutput)
        }
        let responseData = try Data(contentsOf: responseURL)
        guard let raw = try JSONSerialization.jsonObject(with: responseData)
            as? [String: Any],
            let status = raw["status"] as? String
        else {
            throw ScenarioKitError.invalidDriverResponse(command)
        }
        let result = raw["result"] as? [String: Any] ?? [:]
        try actionRecorder.append(
            event: "driver_response",
            fields: [
                "command": command,
                "requestID": requestID,
                "status": status,
                "durationMilliseconds": Date().timeIntervalSince(started) * 1_000,
            ]
        )
        return DriverResponse(
            status: status,
            command: raw["command"] as? String,
            errorCode: (raw["errorCode"] as? String)
                .flatMap(AutomationFailureCode.init(rawValue:)),
            message: raw["message"] as? String,
            result: result,
            raw: raw
        )
    }
}

private func bundleExecutableName(_ bundleURL: URL) throws -> String {
    let data = try Data(
        contentsOf: bundleURL.appendingPathComponent("Contents/Info.plist")
    )
    var format = PropertyListSerialization.PropertyListFormat.xml
    guard let info = try PropertyListSerialization.propertyList(
        from: data,
        options: [],
        format: &format
    ) as? [String: Any],
    let executableName = info["CFBundleExecutable"] as? String
    else {
        throw ScenarioKitError.invalidStageSource(bundleURL.path)
    }
    return executableName
}

public enum ScenarioKitError: Error, CustomStringConvertible {
    case invalidDisplayName(String)
    case invalidRunID(String)
    case invalidStageSource(String)
    case stageAlreadyExists(String)
    case invalidStagedCleanupPath(String)
    case driverTimedOut(String)
    case driverFailed(String, String)
    case invalidDriverResponse(String)

    public var description: String {
        switch self {
        case let .invalidDisplayName(name):
            return "invalid UI test display name: \(String(reflecting: name))"
        case let .invalidRunID(runID):
            return "invalid run identifier: \(runID)"
        case let .invalidStageSource(path):
            return "invalid staged application source: \(path)"
        case let .stageAlreadyExists(path):
            return "isolated stage already exists: \(path)"
        case let .invalidStagedCleanupPath(path):
            return "refusing staged application cleanup outside the run root: \(path)"
        case let .driverTimedOut(command):
            return "driver command timed out: \(command)"
        case let .driverFailed(command, output):
            return "driver command \(command) produced no response: \(output)"
        case let .invalidDriverResponse(command):
            return "driver command produced an invalid response: \(command)"
        }
    }
}
