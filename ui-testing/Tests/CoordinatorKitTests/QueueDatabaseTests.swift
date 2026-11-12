@testable import CoordinatorKit
import Darwin
import Foundation
import XCTest

final class QueueDatabaseTests: XCTestCase {
    private var temporaryDirectory: URL!
    private var databaseURL: URL!

    override func setUpWithError() throws {
        temporaryDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-coordinator-\(UUID().uuidString)")
        try FileManager.default.createDirectory(
            at: temporaryDirectory,
            withIntermediateDirectories: true
        )
        databaseURL = temporaryDirectory.appendingPathComponent("queue.sqlite")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: temporaryDirectory)
    }

    func testFIFOAcquisitionAndRelease() throws {
        let database = try makeDatabase()
        let first = try enqueue(database, runID: "first")
        let second = try enqueue(database, runID: "second")

        XCTAssertTrue(try database.tryAcquire(ticketID: first))
        XCTAssertFalse(try database.tryAcquire(ticketID: second))
        XCTAssertEqual(try database.position(ticketID: second), 1)

        try database.finish(
            ticketID: first,
            expectedState: "acquired",
            newState: "released"
        )
        XCTAssertTrue(try database.tryAcquire(ticketID: second))
    }

    func testHeartbeatAndCancellation() throws {
        let database = try makeDatabase()
        let acquired = try enqueue(database, runID: "heartbeat")
        XCTAssertTrue(try database.tryAcquire(ticketID: acquired))
        let before = try XCTUnwrap(
            database.ticketSnapshot(ticketID: acquired)["heartbeatAt"] as? Double
        )
        Thread.sleep(forTimeInterval: 0.01)
        try database.heartbeat(ticketID: acquired)
        let after = try XCTUnwrap(
            database.ticketSnapshot(ticketID: acquired)["heartbeatAt"] as? Double
        )
        XCTAssertGreaterThan(after, before)

        let cancelled = try enqueue(database, runID: "cancelled")
        try database.finish(
            ticketID: cancelled,
            expectedState: "waiting",
            newState: "cancelled"
        )
        XCTAssertEqual(
            try database.ticketSnapshot(ticketID: cancelled)["state"] as? String,
            "cancelled"
        )
    }

    func testWaitingTicketTimesOut() throws {
        let database = try makeDatabase()
        let ticket = try enqueue(
            database,
            runID: "expired",
            deadline: Date().addingTimeInterval(-1)
        )
        try database.expireWaitingTickets()
        XCTAssertEqual(
            try database.ticketSnapshot(ticketID: ticket)["state"] as? String,
            "timed_out"
        )
    }

    func testAbandonedWaitingTicketDoesNotBlockNextLiveRunner() throws {
        let database = try makeDatabase()
        let abandoned = try enqueue(
            database,
            runID: "abandoned",
            processIdentity: "impossible-process-identity"
        )
        let live = try enqueue(database, runID: "live")

        try database.expireWaitingTickets()

        XCTAssertEqual(
            try database.ticketSnapshot(ticketID: abandoned)["state"] as? String,
            "cancelled"
        )
        XCTAssertTrue(try database.tryAcquire(ticketID: live))
    }

    func testStaleExitedAppDoesNotRequireDeletedStagedBundle() throws {
        let result = try OwnedApplication.cleanupIfPresent([
            "staged_app_url": temporaryDirectory
                .appendingPathComponent("deleted.app").path,
            "bundle_identifier": "dev.ottam.Ottam.ui-test.fixture",
            "executable_path": temporaryDirectory
                .appendingPathComponent("deleted.app/Contents/MacOS/Ottam").path,
            "executable_sha256": String(repeating: "0", count: 64),
            "app_pid": "999999",
            "app_process_identity": "999999:1:1",
            "app_child_identities": "",
        ])

        XCTAssertEqual(
            result["status"] as? String,
            "attached_application_already_exited"
        )
        XCTAssertEqual(result["exited"] as? Bool, true)
        XCTAssertEqual(result["childrenExited"] as? Bool, true)
    }

    func testStaleOwnerRecoveryReleasesLease() throws {
        let database = try makeDatabase()
        let ticket = try enqueue(
            database,
            runID: "stale",
            processIdentity: "impossible-process-identity"
        )
        XCTAssertTrue(try database.tryAcquire(ticketID: ticket))
        let result = try XCTUnwrap(database.recoverStaleOwner())
        XCTAssertEqual(result["status"] as? String, "stale_owner_recovered")
        XCTAssertEqual(
            try database.ticketSnapshot(ticketID: ticket)["state"] as? String,
            "stale_recovered"
        )
    }

    func testConcurrentAcquisitionHasOneOwner() throws {
        let seed = try makeDatabase()
        let first = try enqueue(seed, runID: "first")
        let second = try enqueue(seed, runID: "second")
        let lock = NSLock()
        var acquired: [String] = []
        let group = DispatchGroup()

        for ticket in [first, second] {
            group.enter()
            DispatchQueue.global().async {
                defer { group.leave() }
                do {
                    let database = try self.makeDatabase()
                    if try database.tryAcquire(ticketID: ticket) {
                        lock.lock()
                        acquired.append(ticket)
                        lock.unlock()
                    }
                } catch {
                    XCTFail("concurrent acquisition failed: \(error)")
                }
            }
        }

        XCTAssertEqual(group.wait(timeout: .now() + 5), .success)
        XCTAssertEqual(acquired, [first])
    }

    private func makeDatabase() throws -> QueueDatabase {
        let database = try QueueDatabase(path: databaseURL.path)
        try database.initialize()
        return database
    }

    private func enqueue(
        _ database: QueueDatabase,
        runID: String,
        processIdentity: String? = nil,
        deadline: Date = Date().addingTimeInterval(60)
    ) throws -> String {
        let identity = processIdentity ?? currentProcessIdentity()
        let result = try database.enqueue(
            runnerPID: getpid(),
            processIdentity: identity,
            worktreeID: "test-worktree",
            runID: runID,
            scenario: "test-scenario",
            evidencePath: temporaryDirectory.path,
            deadline: deadline
        )
        return try XCTUnwrap(result["ticketID"] as? String)
    }

    private func currentProcessIdentity() -> String {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        XCTAssertEqual(
            proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, &info, size),
            size
        )
        return "\(getpid()):\(info.pbi_start_tvsec):\(info.pbi_start_tvusec)"
    }
}
