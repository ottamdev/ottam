import Foundation
import Testing
@testable import OttamBackendClient

struct CoreRefreshProtectionTests {
    @Test func idleSharedHomesDoNotBlockQuitOrPermitForceKill() {
        let protection = CoreRefreshProtection()
        protection.update(paths: [], hasSharedHomes: true)
        #expect(!protection.isRefreshing)
        #expect(!protection.canForceTerminate)
    }

    @Test func quitDoesNotLaunchAMissingCore() async throws {
        let connection = LocalProcessConnection(executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core"))
        #expect(try await connection.shutdownReadinessIfRunning() == nil)
        try await connection.prepareShutdownIfRunning(operationKey: "no-core")
        try await connection.cancelShutdownIfRunning(operationKey: "no-core")
        await connection.close()
    }
    @Test func unknownAndUnreadableLocksFailClosed() throws {
        let protection = CoreRefreshProtection()
        #expect(protection.isRefreshing)
        #expect(!protection.canForceTerminate)
        protection.update(paths: [])
        #expect(!protection.isRefreshing)
        #expect(protection.canForceTerminate)
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let path = root.appendingPathComponent(".oauth_refresh.lock")
        protection.update(paths: [path.path])
        #expect(!protection.canForceTerminate)
        #expect(!protection.isRefreshing)
        try FileManager.default.createDirectory(at: path, withIntermediateDirectories: false)
        #expect(protection.isRefreshing)
        try FileManager.default.removeItem(at: path)
        #expect(!protection.isRefreshing)
        let blocked = root.appendingPathComponent("blocked")
        try Data().write(to: blocked)
        protection.update(paths: [blocked.appendingPathComponent(".oauth_refresh.lock").path])
        #expect(protection.isRefreshing)
    }
}
