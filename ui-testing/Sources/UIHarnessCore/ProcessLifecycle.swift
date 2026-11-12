import AppKit
import Darwin
import Foundation

public struct HarnessProcessIdentity: Codable, Equatable, Hashable, Sendable {
    public let pid: Int32
    public let startSeconds: UInt64
    public let startMicroseconds: UInt64

    public var stableValue: String {
        "\(pid):\(startSeconds):\(startMicroseconds)"
    }

    public init(pid: Int32, startSeconds: UInt64, startMicroseconds: UInt64) {
        self.pid = pid
        self.startSeconds = startSeconds
        self.startMicroseconds = startMicroseconds
    }

    public static func current() -> HarnessProcessIdentity? {
        forPID(ProcessInfo.processInfo.processIdentifier)
    }

    public static func forPID(_ pid: pid_t) -> HarnessProcessIdentity? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else {
            return nil
        }
        return HarnessProcessIdentity(
            pid: pid,
            startSeconds: info.pbi_start_tvsec,
            startMicroseconds: UInt64(info.pbi_start_tvusec)
        )
    }

    public static func descendants(of pid: pid_t) -> [HarnessProcessIdentity] {
        var pending = directChildren(of: pid)
        var observed: Set<HarnessProcessIdentity> = []
        while let child = pending.popLast() {
            guard observed.insert(child).inserted else { continue }
            pending.append(contentsOf: directChildren(of: child.pid))
        }
        return observed.sorted { lhs, rhs in
            lhs.pid == rhs.pid
                ? lhs.startMicroseconds < rhs.startMicroseconds
                : lhs.pid < rhs.pid
        }
    }

    public static func running(executableURL: URL) -> [HarnessProcessIdentity] {
        let expected = executableURL
            .resolvingSymlinksInPath()
            .standardizedFileURL
            .path
        let capacity = max(32, Int(proc_listallpids(nil, 0)) + 32)
        var pids = [pid_t](repeating: 0, count: capacity)
        let count = proc_listallpids(
            &pids,
            Int32(pids.count * MemoryLayout<pid_t>.size)
        )
        guard count > 0 else { return [] }
        return pids.prefix(Int(count)).compactMap { pid in
            guard let identity = HarnessProcessIdentity.forPID(pid),
                  let executablePath = identity.executablePath
            else {
                return nil
            }
            let observed = URL(fileURLWithPath: executablePath)
                .resolvingSymlinksInPath()
                .standardizedFileURL
                .path
            return observed == expected ? identity : nil
        }
    }

    public var executablePath: String? {
        var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        return String(cString: buffer)
    }

    private static func directChildren(of pid: pid_t) -> [HarnessProcessIdentity] {
        var capacity = 32
        while capacity <= 4_096 {
            var values = [pid_t](repeating: 0, count: capacity)
            let count = proc_listchildpids(
                pid,
                &values,
                Int32(values.count * MemoryLayout<pid_t>.size)
            )
            guard count >= 0 else { return [] }
            if count < capacity {
                return values
                    .prefix(Int(count))
                    .compactMap(HarnessProcessIdentity.forPID)
            }
            capacity *= 2
        }
        return []
    }
}

public struct HarnessLaunchedApplication: Codable, Sendable {
    public let bundleURL: String
    public let bundleIdentifier: String
    public let executablePath: String
    public let executableSHA256: String
    public let processIdentity: HarnessProcessIdentity
    public let childProcesses: [HarnessProcessIdentity]

    public init(
        bundleURL: String,
        bundleIdentifier: String,
        executablePath: String,
        executableSHA256: String,
        processIdentity: HarnessProcessIdentity,
        childProcesses: [HarnessProcessIdentity]
    ) {
        self.bundleURL = bundleURL
        self.bundleIdentifier = bundleIdentifier
        self.executablePath = executablePath
        self.executableSHA256 = executableSHA256
        self.processIdentity = processIdentity
        self.childProcesses = childProcesses
    }
}

