import OttamBackendClient
import Darwin
import Foundation

/// The "running" record an app session leaves in the Ottam home until it
/// quits gracefully. Finding one at launch means the previous session ended
/// some other way: a crash, a signal, a direct `exit()`, or a kill from outside.
struct AppSessionMarker: Codable, Equatable {
    var pid: Int32
    var startedAt: Date
    var build: String
}

/// How the previous session ended, read back at launch.
struct UncleanAppExit: Equatable {
    var marker: AppSessionMarker
    /// Lines the exit hooks wrote while that session went down (signal,
    /// uncaught exception, `exit()`), oldest first. Empty when nothing in the
    /// process got to run: a kill from outside (SIGKILL, jetsam, a hang), power
    /// loss, or a crash the hooks couldn't record.
    var recordedCauses: [String]
}

/// What the marker left in the Ottam home says about the last session.
enum PreviousAppSession: Equatable {
    /// No marker: the last session quit gracefully, or there was none.
    case none
    /// The marker's process is still alive: another instance owns this home.
    case running(AppSessionMarker)
    case ended(UncleanAppExit)
}

/// Owns `app-session.json` (the marker) and `app-session-exit.log` (exit-cause
/// lines written by `ExitDiagnostics`, including from signal handlers through
/// a descriptor opened here).
struct AppSessionMarkerStore {
    var directory: URL

    var markerURL: URL { directory.appendingPathComponent("app-session.json") }
    var exitCauseURL: URL { directory.appendingPathComponent("app-session-exit.log") }

    static func `default`(environment: [String: String] = ProcessInfo.processInfo.environment) -> AppSessionMarkerStore {
        AppSessionMarkerStore(directory: OttamPaths.home(environment: environment))
    }

    func previousSession(isRunning: (AppSessionMarker) -> Bool = Self.isRunning) -> PreviousAppSession {
        guard let marker = readMarker() else { return .none }
        guard !isRunning(marker) else { return .running(marker) }
        let causes = (try? String(contentsOf: exitCauseURL, encoding: .utf8))?
            .split(separator: "\n", omittingEmptySubsequences: true)
            .map(String.init) ?? []
        return .ended(UncleanAppExit(marker: marker, recordedCauses: causes))
    }

    /// Truncates the exit-cause log and writes this session's marker, returning
    /// an append-only descriptor for the log. Read the previous session first,
    /// and don't begin over a running one. The descriptor opens first so a
    /// failure leaves no marker that nothing would clear.
    func begin(_ marker: AppSessionMarker) throws -> Int32 {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let descriptor = open(exitCauseURL.path, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else {
            throw CocoaError(.fileWriteUnknown, userInfo: [NSFilePathErrorKey: exitCauseURL.path])
        }
        do {
            try Self.encoder.encode(marker).write(to: markerURL, options: .atomic)
        } catch {
            close(descriptor)
            throw error
        }
        return descriptor
    }

    /// Removes the marker on a graceful quit. Leaves one written by another
    /// instance alone.
    func clear(pid: Int32) {
        guard readMarker()?.pid == pid else { return }
        try? FileManager.default.removeItem(at: markerURL)
    }

    private func readMarker() -> AppSessionMarker? {
        guard let data = try? Data(contentsOf: markerURL) else { return nil }
        return try? Self.decoder.decode(AppSessionMarker.self, from: data)
    }

    /// Whether the marker's process is still alive. The start time guards
    /// against the pid having been reused by an unrelated process.
    static func isRunning(_ marker: AppSessionMarker) -> Bool {
        guard let started = processStartTime(pid: marker.pid) else { return false }
        return abs(started.timeIntervalSince(marker.startedAt)) < 1
    }

    static func processStartTime(pid: Int32) -> Date? {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid]
        guard sysctl(&mib, u_int(mib.count), &info, &size, nil, 0) == 0, size > 0 else { return nil }
        let start = info.kp_proc.p_un.__p_starttime
        return Date(timeIntervalSince1970: TimeInterval(start.tv_sec) + TimeInterval(start.tv_usec) / 1_000_000)
    }

    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }()
}
