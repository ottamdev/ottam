import AppKit
import Darwin
import Foundation
import MetricKit
import OSLog

private let exitLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "ExitDiagnostics"
)

/// Leaves a record of how the app process ended when it didn't quit through
/// AppKit's graceful terminate (the path that asks Core to prepare shutdown).
///
/// - A session marker (`AppSessionMarkerStore`) written at launch and cleared
///   in `applicationWillTerminate` or by `exitDeliberately`; the next launch
///   logs one error when it is still there, with whatever the hooks below
///   recorded.
/// - An uncaught `NSException` handler. Debug builds also set
///   `NSApplicationCrashOnExceptions`, so AppKit doesn't log and swallow
///   exceptions raised during event handling.
/// - Handlers for fatal and terminating signals still at their default
///   disposition. Each writes a pre-formatted line (async-signal-safe) to the
///   exit-cause log and stderr, then restores the default and re-raises, so
///   macOS still writes its crash report.
/// - An `atexit` hook that logs a direct `exit()` and its caller.
/// - MetricKit crash diagnostics, when the system delivers them.
///
/// SIGKILL, jetsam, and power loss run none of this, nor does a crash the
/// handler can't run for (a stack overflow off the main thread); for those the
/// marker alone says the session ended without a recorded cause.
enum ExitDiagnostics {
    nonisolated(unsafe) private static var store: AppSessionMarkerStore?
    nonisolated(unsafe) private static var gracefulQuit = false
    nonisolated(unsafe) private static var previousExceptionHandler: (@convention(c) (NSException) -> Void)?
    nonisolated(unsafe) private static var metricSubscriber: CrashDiagnosticSubscriber?

    /// Signals whose default action ends the process. The first group also
    /// produces a crash report. SIGTERM is `TerminationSignalHandler`'s; SIGKILL
    /// and SIGSTOP can't be caught.
    static let recordedSignals: [(Int32, String)] = [
        (SIGSEGV, "SIGSEGV"), (SIGBUS, "SIGBUS"), (SIGILL, "SIGILL"), (SIGABRT, "SIGABRT"),
        (SIGTRAP, "SIGTRAP"), (SIGFPE, "SIGFPE"), (SIGSYS, "SIGSYS"), (SIGQUIT, "SIGQUIT"),
        (SIGHUP, "SIGHUP"), (SIGINT, "SIGINT"), (SIGPIPE, "SIGPIPE"), (SIGXCPU, "SIGXCPU"),
        (SIGXFSZ, "SIGXFSZ"), (SIGUSR1, "SIGUSR1"), (SIGUSR2, "SIGUSR2"),
    ]

    /// Call first thing at launch, on the main thread (the alternate signal
    /// stack covers the thread that installs it).
    @MainActor
    static func install() {
        guard !SignalRecord.installed else { return }
        beginSession()
#if DEBUG
        // Release builds keep AppKit's log-and-continue for exceptions raised
        // during event handling; development builds crash so they get a record.
        UserDefaults.standard.register(defaults: ["NSApplicationCrashOnExceptions": true])
#endif
        previousExceptionHandler = NSGetUncaughtExceptionHandler()
        NSSetUncaughtExceptionHandler { exception in
            ExitDiagnostics.recordUncaughtException(exception)
        }
        SignalRecord.install(signals: recordedSignals, pid: getpid())
        if store != nil {
            atexit {
                ExitDiagnostics.recordProcessExit()
            }
        }
    }

    /// After launch, so MetricKit's setup stays off the launch path.
    @MainActor
    static func subscribeToCrashDiagnostics() {
        guard metricSubscriber == nil else { return }
        let subscriber = CrashDiagnosticSubscriber()
        MXMetricManager.shared.add(subscriber)
        metricSubscriber = subscriber
    }

    /// The graceful terminate path: AppKit approved the quit (after Core
    /// prepared shutdown) and is about to exit.
    @MainActor
    static func recordGracefulQuit() {
        gracefulQuit = true
        store?.clear(pid: getpid())
    }

    /// Ends the process on purpose without AppKit's terminate (headless
    /// capture, UI-test preparation), so it isn't reported as an unclean exit.
    @MainActor
    static func exitDeliberately(_ status: Int32) -> Never {
        recordGracefulQuit()
        exit(status)
    }

