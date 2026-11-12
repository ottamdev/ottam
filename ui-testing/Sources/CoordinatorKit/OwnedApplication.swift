import CryptoKit
import Darwin
import Foundation
import UIHarnessCore

struct ProcessRecord: Codable, Hashable {
    let pid: pid_t
    let identity: String
}

enum ProcessIdentity {
    static func forPID(_ pid: pid_t) -> String? {
        HarnessProcessIdentity.forPID(pid)?.stableValue
    }

    static func record(forPID pid: pid_t) -> ProcessRecord? {
        guard let identity = HarnessProcessIdentity.forPID(pid) else {
            return nil
        }
        return ProcessRecord(
            pid: pid,
            identity: identity.stableValue
        )
    }

    static func descendants(of rootPID: pid_t) -> Set<ProcessRecord> {
        Set(
            HarnessProcessIdentity.descendants(of: rootPID).map {
                ProcessRecord(pid: $0.pid, identity: $0.stableValue)
            }
        )
    }
}

enum OwnedApplication {
    static func encodeChildren(_ children: Set<ProcessRecord>) throws -> String {
        let data = try JSONEncoder().encode(
            children.sorted { $0.pid < $1.pid }
        )
        return String(decoding: data, as: UTF8.self)
    }

    static func decodeChildren(_ value: String) throws -> Set<ProcessRecord> {
        guard !value.isEmpty else { return [] }
        return Set(try JSONDecoder().decode([ProcessRecord].self, from: Data(value.utf8)))
    }

    static func childDictionaries(_ value: String) throws -> [[String: Any]] {
        try decodeChildren(value)
            .sorted { $0.pid < $1.pid }
            .map { ["pid": Int($0.pid), "processIdentity": $0.identity] }
    }

    static func validate(
        appURL: String,
        appPID: pid_t,
        bundleIdentifier: String,
        executablePath: String,
        executableSHA256: String
    ) throws {
        let standardizedExecutable = try validateStagedBundle(
            appURL: appURL,
            bundleIdentifier: bundleIdentifier,
            executablePath: executablePath,
            executableSHA256: executableSHA256
        )
        guard processExecutablePath(appPID) == standardizedExecutable.path else {
            throw CoordinatorError.unsafeRecovery(
                "attached application executable does not match pid \(appPID)"
            )
        }
    }

    @discardableResult
    static func validateStagedBundle(
        appURL: String,
        bundleIdentifier: String,
        executablePath: String,
        executableSHA256: String
    ) throws -> URL {
        let standardizedAppURL = URL(fileURLWithPath: appURL)
            .resolvingSymlinksInPath()
            .standardizedFileURL
        let standardizedExecutable = URL(fileURLWithPath: executablePath)
            .resolvingSymlinksInPath()
            .standardizedFileURL
        guard bundleIdentifier.hasPrefix("dev.ottam.Ottam.ui-test."),
              standardizedExecutable.path.hasPrefix(
                standardizedAppURL.appendingPathComponent("Contents/MacOS").path + "/"
              ),
              Bundle(path: standardizedAppURL.path)?.bundleIdentifier == bundleIdentifier
        else {
            throw CoordinatorError.unsafeRecovery(
                "attached application identity is not an isolated Ottam UI test bundle"
            )
        }
        let data = try Data(contentsOf: standardizedExecutable, options: .mappedIfSafe)
        let observedHash = SHA256.hash(data: data)
            .map { String(format: "%02x", $0) }
            .joined()
        guard observedHash == executableSHA256 else {
            throw CoordinatorError.unsafeRecovery(
                "staged application executable hash does not match its recorded value"
            )
        }
        return standardizedExecutable
    }