public enum HarnessApplicationLifecycle {
    @MainActor
    public static func launch(
        bundleURL: URL,
        environment: [String: String],
        arguments: [String] = [],
        activates: Bool
    ) async throws -> HarnessLaunchedApplication {
        let infoURL = bundleURL.appendingPathComponent("Contents/Info.plist")
        let info = NSDictionary(contentsOf: infoURL) as? [String: Any]
        guard let bundleIdentifier = info?["CFBundleIdentifier"] as? String,
              let executableName = info?["CFBundleExecutable"] as? String
        else {
            throw HarnessAutomationError.invalidBundle(bundleURL.path)
        }
        let executableURL = bundleURL
            .appendingPathComponent("Contents/MacOS")
            .appendingPathComponent(executableName)
        let executableHash = try fileSHA256(executableURL)

        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = activates
        configuration.createsNewApplicationInstance = true
        configuration.environment = environment
        configuration.arguments = arguments
        let application: NSRunningApplication = try await
            withCheckedThrowingContinuation { continuation in
                NSWorkspace.shared.openApplication(
                    at: bundleURL,
                    configuration: configuration
                ) { application, error in
                    if let application {
                        continuation.resume(returning: application)
                    } else {
                        continuation.resume(
                            throwing: error
                                ?? HarnessAutomationError.launchFailed(bundleURL.path)
                        )
                    }
                }
            }
        guard let identity = HarnessProcessIdentity.forPID(
            application.processIdentifier
        ) else {
            throw HarnessAutomationError.processIdentityUnavailable(
                application.processIdentifier
            )
        }
        return HarnessLaunchedApplication(
            bundleURL: bundleURL.path,
            bundleIdentifier: bundleIdentifier,
            executablePath: executableURL.path,
            executableSHA256: executableHash,
            processIdentity: identity,
            childProcesses: HarnessProcessIdentity.descendants(of: identity.pid)
        )
    }

    public static func terminate(
        identity: HarnessProcessIdentity,
        knownDescendants: [HarnessProcessIdentity] = [],
        gracefulTimeout: TimeInterval = 5,
        forcedTimeout: TimeInterval = 2
    ) async throws -> [HarnessProcessIdentity] {
        var observedDescendants = Set(knownDescendants)
        guard HarnessProcessIdentity.forPID(identity.pid) == identity else {
            try await terminateExactDescendants(observedDescendants)
            return sorted(observedDescendants)
        }
        guard let application = NSRunningApplication(
            processIdentifier: identity.pid
        ) else {
            throw HarnessAutomationError.runningApplicationUnavailable(identity.pid)
        }
        observedDescendants.formUnion(
            HarnessProcessIdentity.descendants(of: identity.pid)
        )
        application.terminate()
        var observation = await observeUntilExited(
            identity,
            timeout: gracefulTimeout,
            descendants: observedDescendants
        )
        observedDescendants = observation.descendants
        if !observation.exited {
            guard HarnessProcessIdentity.forPID(identity.pid) == identity else {
                try await terminateExactDescendants(observedDescendants)
                return sorted(observedDescendants)
            }
            application.forceTerminate()
            observation = await observeUntilExited(
                identity,
                timeout: forcedTimeout,
                descendants: observedDescendants
            )
            observedDescendants = observation.descendants
            guard observation.exited else {
                throw HarnessAutomationError.terminationFailed(identity.pid)
            }
        }
        try await terminateExactDescendants(observedDescendants)
        return sorted(observedDescendants)
    }

    public static func waitUntilExited(
        _ identity: HarnessProcessIdentity,
        timeout: TimeInterval
    ) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if HarnessProcessIdentity.forPID(identity.pid) != identity {
                return true
            }
            try? await Task.sleep(for: .milliseconds(100))
        }
        return HarnessProcessIdentity.forPID(identity.pid) != identity
    }

    private static func observeUntilExited(
        _ identity: HarnessProcessIdentity,
        timeout: TimeInterval,
        descendants initialDescendants: Set<HarnessProcessIdentity>
    ) async -> (exited: Bool, descendants: Set<HarnessProcessIdentity>) {
        let deadline = Date().addingTimeInterval(timeout)
        var descendants = initialDescendants
        while Date() < deadline {
            guard HarnessProcessIdentity.forPID(identity.pid) == identity else {
                return (true, descendants)
            }
            descendants.formUnion(
                HarnessProcessIdentity.descendants(of: identity.pid)
            )
            try? await Task.sleep(for: .milliseconds(100))
        }
        return (
            HarnessProcessIdentity.forPID(identity.pid) != identity,
            descendants
        )
    }

    private static func terminateExactDescendants(
        _ descendants: Set<HarnessProcessIdentity>
    ) async throws {
        for descendant in descendants.sorted(by: { $0.pid > $1.pid }) {
            guard HarnessProcessIdentity.forPID(descendant.pid) == descendant else {
                continue
            }
            guard kill(descendant.pid, SIGTERM) == 0 || errno == ESRCH else {
                throw HarnessAutomationError.terminationFailed(descendant.pid)
            }
        }
        let gracefulDeadline = Date().addingTimeInterval(2)
        while Date() < gracefulDeadline {
            if descendants.allSatisfy({
                HarnessProcessIdentity.forPID($0.pid) != $0
            }) {
                return
            }
            try? await Task.sleep(for: .milliseconds(50))
        }
        for descendant in descendants {
            guard HarnessProcessIdentity.forPID(descendant.pid) == descendant else {
                continue
            }
            guard kill(descendant.pid, SIGKILL) == 0 || errno == ESRCH else {
                throw HarnessAutomationError.terminationFailed(descendant.pid)
            }
        }
        let forcedDeadline = Date().addingTimeInterval(2)
        while Date() < forcedDeadline {
            if descendants.allSatisfy({
                HarnessProcessIdentity.forPID($0.pid) != $0
            }) {
                return
            }
            try? await Task.sleep(for: .milliseconds(50))
        }
        if let survivor = descendants.first(where: {
            HarnessProcessIdentity.forPID($0.pid) == $0
        }) {
            throw HarnessAutomationError.terminationFailed(survivor.pid)
        }
    }

    private static func sorted(
        _ identities: Set<HarnessProcessIdentity>
    ) -> [HarnessProcessIdentity] {
        identities.sorted { lhs, rhs in
            lhs.pid == rhs.pid
                ? lhs.startMicroseconds < rhs.startMicroseconds
                : lhs.pid < rhs.pid
        }
    }
}

