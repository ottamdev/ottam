import Foundation
import SQLite3

private let sqliteTransient = unsafeBitCast(
    -1,
    to: sqlite3_destructor_type.self
)

final class QueueDatabase {
    private var handle: OpaquePointer?

    init(path: String) throws {
        let url = URL(fileURLWithPath: path).standardizedFileURL
        let parent = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(
            at: parent,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        guard sqlite3_open_v2(
            url.path,
            &handle,
            SQLITE_OPEN_CREATE | SQLITE_OPEN_READWRITE | SQLITE_OPEN_FULLMUTEX,
            nil
        ) == SQLITE_OK else {
            throw CoordinatorError.sqlite("open: \(message)")
        }
        sqlite3_busy_timeout(handle, 5_000)
        try? FileManager.default.setAttributes(
            [.posixPermissions: 0o600],
            ofItemAtPath: url.path
        )
    }

    deinit {
        sqlite3_close(handle)
    }

    func initialize() throws {
        try execute("PRAGMA journal_mode=WAL")
        try execute("PRAGMA foreign_keys=ON")
        try execute("""
            CREATE TABLE IF NOT EXISTS tickets (
              sequence INTEGER PRIMARY KEY AUTOINCREMENT,
              ticket_id TEXT NOT NULL UNIQUE,
              runner_pid INTEGER NOT NULL,
              process_identity TEXT NOT NULL,
              worktree_id TEXT NOT NULL,
              run_id TEXT NOT NULL,
              scenario TEXT NOT NULL,
              evidence_path TEXT NOT NULL,
              state TEXT NOT NULL,
              enqueued_at REAL NOT NULL,
              acquired_at REAL,
              heartbeat_at REAL,
              released_at REAL,
              deadline REAL NOT NULL,
              staged_app_url TEXT,
              app_pid INTEGER,
              app_process_identity TEXT,
              app_child_identities TEXT,
              bundle_identifier TEXT,
              executable_path TEXT,
              executable_sha256 TEXT
            )
            """)
        try ensureColumn(
            "app_process_identity",
            definition: "TEXT"
        )
        try ensureColumn(
            "app_child_identities",
            definition: "TEXT"
        )
        try execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS one_acquired_ticket "
                + "ON tickets(state) WHERE state = 'acquired'"
        )
    }

    func enqueue(
        runnerPID: pid_t,
        processIdentity: String,
        worktreeID: String,
        runID: String,
        scenario: String,
        evidencePath: String,
        deadline: Date
    ) throws -> [String: Any] {
        let ticketID = UUID().uuidString.lowercased()
        let now = Date().timeIntervalSince1970
        try execute(
            """
            INSERT INTO tickets
              (ticket_id, runner_pid, process_identity, worktree_id, run_id,
               scenario, evidence_path, state, enqueued_at, heartbeat_at, deadline)
            VALUES (?, ?, ?, ?, ?, ?, ?, 'waiting', ?, ?, ?)
            """,
            [
                ticketID, Int64(runnerPID), processIdentity, worktreeID, runID,
                scenario, evidencePath, now, now, deadline.timeIntervalSince1970,
            ]
        )
        return try ticketSnapshot(ticketID: ticketID)
    }

    func tryAcquire(ticketID: String) throws -> Bool {
        try transaction {
            if try scalarInt("SELECT COUNT(*) FROM tickets WHERE state = 'acquired'") > 0 {
                return false
            }
            guard try scalarText(
                "SELECT ticket_id FROM tickets WHERE state = 'waiting' "
                    + "ORDER BY sequence LIMIT 1"
            ) == ticketID else {
                return false
            }
            let now = Date().timeIntervalSince1970
            try execute(
                """
                UPDATE tickets
                SET state = 'acquired', acquired_at = ?, heartbeat_at = ?
                WHERE ticket_id = ? AND state = 'waiting' AND deadline > ?
                """,
                [now, now, ticketID, now]
            )
            return sqlite3_changes(handle) == 1
        }
    }

