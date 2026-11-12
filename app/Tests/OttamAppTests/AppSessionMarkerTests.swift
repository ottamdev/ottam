import Darwin
import Foundation
import Testing
@testable import OttamApp

@Suite("App session marker")
struct AppSessionMarkerTests {
    private func makeStore() -> AppSessionMarkerStore {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("AppSessionMarkerTests-\(UUID().uuidString)")
        return AppSessionMarkerStore(directory: directory)
    }

    private let marker = AppSessionMarker(
        pid: 4242,
        startedAt: Date(timeIntervalSince1970: 1_800_000_000),
        build: "0.1.0 (1)"
    )

    @Test("a marker left behind reports an unclean exit with the recorded causes")
    func uncleanExitIncludesCauses() throws {
        let store = makeStore()
        defer { try? FileManager.default.removeItem(at: store.directory) }
        #expect(store.previousSession(isRunning: { _ in false }) == .none)

        let descriptor = try store.begin(marker)
        let line = "signal SIGSEGV (11) pid=4242\n"
        _ = line.withCString { write(descriptor, $0, strlen($0)) }
        close(descriptor)

        #expect(store.previousSession(isRunning: { _ in false }) == .ended(UncleanAppExit(
            marker: marker,
            recordedCauses: ["signal SIGSEGV (11) pid=4242"]
        )))
    }

    @Test("beginning a session truncates the previous session's causes")
    func beginTruncatesCauses() throws {
        let store = makeStore()
        defer { try? FileManager.default.removeItem(at: store.directory) }
        let first = try store.begin(marker)
        _ = "exit() without a graceful quit\n".withCString { write(first, $0, strlen($0)) }
        close(first)

        close(try store.begin(marker))
        #expect(store.previousSession(isRunning: { _ in false }) == .ended(UncleanAppExit(marker: marker, recordedCauses: [])))
    }

    @Test("a graceful quit clears only its own marker")
    func clearMatchesPID() throws {
        let store = makeStore()
        defer { try? FileManager.default.removeItem(at: store.directory) }
        close(try store.begin(marker))

        store.clear(pid: marker.pid + 1)
        #expect(store.previousSession(isRunning: { _ in false }) != .none)

        store.clear(pid: marker.pid)
        #expect(store.previousSession(isRunning: { _ in false }) == .none)
    }

    @Test("a failed begin leaves no marker behind")
    func failedBeginLeavesNoMarker() throws {
        let store = makeStore()
        defer { try? FileManager.default.removeItem(at: store.directory) }
        // A directory where the cause log goes makes open() fail.
        try FileManager.default.createDirectory(at: store.exitCauseURL, withIntermediateDirectories: true)
        #expect(throws: (any Error).self) { try store.begin(marker) }
        #expect(!FileManager.default.fileExists(atPath: store.markerURL.path))
    }

    @Test("a marker whose process is still running is not reported")
    func liveInstanceIsNotReported() throws {
        let store = makeStore()
        defer { try? FileManager.default.removeItem(at: store.directory) }
        let pid = getpid()
        let live = AppSessionMarker(
            pid: pid,
            startedAt: try #require(AppSessionMarkerStore.processStartTime(pid: pid)),
            build: "test"
        )
        close(try store.begin(live))
        guard case .running(let running) = store.previousSession() else {
            Issue.record("expected this live process's marker to read as running")
            return
        }
        #expect(running.pid == pid)

        // Same pid, different start: the pid was reused after the session died.
        var reused = live
        reused.startedAt = live.startedAt.addingTimeInterval(-3_600)
        close(try store.begin(reused))
        // The marker stores whole seconds.
        guard case .ended(let exit) = store.previousSession() else {
            Issue.record("expected the reused pid's marker to be reported")
            return
        }
        let reported = exit.marker
        #expect(reported.pid == pid)
        #expect(abs(reported.startedAt.timeIntervalSince(reused.startedAt)) < 1)
    }

    @Test("exit caller is the frame after exit")
    func exitCallerParsesStack() {
        let stack = [
            "0   Ottam                               0x0000000100001000 $s8OttamApp14ExitDiagnosticsO + 12",
            "1   libsystem_c.dylib                   0x0000000180001000 __cxa_finalize_ranges + 400",
            "2   libsystem_c.dylib                   0x0000000180002000 exit + 44",
            "3   Ottam                               0x0000000100002000 $s8OttamApp4MainV4quityyF + 88",
        ]
        #expect(ExitDiagnostics.exitCaller(in: stack) == "Ottam $s8OttamApp4MainV4quityyF + 88")
        #expect(ExitDiagnostics.exitCaller(in: Array(stack.prefix(3))) == nil)
    }
}
