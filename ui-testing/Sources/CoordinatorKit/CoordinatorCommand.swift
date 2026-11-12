import Darwin
import Foundation

public enum CoordinatorCommand {
    public static func main() {
        do {
            let arguments = Arguments(CommandLine.arguments)
            guard let command = arguments.command else {
                throw CoordinatorError.usage
            }
            let database = try QueueDatabase(path: arguments.databasePath)
            try database.initialize()

            switch command {
            case "enqueue":
                let runnerPID = try arguments.requiredPID("--runner-pid")
                guard let processIdentity = ProcessIdentity.forPID(runnerPID) else {
                    throw CoordinatorError.runnerNotAlive(runnerPID)
                }
                let timeout = try arguments.duration("--queue-timeout", default: 900)
                let ticket = try database.enqueue(
                    runnerPID: runnerPID,
                    processIdentity: processIdentity,
                    worktreeID: try arguments.required("--worktree-id"),
                    runID: try arguments.required("--run-id"),
                    scenario: try arguments.required("--scenario"),
                    evidencePath: try arguments.required("--evidence"),
                    deadline: Date().addingTimeInterval(timeout)
                )
                try printJSON(ticket)
            case "wait":
                let ticketID = try arguments.required("--ticket")
                let pollInterval = try arguments.duration("--poll-interval", default: 0.2)
                try waitForLease(
                    ticketID: ticketID,
                    database: database,
                    pollInterval: pollInterval
                )
            case "heartbeat":
                try database.heartbeat(ticketID: try arguments.required("--ticket"))
                try printJSON(["status": "heartbeat_recorded"])
            case "stage-app":
                try database.stageApp(
                    ticketID: try arguments.required("--ticket"),
                    appURL: try arguments.required("--app-url"),
                    bundleIdentifier: try arguments.required("--bundle-id"),
                    executablePath: try arguments.required("--executable"),
                    executableSHA256: try arguments.required("--executable-sha256")
                )
                try printJSON(["status": "app_staged"])
            case "attach-app":
                try database.attachApp(
                    ticketID: try arguments.required("--ticket"),
                    appURL: try arguments.required("--app-url"),
                    appPID: try arguments.requiredPID("--app-pid"),
                    bundleIdentifier: try arguments.required("--bundle-id"),
                    executablePath: try arguments.required("--executable"),
                    executableSHA256: try arguments.required("--executable-sha256")
                )
                try printJSON(["status": "app_attached"])
            case "cleanup-app":
                let result = try database.cleanupAttachedApp(
                    ticketID: try arguments.required("--ticket")
                )
                try printJSON(result)
            case "release":
                try database.finish(
                    ticketID: try arguments.required("--ticket"),
                    expectedState: "acquired",
                    newState: "released"
                )
                try printJSON(["status": "released"])
            case "cancel":
                try database.finish(
                    ticketID: try arguments.required("--ticket"),
                    expectedState: "waiting",
                    newState: "cancelled"
                )
                try printJSON(["status": "cancelled"])
            case "recover":
                let result = try database.recoverStaleOwner()
                try printJSON(result ?? ["status": "no_stale_owner"])
            case "status":
                let status = try database.status()
                if arguments.contains("--json") {
                    try printJSON(status)
                } else {
                    printHumanStatus(status)
                }
            default:
                throw CoordinatorError.usage
            }
        } catch {
            let payload: [String: Any] = [
                "status": "error",
                "message": String(describing: error),
            ]
            if let data = try? JSONSerialization.data(
                withJSONObject: payload,
                options: [.prettyPrinted, .sortedKeys]
            ) {
                FileHandle.standardError.write(data + Data([0x0A]))
            }
            exit(error is CoordinatorError ? 2 : 1)
        }
    }