    func heartbeat(ticketID: String) throws {
        let snapshot = try ticketSnapshot(ticketID: ticketID)
        guard snapshot["state"] as? String == "acquired" else {
            throw CoordinatorError.invalidState(snapshot["state"] as? String ?? "missing")
        }
        let expectedIdentity = snapshot["processIdentity"] as? String
        let runnerPID = pid_t(snapshot["runnerPID"] as? Int ?? 0)
        guard ProcessIdentity.forPID(runnerPID) == expectedIdentity else {
            throw CoordinatorError.runnerIdentityMismatch(runnerPID)
        }
        try execute(
            "UPDATE tickets SET heartbeat_at = ? "
                + "WHERE ticket_id = ? AND state = 'acquired'",
            [Date().timeIntervalSince1970, ticketID]
        )
        try refreshAttachedChildren(ticketID: ticketID)
    }

    func attachApp(
        ticketID: String,
        appURL: String,
        appPID: pid_t,
        bundleIdentifier: String,
        executablePath: String,
        executableSHA256: String
    ) throws {
        let state = try scalarText(
            "SELECT state FROM tickets WHERE ticket_id = ?",
            [ticketID]
        )
        guard state == "acquired" else {
            throw CoordinatorError.invalidState(state ?? "missing")
        }
        guard let appProcessIdentity = ProcessIdentity.forPID(appPID) else {
            throw CoordinatorError.applicationNotAlive(appPID)
        }
        try OwnedApplication.validate(
            appURL: appURL,
            appPID: appPID,
            bundleIdentifier: bundleIdentifier,
            executablePath: executablePath,
            executableSHA256: executableSHA256
        )
        var children = ProcessIdentity.descendants(of: appPID)
        children.formUnion(OwnedApplication.bundledHelperProcesses(appURL: appURL))
        let childIdentities = try OwnedApplication.encodeChildren(children)
        try execute(
            """
            UPDATE tickets SET
              staged_app_url = ?, app_pid = ?, app_process_identity = ?,
              app_child_identities = ?, bundle_identifier = ?,
              executable_path = ?, executable_sha256 = ?
            WHERE ticket_id = ? AND state = 'acquired'
            """,
            [
                appURL, Int64(appPID), appProcessIdentity, childIdentities,
                bundleIdentifier, executablePath, executableSHA256, ticketID,
            ]
        )
    }

    func stageApp(
        ticketID: String,
        appURL: String,
        bundleIdentifier: String,
        executablePath: String,
        executableSHA256: String
    ) throws {
        let state = try scalarText(
            "SELECT state FROM tickets WHERE ticket_id = ?",
            [ticketID]
        )
        guard state == "acquired" else {
            throw CoordinatorError.invalidState(state ?? "missing")
        }
        try OwnedApplication.validateStagedBundle(
            appURL: appURL,
            bundleIdentifier: bundleIdentifier,
            executablePath: executablePath,
            executableSHA256: executableSHA256
        )
        try execute(
            """
            UPDATE tickets SET
              staged_app_url = ?, bundle_identifier = ?,
              executable_path = ?, executable_sha256 = ?
            WHERE ticket_id = ? AND state = 'acquired'
            """,
            [
                appURL, bundleIdentifier, executablePath, executableSHA256,
                ticketID,
            ]
        )
    }

    func cleanupAttachedApp(ticketID: String) throws -> [String: Any] {
        let snapshot = try ticketSnapshot(ticketID: ticketID)
        guard snapshot["state"] as? String == "acquired" else {
            throw CoordinatorError.invalidState(snapshot["state"] as? String ?? "missing")
        }
        let runnerPID = pid_t(snapshot["runnerPID"] as? Int ?? 0)
        guard ProcessIdentity.forPID(runnerPID) == snapshot["processIdentity"] as? String else {
            throw CoordinatorError.runnerIdentityMismatch(runnerPID)
        }
        guard let row = try firstRow(
            """
            SELECT staged_app_url, app_pid, app_process_identity,
                   app_child_identities, bundle_identifier, executable_path,
                   executable_sha256
            FROM tickets WHERE ticket_id = ? AND state = 'acquired'
            """,
            [ticketID]
        ) else {
            throw CoordinatorError.ticketNotFound(ticketID)
        }
        return try OwnedApplication.cleanupIfPresent(row)
    }