    @discardableResult
    private static func validateRecordedLiveApplication(
        appURL: String,
        appPID: pid_t,
        bundleIdentifier: String,
        executablePath: String
    ) throws -> URL {
        let standardizedAppURL = URL(fileURLWithPath: appURL)
            .resolvingSymlinksInPath()
            .standardizedFileURL
        let standardizedExecutable = URL(fileURLWithPath: executablePath)
            .resolvingSymlinksInPath()
            .standardizedFileURL
        guard bundleIdentifier.hasPrefix("dev.ottam.Ottam.ui-test.") else {
            throw CoordinatorError.unsafeRecovery(
                "recorded application bundle identifier is not isolated"
            )
        }
        guard standardizedExecutable.path.hasPrefix(
            standardizedAppURL.appendingPathComponent("Contents/MacOS").path + "/"
        ) else {
            throw CoordinatorError.unsafeRecovery(
                "recorded executable is outside its staged application"
            )
        }
        let observedExecutablePath = processExecutablePath(appPID)
        // `proc_pidpath` returns no path after the staged executable is
        // unlinked even though that exact recorded process can keep its vnode
        // open. A different observed path is unsafe; no path is the expected
        // interrupted-run case this fallback exists to recover.
        if let observedExecutablePath,
           observedExecutablePath != standardizedExecutable.path
        {
            throw CoordinatorError.unsafeRecovery(
                "live executable path \(observedExecutablePath) "
                    + "does not match recorded path \(standardizedExecutable.path)"
            )
        }
        return standardizedExecutable
    }