    private static func waitForLease(
        ticketID: String,
        database: QueueDatabase,
        pollInterval: TimeInterval
    ) throws {
        var lastPosition: Int?
        while true {
            try database.expireWaitingTickets()
            if let recovery = try database.recoverStaleOwner() {
                try printJSON(recovery, to: .standardError)
            }
            let snapshot = try database.ticketSnapshot(ticketID: ticketID)
            guard let state = snapshot["state"] as? String else {
                throw CoordinatorError.ticketNotFound(ticketID)
            }
            switch state {
            case "waiting":
                guard let runnerPID = snapshot["runnerPID"] as? Int,
                      let expectedIdentity = snapshot["processIdentity"] as? String,
                      ProcessIdentity.forPID(pid_t(runnerPID)) == expectedIdentity
                else {
                    try database.finish(
                        ticketID: ticketID,
                        expectedState: "waiting",
                        newState: "cancelled"
                    )
                    throw CoordinatorError.runnerIdentityMismatch(
                        pid_t(snapshot["runnerPID"] as? Int ?? 0)
                    )
                }
                let position = try database.position(ticketID: ticketID)
                if position != lastPosition {
                    let owner = try database.ownerSummary()
                    try printJSON([
                        "status": "waiting",
                        "ticketID": ticketID,
                        "position": position,
                        "owner": owner.map { $0 as Any } ?? NSNull(),
                    ], to: .standardError)
                    lastPosition = position
                }
                if try database.tryAcquire(ticketID: ticketID) {
                    try printJSON(try database.ticketSnapshot(ticketID: ticketID))
                    return
                }
            case "acquired":
                try printJSON(snapshot)
                return
            case "timed_out":
                throw CoordinatorError.queueTimedOut(ticketID)
            case "cancelled":
                throw CoordinatorError.cancelled(ticketID)
            default:
                throw CoordinatorError.invalidState(state)
            }
            Thread.sleep(forTimeInterval: max(0.05, min(pollInterval, 2)))
        }
    }

    private static func printJSON(
        _ value: Any,
        to handle: FileHandle = .standardOutput
    ) throws {
        let data = try JSONSerialization.data(
            withJSONObject: value,
            options: [.prettyPrinted, .sortedKeys]
        )
        handle.write(data + Data([0x0A]))
    }

    private static func printHumanStatus(_ value: [String: Any]) {
        let owner = value["owner"] as? [String: Any]
        if let owner {
            print(
                "owner #\(owner["sequence"] ?? "?") "
                    + "\(owner["scenario"] ?? "?") "
                    + "pid \(owner["runnerPID"] ?? "?")"
            )
        } else {
            print("owner: none")
        }
        let waiting = value["waiting"] as? [[String: Any]] ?? []
        for (offset, ticket) in waiting.enumerated() {
            print(
                "\(offset + 1). #\(ticket["sequence"] ?? "?") "
                    + "\(ticket["scenario"] ?? "?") "
                    + "pid \(ticket["runnerPID"] ?? "?")"
            )
        }
        if waiting.isEmpty {
            print("waiting: none")
        }
    }
}

private struct Arguments {
    let values: [String]

    init(_ values: [String]) {
        self.values = values
    }

    var command: String? {
        values.indices.contains(1) ? values[1] : nil
    }

    var databasePath: String {
        if let value = value(after: "--database") {
            return URL(fileURLWithPath: value).standardizedFileURL.path
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Caches/dev.ottam.ui-test-coordinator")
            .appendingPathComponent("queue.sqlite")
            .path
    }

    func contains(_ flag: String) -> Bool {
        values.contains(flag)
    }

    func value(after flag: String) -> String? {
        guard let index = values.firstIndex(of: flag),
              values.indices.contains(index + 1)
        else {
            return nil
        }
        return values[index + 1]
    }

    func required(_ flag: String) throws -> String {
        guard let value = value(after: flag), !value.isEmpty else {
            throw CoordinatorError.missing(flag)
        }
        return value
    }

    func requiredPID(_ flag: String) throws -> pid_t {
        let value = try required(flag)
        guard let pid = pid_t(value), pid > 1 else {
            throw CoordinatorError.invalidValue(flag, value)
        }
        return pid
    }

    func duration(_ flag: String, default defaultValue: TimeInterval) throws -> TimeInterval {
        guard let raw = value(after: flag) else { return defaultValue }
        let multiplier: Double
        let number: String
        if raw.hasSuffix("ms") {
            multiplier = 0.001
            number = String(raw.dropLast(2))
        } else if raw.hasSuffix("s") {
            multiplier = 1
            number = String(raw.dropLast())
        } else if raw.hasSuffix("m") {
            multiplier = 60
            number = String(raw.dropLast())
        } else {
            multiplier = 1
            number = raw
        }
        guard let value = Double(number), value > 0 else {
            throw CoordinatorError.invalidValue(flag, raw)
        }
        return value * multiplier
    }
}