    private func refreshAttachedChildren(ticketID: String) throws {
        guard let row = try firstRow(
            """
            SELECT staged_app_url, app_pid, app_process_identity,
                   app_child_identities
            FROM tickets WHERE ticket_id = ? AND state = 'acquired'
            """,
            [ticketID]
        ), let appPID = pid_t(row["app_pid"] ?? ""),
        ProcessIdentity.forPID(appPID) == row["app_process_identity"]
        else {
            return
        }
        var children = try OwnedApplication.decodeChildren(
            row["app_child_identities"] ?? ""
        )
        children.formUnion(ProcessIdentity.descendants(of: appPID))
        if let appURL = row["staged_app_url"] {
            children.formUnion(OwnedApplication.bundledHelperProcesses(appURL: appURL))
        }
        try execute(
            "UPDATE tickets SET app_child_identities = ? "
                + "WHERE ticket_id = ? AND state = 'acquired'",
            [try OwnedApplication.encodeChildren(children), ticketID]
        )
    }

    func finish(ticketID: String, expectedState: String, newState: String) throws {
        try execute(
            "UPDATE tickets SET state = ?, released_at = ? "
                + "WHERE ticket_id = ? AND state = ?",
            [newState, Date().timeIntervalSince1970, ticketID, expectedState]
        )
        guard sqlite3_changes(handle) == 1 else {
            let state = try scalarText(
                "SELECT state FROM tickets WHERE ticket_id = ?",
                [ticketID]
            )
            throw CoordinatorError.invalidTransition(state ?? "missing", newState)
        }
    }

    func expireWaitingTickets() throws {
        for row in try rows(
            "SELECT ticket_id, runner_pid, process_identity FROM tickets "
                + "WHERE state = 'waiting'"
        ) {
            guard let pid = pid_t(row["runner_pid"] ?? ""),
                  ProcessIdentity.forPID(pid) != row["process_identity"]
            else { continue }
            try execute(
                "UPDATE tickets SET state = 'cancelled', released_at = ? "
                    + "WHERE ticket_id = ? AND state = 'waiting'",
                [Date().timeIntervalSince1970, row["ticket_id"] ?? ""]
            )
        }
        try execute(
            "UPDATE tickets SET state = 'timed_out', released_at = ? "
                + "WHERE state = 'waiting' AND deadline <= ?",
            [Date().timeIntervalSince1970, Date().timeIntervalSince1970]
        )
    }

    func recoverStaleOwner() throws -> [String: Any]? {
        guard let row = try firstRow(
            """
            SELECT ticket_id, runner_pid, process_identity, heartbeat_at,
                   staged_app_url, app_pid, app_process_identity,
                   app_child_identities, bundle_identifier, executable_path,
                   executable_sha256
            FROM tickets WHERE state = 'acquired' ORDER BY sequence LIMIT 1
            """
        ), let pid = pid_t(row["runner_pid"] ?? "") else {
            return nil
        }
        let recordedIdentity = row["process_identity"] ?? ""
        if ProcessIdentity.forPID(pid) == recordedIdentity {
            return nil
        }
        let ticketID = row["ticket_id"] ?? ""
        let cleanup = try OwnedApplication.cleanupIfPresent(row)
        try execute(
            "UPDATE tickets SET state = 'stale_recovered', released_at = ? "
                + "WHERE ticket_id = ? AND state = 'acquired'",
            [Date().timeIntervalSince1970, ticketID]
        )
        guard sqlite3_changes(handle) == 1 else { return nil }
        return [
            "status": "stale_owner_recovered",
            "ticketID": ticketID,
            "runnerPID": Int(pid),
            "recordedProcessIdentity": recordedIdentity,
            "observedProcessIdentity": ProcessIdentity.forPID(pid) ?? NSNull(),
            "lastHeartbeatAt": Double(row["heartbeat_at"] ?? "") ?? NSNull(),
            "ownedApplicationCleanup": cleanup,
        ]
    }