public enum HarnessAutomationError: Error, CustomStringConvertible {
    case accessibilityPermissionMissing
    case screenRecordingPermissionMissing
    case invalidBundle(String)
    case launchFailed(String)
    case processIdentityUnavailable(pid_t)
    case runningApplicationUnavailable(pid_t)
    case terminationFailed(pid_t)
    case elementNotFound(String)
    case unsupportedAction(String)
    case valueRequired
    case accessibilityFailure(Int32)
    case semanticCaptureTimedOut
    case elementSearchTimedOut(String)
    case windowNotFound(pid_t)
    case displayNotFound
    case missingCapturedImage
    case cannotWriteCapture(String)
    case cannotReadCapture(String)
    case occlusionMarkerNotVisible
    case occlusionIsolationFailed

    public var failureCode: AutomationFailureCode {
        switch self {
        case .accessibilityPermissionMissing: .accessibilityPermissionMissing
        case .screenRecordingPermissionMissing: .screenRecordingPermissionMissing
        case .invalidBundle: .invalidBundle
        case .launchFailed: .launchFailed
        case .processIdentityUnavailable: .processIdentityUnavailable
        case .runningApplicationUnavailable: .runningApplicationUnavailable
        case .terminationFailed: .terminationFailed
        case .elementNotFound: .elementNotFound
        case .unsupportedAction: .unsupportedAction
        case .valueRequired: .valueRequired
        case .accessibilityFailure: .accessibilityFailure
        case .semanticCaptureTimedOut: .semanticCaptureTimedOut
        case .elementSearchTimedOut: .elementSearchTimedOut
        case .windowNotFound: .windowNotFound
        case .displayNotFound: .displayNotFound
        case .missingCapturedImage: .missingCapturedImage
        case .cannotWriteCapture: .cannotWriteCapture
        case .cannotReadCapture: .cannotReadCapture
        case .occlusionMarkerNotVisible: .occlusionMarkerNotVisible
        case .occlusionIsolationFailed: .occlusionIsolationFailed
        }
    }

    public var description: String {
        switch self {
        case .accessibilityPermissionMissing:
            "Accessibility permission is unavailable for the stable UI test driver."
        case .screenRecordingPermissionMissing:
            "Screen Recording permission is unavailable for the stable UI test driver."
        case let .invalidBundle(path):
            "invalid application bundle: \(path)"
        case let .launchFailed(path):
            "failed to launch application bundle: \(path)"
        case let .processIdentityUnavailable(pid):
            "process identity unavailable for pid \(pid)"
        case let .runningApplicationUnavailable(pid):
            "running application unavailable for pid \(pid)"
        case let .terminationFailed(pid):
            "exact process \(pid) did not exit"
        case let .elementNotFound(identifier):
            "accessibility element not found: \(identifier)"
        case let .unsupportedAction(action):
            "unsupported accessibility action: \(action)"
        case .valueRequired:
            "setValue requires a value"
        case let .accessibilityFailure(code):
            "accessibility operation failed with code \(code)"
        case .semanticCaptureTimedOut:
            "bounded semantic-tree capture timed out"
        case let .elementSearchTimedOut(description):
            "accessibility element search timed out: \(description)"
        case let .windowNotFound(pid):
            "no target window found for pid \(pid)"
        case .displayNotFound:
            "no ScreenCaptureKit display contains the target window"
        case .missingCapturedImage:
            "ScreenCaptureKit returned no SDR image"
        case let .cannotWriteCapture(path):
            "could not write capture: \(path)"
        case let .cannotReadCapture(path):
            "could not inspect capture pixels: \(path)"
        case .occlusionMarkerNotVisible:
            "controlled occlusion marker was not visible in the display proof"
        case .occlusionIsolationFailed:
            "desktop-independent capture included the marker or clipped the target"
        }
    }
}