    @MainActor
    private static func beginSession() {
        let store = AppSessionMarkerStore.default()
        switch store.previousSession() {
        case .none:
            break
        case .ended(let previous):
            logUncleanExit(previous)
        case .running(let other):
            // Taking over would overwrite its marker and truncate the cause
            // log it is still appending to.
            exitLogger.notice("not tracking this session: Ottam pid=\(other.pid, privacy: .public) is running with the same home")
            return
        }
        let pid = getpid()
        let marker = AppSessionMarker(
            pid: pid,
            startedAt: AppSessionMarkerStore.processStartTime(pid: pid) ?? Date(),
            build: buildDescription()
        )
        do {
            SignalRecord.exitCauseDescriptor = try store.begin(marker)
            self.store = store
        } catch {
            exitLogger.error("failed to write app session marker: \(error.localizedDescription, privacy: .public)")
        }
    }

    private static func logUncleanExit(_ exit: UncleanAppExit) {
        let cause = exit.recordedCauses.isEmpty
            ? "none recorded (killed from outside by SIGKILL, jetsam, or a hang watchdog; power loss; or a crash the hooks couldn't record, so check ~/Library/Logs/DiagnosticReports)"
            : exit.recordedCauses.joined(separator: "; ")
        exitLogger.error("""
            previous Ottam session ended without quitting \
            pid=\(exit.marker.pid, privacy: .public) \
            started=\(exit.marker.startedAt.formatted(.iso8601), privacy: .public) \
            build=\(exit.marker.build, privacy: .public) \
            cause=\(cause, privacy: .public)
            """)
    }

    private static func buildDescription() -> String {
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "?"
        let build = info["CFBundleVersion"] as? String ?? "?"
        var description = "\(version) (\(build))"
        if let executable = Bundle.main.executableURL,
           let modified = try? executable.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate {
            description += " binary=\(modified.formatted(.iso8601))"
        }
        return description
    }

    private static func recordUncaughtException(_ exception: NSException) {
        // The reason can carry app data, so it stays private here and out of
        // the cause log; Foundation prints it to stderr as the process aborts.
        let name = exception.name.rawValue
        exitLogger.fault("uncaught exception name=\(name, privacy: .public) reason=\(exception.reason ?? "", privacy: .private)")
        logStack(exception.callStackSymbols, label: "uncaught exception")
        writeCause("uncaught exception \(name)")
        previousExceptionHandler?(exception)
    }

    private static func recordProcessExit() {
        guard !gracefulQuit else {
            exitLogger.notice("process exiting after graceful quit")
            return
        }
        let stack = Thread.callStackSymbols
        let caller = exitCaller(in: stack).map { ", called from \($0)" } ?? ""
        exitLogger.error("exit() without a graceful quit\(caller, privacy: .public)")
        logStack(stack, label: "exit()")
        writeCause("exit() without a graceful quit\(caller)")
    }

    /// The frame that called `exit`, as "image symbol + offset".
    static func exitCaller(in stack: [String]) -> String? {
        let frames = stack.map { $0.split(separator: " ", omittingEmptySubsequences: true) }
        guard let exitIndex = frames.firstIndex(where: { $0.count >= 4 && $0[3] == "exit" }),
              exitIndex + 1 < frames.count else {
            return nil
        }
        let caller = frames[exitIndex + 1]
        guard caller.count >= 4 else { return nil }
        return ([caller[1]] + caller[3...]).joined(separator: " ")
    }

    /// Unified logging truncates long messages, so frames go out in chunks.
    private static func logStack(_ frames: [String], label: String) {
        stride(from: 0, to: frames.count, by: 12).forEach { start in
            let chunk = frames[start..<min(start + 12, frames.count)].joined(separator: "\n")
            exitLogger.error("\(label, privacy: .public) stack:\n\(chunk, privacy: .public)")
        }
        FileHandle.standardError.write(Data("ottam-app: \(label) stack:\n\(frames.joined(separator: "\n"))\n".utf8))
    }

    /// Not for signal handlers: formats and allocates.
    private static func writeCause(_ cause: String) {
        let line = "\(Date().formatted(.iso8601)) \(cause)\n"
        FileHandle.standardError.write(Data("ottam-app: \(line)".utf8))
        let descriptor = SignalRecord.exitCauseDescriptor
        guard descriptor >= 0 else { return }
        line.utf8CString.withUnsafeBufferPointer { buffer in
            _ = write(descriptor, buffer.baseAddress, buffer.count - 1)
        }
    }
}