    func ticketSnapshot(ticketID: String) throws -> [String: Any] {
        guard let row = try firstRow(
            """
            SELECT sequence, ticket_id, runner_pid, process_identity, worktree_id,
                   run_id, scenario, evidence_path, state, enqueued_at, acquired_at,
                   heartbeat_at, released_at, deadline, staged_app_url, app_pid,
                   app_process_identity, app_child_identities, bundle_identifier,
                   executable_path, executable_sha256
            FROM tickets WHERE ticket_id = ?
            """,
            [ticketID]
        ) else {
            throw CoordinatorError.ticketNotFound(ticketID)
        }
        return ticketDictionary(row)
    }

    func position(ticketID: String) throws -> Int {
        guard let sequence = try scalarIntOptional(
            "SELECT sequence FROM tickets WHERE ticket_id = ? AND state = 'waiting'",
            [ticketID]
        ) else {
            return 0
        }
        return try scalarInt(
            "SELECT COUNT(*) FROM tickets WHERE state = 'waiting' AND sequence <= ?",
            [sequence]
        )
    }

    func ownerSummary() throws -> [String: Any]? {
        guard let row = try firstRow(
            """
            SELECT sequence, ticket_id, runner_pid, process_identity, worktree_id,
                   run_id, scenario, evidence_path, state, enqueued_at, acquired_at,
                   heartbeat_at, released_at, deadline, staged_app_url, app_pid,
                   app_process_identity, app_child_identities, bundle_identifier,
                   executable_path, executable_sha256
            FROM tickets WHERE state = 'acquired' ORDER BY sequence LIMIT 1
            """
        ) else {
            return nil
        }
        return ticketDictionary(row)
    }

    func status() throws -> [String: Any] {
        let waitingRows = try rows(
            """
            SELECT sequence, ticket_id, runner_pid, process_identity, worktree_id,
                   run_id, scenario, evidence_path, state, enqueued_at, acquired_at,
                   heartbeat_at, released_at, deadline, staged_app_url, app_pid,
                   app_process_identity, app_child_identities, bundle_identifier,
                   executable_path, executable_sha256
            FROM tickets WHERE state = 'waiting' ORDER BY sequence
            """
        )
        return [
            "schemaVersion": 1,
            "journalMode": try scalarText("PRAGMA journal_mode") ?? "unknown",
            "owner": try ownerSummary() ?? NSNull(),
            "waiting": waitingRows.map(ticketDictionary),
        ]
    }

    private func ticketDictionary(_ row: [String: String]) -> [String: Any] {
        func number(_ key: String) -> Any {
            guard let raw = row[key], !raw.isEmpty else { return NSNull() }
            return Double(raw) ?? NSNull()
        }
        return [
            "sequence": Int(row["sequence"] ?? "") ?? 0,
            "ticketID": row["ticket_id"] ?? "",
            "runnerPID": Int(row["runner_pid"] ?? "") ?? 0,
            "processIdentity": row["process_identity"] ?? "",
            "worktreeID": row["worktree_id"] ?? "",
            "runID": row["run_id"] ?? "",
            "scenario": row["scenario"] ?? "",
            "evidencePath": row["evidence_path"] ?? "",
            "state": row["state"] ?? "",
            "enqueuedAt": number("enqueued_at"),
            "acquiredAt": number("acquired_at"),
            "heartbeatAt": number("heartbeat_at"),
            "releasedAt": number("released_at"),
            "deadline": number("deadline"),
            "stagedAppURL": row["staged_app_url"].flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
            "appPID": Int(row["app_pid"] ?? "") ?? NSNull(),
            "appProcessIdentity":
                row["app_process_identity"].flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
            "appChildIdentities": (try? OwnedApplication.childDictionaries(
                row["app_child_identities"] ?? ""
            )) ?? [],
            "bundleIdentifier": row["bundle_identifier"].flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
            "executablePath": row["executable_path"].flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
            "executableSHA256": row["executable_sha256"].flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
        ]
    }