    static func cleanupIfPresent(_ row: [String: String]) throws -> [String: Any] {
        var descendants = try decodeChildren(row["app_child_identities"] ?? "")
        guard let appURL = nonempty(row["staged_app_url"]),
              let bundleIdentifier = nonempty(row["bundle_identifier"]),
              let executablePath = nonempty(row["executable_path"]),
              let executableSHA256 = nonempty(row["executable_sha256"])
        else {
            return [
                "status": "no_staged_application",
                "exited": true,
                "childrenExited": true,
            ]
        }
        let recordedPID = pid_t(row["app_pid"] ?? "")
        let recordedAppIdentity = nonempty(row["app_process_identity"])
        if let recordedPID, let recordedAppIdentity,
           ProcessIdentity.forPID(recordedPID) != recordedAppIdentity
        {
            // Once the exact recorded app identity is gone, no app process is
            // left to authorize against staged bytes that an interrupted run
            // may already have removed. Known children remain safe to clean by
            // their own exact process identities.
            try terminateExactChildren(descendants)
            return [
                "status": "attached_application_already_exited",
                "pid": Int(recordedPID),
                "exited": true,
                "childrenExited": true,
                "observedChildren": try childDictionaries(
                    encodeChildren(descendants)
                ),
            ]
        }
        let stagedExecutableExists = FileManager.default.fileExists(
            atPath: executablePath
        )
        let standardizedExecutable: URL
        if stagedExecutableExists {
            standardizedExecutable = try validateStagedBundle(
                appURL: appURL,
                bundleIdentifier: bundleIdentifier,
                executablePath: executablePath,
                executableSHA256: executableSHA256
            )
        } else if let recordedPID, let recordedAppIdentity,
                  ProcessIdentity.forPID(recordedPID) == recordedAppIdentity
        {
            // The evidence directory can be replaced after an interrupted run
            // while the executable vnode remains live. The queue's exact
            // process identity and executable path still safely identify that
            // isolated process even though its staged bytes are gone.
            standardizedExecutable = try validateRecordedLiveApplication(
                appURL: appURL,
                appPID: recordedPID,
                bundleIdentifier: bundleIdentifier,
                executablePath: executablePath
            )
        } else {
            throw CoordinatorError.unsafeRecovery(
                "staged application is missing without a matching live process"
            )
        }
        descendants.formUnion(bundledHelperProcesses(appURL: appURL))
        let discovered = exactProcesses(executablePath: standardizedExecutable.path)
        guard discovered.count <= 1 else {
            throw CoordinatorError.unsafeRecovery(
                "multiple processes execute the unique staged application path"
            )
        }
        let appRecord: ProcessRecord?
        if let recordedPID, let recordedAppIdentity {
            appRecord = ProcessRecord(pid: recordedPID, identity: recordedAppIdentity)
            if let liveExact = discovered.first, liveExact != appRecord {
                throw CoordinatorError.unsafeRecovery(
                    "the staged executable is running under an unrecorded process identity"
                )
            }
        } else {
            appRecord = discovered.first
        }
        guard let appRecord else {
            try terminateExactChildren(descendants)
            return [
                "status": "staged_application_not_running",
                "exited": true,
                "childrenExited": true,
                "observedChildren": try childDictionaries(encodeChildren(descendants)),
            ]
        }
        let appPID = appRecord.pid
        let targetIdentity = appRecord.identity
        guard ProcessIdentity.forPID(appPID) == targetIdentity else {
            try terminateExactChildren(descendants)
            return [
                "status": "attached_application_already_exited",
                "pid": Int(appPID),
                "exited": true,
                "childrenExited": true,
                "observedChildren": try childDictionaries(
                    encodeChildren(descendants)
                ),
            ]
        }
        if stagedExecutableExists {
            try validate(
                appURL: appURL,
                appPID: appPID,
                bundleIdentifier: bundleIdentifier,
                executablePath: executablePath,
                executableSHA256: executableSHA256
            )
        } else {
            try validateRecordedLiveApplication(
                appURL: appURL,
                appPID: appPID,
                bundleIdentifier: bundleIdentifier,
                executablePath: executablePath
            )
        }

        descendants.formUnion(ProcessIdentity.descendants(of: appPID))
        guard kill(appPID, SIGTERM) == 0 || errno == ESRCH else {
            throw CoordinatorError.cleanupFailed(appPID)
        }
        var deadline = Date().addingTimeInterval(5)
        while Date() < deadline, ProcessIdentity.forPID(appPID) == targetIdentity {
            descendants.formUnion(ProcessIdentity.descendants(of: appPID))
            Thread.sleep(forTimeInterval: 0.05)
        }
        if ProcessIdentity.forPID(appPID) == targetIdentity {
            guard kill(appPID, SIGKILL) == 0 || errno == ESRCH else {
                throw CoordinatorError.cleanupFailed(appPID)
            }
            deadline = Date().addingTimeInterval(2)
            while Date() < deadline, ProcessIdentity.forPID(appPID) == targetIdentity {
                Thread.sleep(forTimeInterval: 0.05)
            }
        }
        guard ProcessIdentity.forPID(appPID) != targetIdentity else {
            throw CoordinatorError.cleanupFailed(appPID)
        }

        try terminateExactChildren(descendants)
        return [
            "status": "attached_application_cleaned",
            "pid": Int(appPID),
            "processIdentity": targetIdentity,
            "exited": true,
            "childrenExited": true,
            "observedChildren": descendants
                .sorted { $0.pid < $1.pid }
                .map { ["pid": Int($0.pid), "processIdentity": $0.identity] },
        ]
    }

    private static func terminateExactChildren(
        _ descendants: Set<ProcessRecord>
    ) throws {
        for child in descendants {
            guard ProcessIdentity.forPID(child.pid) == child.identity else { continue }
            guard kill(child.pid, SIGTERM) == 0 || errno == ESRCH else {
                throw CoordinatorError.cleanupFailed(child.pid)
            }
        }
        var deadline = Date().addingTimeInterval(2)
        while Date() < deadline,
              descendants.contains(where: {
                  ProcessIdentity.forPID($0.pid) == $0.identity
              })
        {
            Thread.sleep(forTimeInterval: 0.05)
        }
        for child in descendants
        where ProcessIdentity.forPID(child.pid) == child.identity {
            guard kill(child.pid, SIGKILL) == 0 || errno == ESRCH else {
                throw CoordinatorError.cleanupFailed(child.pid)
            }
        }
        deadline = Date().addingTimeInterval(2)
        while Date() < deadline,
              descendants.contains(where: {
                  ProcessIdentity.forPID($0.pid) == $0.identity
              })
        {
            Thread.sleep(forTimeInterval: 0.05)
        }
        if let survivor = descendants.first(where: {
            ProcessIdentity.forPID($0.pid) == $0.identity
        }) {
            throw CoordinatorError.cleanupFailed(survivor.pid)
        }
    }