/// State the signal handler reads. Everything is allocated and formatted in
/// `install` before any handler is armed, so the handler itself only calls
/// `write`, `sigaction`, and `raise`.
private enum SignalRecord {
    nonisolated(unsafe) static var installed = false
    nonisolated(unsafe) static var exitCauseDescriptor: Int32 = -1
    nonisolated(unsafe) static var lines: UnsafeMutablePointer<UnsafeMutableBufferPointer<UInt8>>?

    static func install(signals: [(Int32, String)], pid: Int32) {
        let count = Int(NSIG)
        let lines = UnsafeMutablePointer<UnsafeMutableBufferPointer<UInt8>>.allocate(capacity: count)
        lines.initialize(repeating: UnsafeMutableBufferPointer(start: nil, count: 0), count: count)
        for (signal, name) in signals {
            let text = Array("signal \(name) (\(signal)) pid=\(pid)\n".utf8)
            let buffer = UnsafeMutableBufferPointer<UInt8>.allocate(capacity: text.count)
            _ = buffer.initialize(from: text)
            lines[Int(signal)] = buffer
        }
        Self.lines = lines
        installed = true

        // Handlers run on the alternate stack so a stack overflow on the main
        // thread still gets its line written.
        let stackSize = Int(SIGSTKSZ)
        var alternateStack = stack_t(ss_sp: UnsafeMutableRawPointer.allocate(byteCount: stackSize, alignment: 16), ss_size: stackSize, ss_flags: 0)
        sigaltstack(&alternateStack, nil)

        for (signal, _) in signals {
            // Only the default disposition ends the process for certain. An
            // ignored signal doesn't, and a handler installed by someone else
            // may handle it and carry on, which would record a false cause.
            var current = sigaction()
            guard sigaction(signal, nil, &current) == 0, current.__sigaction_u.__sa_handler == nil else {
                continue
            }
            var action = sigaction()
            action.__sigaction_u.__sa_handler = recordSignalAndReraise
            action.sa_mask = 0
            action.sa_flags = SA_ONSTACK
            sigaction(signal, &action, nil)
        }
    }
}

/// Async-signal-safe: no allocation, locks, or Swift runtime calls beyond
/// reading the pointers set up in `SignalRecord.install`. Only installed over
/// the default disposition, so restoring that is restoring the previous one.
private func recordSignalAndReraise(_ signal: Int32) {
    if let lines = SignalRecord.lines, let line = lines[Int(signal)].baseAddress {
        let count = lines[Int(signal)].count
        let descriptor = SignalRecord.exitCauseDescriptor
        if descriptor >= 0 {
            _ = write(descriptor, line, count)
        }
        _ = write(STDERR_FILENO, line, count)
    }
    // The signal stays blocked until this handler returns, so the re-raise is
    // delivered under the default disposition right after; a hardware fault
    // also re-faults on return.
    Darwin.signal(signal, SIG_DFL)
    raise(signal)
}

/// MetricKit delivers crash diagnostics from earlier sessions some time after
/// a later launch (not immediately, and not for every build).
private final class CrashDiagnosticSubscriber: NSObject, MXMetricManagerSubscriber {
    func didReceive(_ payloads: [MXDiagnosticPayload]) {
        for payload in payloads {
            for crash in payload.crashDiagnostics ?? [] {
                let signal = crash.signal.map { "\($0)" } ?? "?"
                let exceptionType = crash.exceptionType.map { "\($0)" } ?? "?"
                let exceptionCode = crash.exceptionCode.map { "\($0)" } ?? "?"
                let reason = crash.terminationReason ?? "?"
                exitLogger.error("""
                    MetricKit crash diagnostic \
                    build=\(crash.metaData.applicationBuildVersion, privacy: .public) \
                    at=\(payload.timeStampBegin.formatted(.iso8601), privacy: .public) \
                    signal=\(signal, privacy: .public) \
                    exceptionType=\(exceptionType, privacy: .public) \
                    exceptionCode=\(exceptionCode, privacy: .public) \
                    reason=\(reason, privacy: .public)
                    """)
            }
        }
    }
}