    private func transaction<T>(_ operation: () throws -> T) throws -> T {
        try execute("BEGIN IMMEDIATE")
        do {
            let value = try operation()
            try execute("COMMIT")
            return value
        } catch {
            try? execute("ROLLBACK")
            throw error
        }
    }

    private func ensureColumn(_ name: String, definition: String) throws {
        let existing = try rows("PRAGMA table_info(tickets)")
            .contains { $0["name"] == name }
        if !existing {
            try execute("ALTER TABLE tickets ADD COLUMN \(name) \(definition)")
        }
    }

    private func execute(_ sql: String, _ bindings: [Any] = []) throws {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK else {
            throw CoordinatorError.sqlite(message)
        }
        defer { sqlite3_finalize(statement) }
        try bind(bindings, to: statement)
        let result = sqlite3_step(statement)
        guard result == SQLITE_DONE || result == SQLITE_ROW else {
            throw CoordinatorError.sqlite(message)
        }
    }

    private func scalarInt(_ sql: String, _ bindings: [Any] = []) throws -> Int {
        try scalarIntOptional(sql, bindings) ?? 0
    }

    private func scalarIntOptional(_ sql: String, _ bindings: [Any] = []) throws -> Int? {
        guard let value = try scalarText(sql, bindings) else { return nil }
        return Int(value)
    }

    private func scalarText(_ sql: String, _ bindings: [Any] = []) throws -> String? {
        try firstRow(sql, bindings)?.values.first
    }

    private func firstRow(
        _ sql: String,
        _ bindings: [Any] = []
    ) throws -> [String: String]? {
        try rows(sql, bindings).first
    }

    private func rows(_ sql: String, _ bindings: [Any] = []) throws -> [[String: String]] {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK else {
            throw CoordinatorError.sqlite(message)
        }
        defer { sqlite3_finalize(statement) }
        try bind(bindings, to: statement)
        var result: [[String: String]] = []
        var stepResult = sqlite3_step(statement)
        while stepResult == SQLITE_ROW {
            var row: [String: String] = [:]
            for column in 0..<sqlite3_column_count(statement) {
                let name = String(cString: sqlite3_column_name(statement, column))
                if let text = sqlite3_column_text(statement, column) {
                    row[name] = String(cString: text)
                } else {
                    row[name] = ""
                }
            }
            result.append(row)
            stepResult = sqlite3_step(statement)
        }
        guard stepResult == SQLITE_DONE else {
            throw CoordinatorError.sqlite(message)
        }
        return result
    }

    private func bind(_ values: [Any], to statement: OpaquePointer?) throws {
        for (offset, value) in values.enumerated() {
            let index = Int32(offset + 1)
            let result: Int32
            switch value {
            case let value as String:
                result = sqlite3_bind_text(statement, index, value, -1, sqliteTransient)
            case let value as Int64:
                result = sqlite3_bind_int64(statement, index, value)
            case let value as Int:
                result = sqlite3_bind_int64(statement, index, Int64(value))
            case let value as Double:
                result = sqlite3_bind_double(statement, index, value)
            default:
                throw CoordinatorError.sqlite("unsupported binding \(value)")
            }
            guard result == SQLITE_OK else {
                throw CoordinatorError.sqlite(message)
            }
        }
    }

    private var message: String {
        guard let handle, let text = sqlite3_errmsg(handle) else { return "unknown" }
        return String(cString: text)
    }
}