    private static func processExecutablePath(_ pid: pid_t) -> String? {
        // `PROC_PIDPATHINFO_MAXSIZE` is a C macro unavailable to Swift.
        // Darwin defines it as four times MAXPATHLEN (4096 bytes on macOS).
        var buffer = [CChar](repeating: 0, count: 4_096)
        guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else {
            return nil
        }
        return URL(fileURLWithPath: String(cString: buffer))
            .resolvingSymlinksInPath()
            .standardizedFileURL
            .path
    }

    private static func exactProcesses(executablePath: String) -> [ProcessRecord] {
        let capacity = max(32, Int(proc_listallpids(nil, 0)) + 32)
        var pids = [pid_t](repeating: 0, count: capacity)
        let written = proc_listallpids(
            &pids,
            Int32(pids.count * MemoryLayout<pid_t>.size)
        )
        guard written > 0 else { return [] }
        let count = min(pids.count, Int(written))
        return pids.prefix(count).compactMap { pid in
            guard processExecutablePath(pid) == executablePath else { return nil }
            return ProcessIdentity.record(forPID: pid)
        }
    }

    static func bundledHelperProcesses(appURL: String) -> Set<ProcessRecord> {
        let helperPath = URL(fileURLWithPath: appURL)
            .appendingPathComponent("Contents/MacOS/ottam-core")
            .resolvingSymlinksInPath()
            .standardizedFileURL
            .path
        return Set(exactProcesses(executablePath: helperPath))
    }

    private static func nonempty(_ value: String?) -> String? {
        guard let value, !value.isEmpty else { return nil }
        return value
    }
}

enum CoordinatorError: Error, CustomStringConvertible {
    case usage
    case missing(String)
    case invalidValue(String, String)
    case sqlite(String)
    case ticketNotFound(String)
    case invalidState(String)
    case invalidTransition(String, String)
    case runnerNotAlive(pid_t)
    case applicationNotAlive(pid_t)
    case runnerIdentityMismatch(pid_t)
    case queueTimedOut(String)
    case cancelled(String)
    case unsafeRecovery(String)
    case cleanupFailed(pid_t)

    var description: String {
        switch self {
        case .usage:
            return """
            usage: OttamUITestCoordinator \
            enqueue|wait|heartbeat|stage-app|attach-app|cleanup-app|release|cancel|recover|status [options]
            """
        case let .missing(flag):
            return "missing \(flag)"
        case let .invalidValue(flag, value):
            return "invalid \(flag): \(value)"
        case let .sqlite(message):
            return "sqlite: \(message)"
        case let .ticketNotFound(ticket):
            return "ticket not found: \(ticket)"
        case let .invalidState(state):
            return "invalid ticket state: \(state)"
        case let .invalidTransition(state, target):
            return "cannot transition \(state) to \(target)"
        case let .runnerNotAlive(pid):
            return "runner pid \(pid) is not alive"
        case let .applicationNotAlive(pid):
            return "staged application pid \(pid) is not alive"
        case let .runnerIdentityMismatch(pid):
            return "runner pid \(pid) no longer matches its recorded process identity"
        case let .queueTimedOut(ticket):
            return "queue_timed_out: \(ticket)"
        case let .cancelled(ticket):
            return "ticket cancelled: \(ticket)"
        case let .unsafeRecovery(message):
            return "environment_unavailable: unsafe stale-owner recovery: \(message)"
        case let .cleanupFailed(pid):
            return "cleanup_failed: exact process \(pid) survived stale-owner recovery"
        }
    }
}
