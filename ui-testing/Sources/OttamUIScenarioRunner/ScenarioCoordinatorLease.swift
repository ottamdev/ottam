import Foundation
import MacAutomationKit
import ScenarioKit

final class ScenarioCoordinatorLease: @unchecked Sendable {
    private let executable: String
    private let ticketID: String
    private let heartbeatQueue = DispatchQueue(
        label: "dev.ottam.ui-test-coordinator-heartbeat"
    )
    private let lock = NSLock()
    private var heartbeatTimer: DispatchSourceTimer?
    private var heartbeatError: Error?
    private var finished = false

    private init(executable: String, ticketID: String) {
        self.executable = executable
        self.ticketID = ticketID
    }

    static func acquire(
        executable: String,
        identity: IsolationIdentity,
        scenarioID: String,
        evidenceURL: URL,
        timeout: TimeInterval = 900
    ) throws -> ScenarioCoordinatorLease {
        guard !executable.isEmpty else {
            throw CoordinatorLeaseError.unavailable(
                "the UI-test coordinator executable was not provided"
            )
        }
        let enqueueOutput = try coordinatorOutput([
            executable,
            "enqueue",
            "--runner-pid", String(getpid()),
            "--worktree-id", identity.worktreeID,
            "--run-id", identity.runID,
            "--scenario", scenarioID,
            "--evidence", evidenceURL.path,
            "--queue-timeout", "\(timeout)s",
        ])
        guard let data = enqueueOutput.data(using: .utf8),
              let object = try JSONSerialization.jsonObject(with: data)
                as? [String: Any],
              let ticketID = object["ticketID"] as? String
        else {
            throw CoordinatorLeaseError.invalidResponse(enqueueOutput)
        }
        do {
            _ = try coordinatorOutput([
                executable,
                "wait",
                "--ticket", ticketID,
            ])
        } catch {
            if String(describing: error).contains("queue_timed_out") {
                throw CoordinatorLeaseError.timedOut(ticketID)
            }
            throw error
        }
        let lease = ScenarioCoordinatorLease(
            executable: executable,
            ticketID: ticketID
        )
        lease.startHeartbeat()
        return lease
    }

    func stage(_ application: StagedApplication) throws {
        try verifyHealthy()
        _ = try Self.coordinatorOutput([
            executable,
            "stage-app",
            "--ticket", ticketID,
            "--app-url", application.bundleURL.path,
            "--bundle-id", application.bundleIdentifier,
            "--executable", application.executableURL.path,
            "--executable-sha256", application.executableSHA256,
        ])
    }

    func attach(
        _ launched: LaunchedApplication,
        application: StagedApplication
    ) throws {
        try verifyHealthy()
        _ = try Self.coordinatorOutput([
            executable,
            "attach-app",
            "--ticket", ticketID,
            "--app-url", application.bundleURL.path,
            "--app-pid", String(launched.processIdentity.pid),
            "--bundle-id", application.bundleIdentifier,
            "--executable", application.executableURL.path,
            "--executable-sha256", application.executableSHA256,
        ])
    }

    func cleanupApplication() throws -> [String: Any] {
        try verifyHealthy()
        let output = try Self.coordinatorOutput([
            executable,
            "cleanup-app",
            "--ticket", ticketID,
        ])
        guard let data = output.data(using: .utf8),
              let object = try JSONSerialization.jsonObject(with: data)
                as? [String: Any]
        else {
            throw CoordinatorLeaseError.invalidResponse(output)
        }
        return object
    }

    func release() throws {
        lock.lock()
        if finished {
            lock.unlock()
            return
        }
        finished = true
        let timer = heartbeatTimer
        heartbeatTimer = nil
        let recordedHeartbeatError = heartbeatError
        lock.unlock()
        timer?.cancel()
        _ = try Self.coordinatorOutput([
            executable,
            "release",
            "--ticket", ticketID,
        ])
        if let recordedHeartbeatError {
            throw CoordinatorLeaseError.heartbeatFailed(
                String(describing: recordedHeartbeatError)
            )
        }
    }

    private func startHeartbeat() {
        let timer = DispatchSource.makeTimerSource(queue: heartbeatQueue)
        timer.schedule(deadline: .now() + 2, repeating: 2)
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            do {
                _ = try Self.coordinatorOutput([
                    executable,
                    "heartbeat",
                    "--ticket", ticketID,
                ])
            } catch {
                lock.lock()
                if heartbeatError == nil {
                    heartbeatError = error
                }
                lock.unlock()
            }
        }
        lock.lock()
        heartbeatTimer = timer
        lock.unlock()
        timer.resume()
    }

    private func verifyHealthy() throws {
        lock.lock()
        let error = heartbeatError
        lock.unlock()
        if let error {
            throw CoordinatorLeaseError.heartbeatFailed(
                String(describing: error)
            )
        }
    }

    private static func coordinatorOutput(_ arguments: [String]) throws -> String {
        do {
            return try Command.output(arguments, timeout: 920)
        } catch {
            throw CoordinatorLeaseError.commandFailed(String(describing: error))
        }
    }
}

enum CoordinatorLeaseError: Error, CustomStringConvertible {
    case unavailable(String)
    case invalidResponse(String)
    case timedOut(String)
    case heartbeatFailed(String)
    case commandFailed(String)

    var description: String {
        switch self {
        case let .unavailable(message):
            "coordinator unavailable: \(message)"
        case let .invalidResponse(response):
            "invalid coordinator response: \(response)"
        case let .timedOut(ticketID):
            "queue_timed_out: \(ticketID)"
        case let .heartbeatFailed(message):
            "coordinator heartbeat failed: \(message)"
        case let .commandFailed(message):
            "coordinator command failed: \(message)"
        }
    }
}
