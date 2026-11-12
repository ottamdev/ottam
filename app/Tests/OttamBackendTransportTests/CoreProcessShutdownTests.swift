import Foundation
import Testing
@testable import OttamBackendClient

/// How a closing connection reaps its core. The core exits on stdin EOF and
/// may need a moment to flush; a signal sent in that moment cuts it short.
@Suite("Core process shutdown")
struct CoreProcessShutdownTests {
    /// Logs `EOF` when stdin closes, takes half a second to finish, then logs
    /// `EXIT`. A SIGTERM at any point logs `TERM` instead.
    private static let flushingCore = """
    trap 'echo TERM >> "$0"; exit 143' TERM
    echo READY >> "$0"
    cat > /dev/null & wait $!
    echo EOF >> "$0"
    sleep 0.5 & wait $!
    echo EXIT >> "$0"
    """

    /// Never reads stdin, so only a signal ends it.
    private static let wedgedCore = """
    trap 'echo TERM >> "$0"; exit 143' TERM
    echo READY >> "$0"
    sleep 30 & wait $!
    """

    @Test("a core that exits after stdin EOF is not sent SIGTERM")
    func exitsOnEOFBeforeTerminate() async throws {
        let lines = try await reap(script: Self.flushingCore, exitGrace: .seconds(3))
        #expect(lines == ["READY", "EOF", "EXIT"])
    }

    @Test("a core that ignores stdin EOF is sent SIGTERM after the grace")
    func wedgedCoreIsTerminated() async throws {
        let lines = try await reap(script: Self.wedgedCore, exitGrace: .milliseconds(200))
        #expect(lines == ["READY", "TERM"])
    }

    /// Runs `script` until its trap is installed, closes its stdin, reaps it,
    /// and returns what it logged.
    private func reap(script: String, exitGrace: Duration) async throws -> [String] {
        let log = FileManager.default.temporaryDirectory
            .appendingPathComponent("core-shutdown-\(UUID().uuidString).log")
        defer { try? FileManager.default.removeItem(at: log) }
        func lines() -> [String] {
            ((try? String(contentsOf: log, encoding: .utf8)) ?? "")
                .split(separator: "\n").map(String.init)
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", script, log.path]
        let stdin = Pipe()
        process.standardInput = stdin
        try process.run()

        let deadline = Date().addingTimeInterval(10)
        while lines().isEmpty, Date() < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let protection = CoreRefreshProtection()
        protection.update(paths: [], hasSharedHomes: false)
        try stdin.fileHandleForWriting.close()
        LocalProcessConnection.terminateAndReap(process, protection: protection, exitGrace: exitGrace)

        while process.isRunning, Date() < deadline {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        #expect(!process.isRunning)
        return lines()
    }
}
