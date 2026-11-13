import Foundation
import Darwin
import OSLog

public enum CoreProcessLifecycleEvent: Sendable {
    case starting
    case ready(InitializeResult)
    case terminated(status: Int32, error: CoreExitError?)
}

public actor LocalProcessConnection: AgentConnection {
    private static let processLogger = Logger(
        subsystem: "dev.ottam.Ottam",
        category: "BackendProcess"
    )

    private let executableURL: URL
    private let maximumMessageBytes: Int
    private var process: Process?
    private var connection: AsyncJSONLConnection?
    private var startupTask: Task<AsyncJSONLConnection, Error>?
    private var initializationTask: Task<Void, Error>?
    private var latestInitializeResult: InitializeResult?
    private var stderrCaptureTask: Task<Void, Never>?
    private var currentLogPaths: BackendProcessLogPaths?
    private var notificationHandler: (@Sendable (AgentNotification) async -> Void)?
    private var clientRequestHandler: AgentClientRequestHandler?
    private var lifecycleHandler: (@Sendable (CoreProcessLifecycleEvent) async -> Void)?
    private let refreshProtection = CoreRefreshProtection()

    public init(
        executableURL: URL,
        maximumMessageBytes: Int = 16 * 1024 * 1024,
        notificationHandler: (@Sendable (AgentNotification) async -> Void)? = nil,
        clientRequestHandler: AgentClientRequestHandler? = nil
    ) {
        self.executableURL = executableURL
        self.maximumMessageBytes = maximumMessageBytes
        self.notificationHandler = notificationHandler
        self.clientRequestHandler = clientRequestHandler
    }

    deinit {
        if let process {
            Self.terminateAndReap(process, protection: refreshProtection)
        }
    }

    public func request<Params: Encodable & Sendable, Result: Decodable & Sendable>(
        method: String,
        params: Params
    ) async throws -> Result {
        let connection = try await ensureStartedAndInitialized()
        let result: Result = try await connection.request(method: method, params: params)
        if let readiness = result as? AgentShutdownReadinessResult {
            refreshProtection.update(paths: readiness.refreshLockPaths, hasSharedHomes: readiness.hasSharedHomes)
        } else if ["agent.list", "engine.list", "agent.create", "agent.setLayout"].contains(method),
                  let readiness: AgentShutdownReadinessResult = try? await connection.request(method: "agent.shutdownReadiness", params: AgentListParams()) {
            refreshProtection.update(paths: readiness.refreshLockPaths, hasSharedHomes: readiness.hasSharedHomes)
        }
        return result
    }

    public func notify<Params: Codable & Sendable>(
        method: String,
        params: Params
    ) async throws {
        let connection = try await ensureStarted()
        try await connection.notify(method: method, params: params)
    }

    public func setNotificationHandler(_ handler: (@Sendable (AgentNotification) async -> Void)?) async {
        notificationHandler = handler
        await connection?.setNotificationHandler(handler)
    }

    public func setClientRequestHandler(_ handler: AgentClientRequestHandler?) async {
        clientRequestHandler = handler
        await connection?.setClientRequestHandler(handler)
    }

    public func setLifecycleHandler(
        _ handler: (@Sendable (CoreProcessLifecycleEvent) async -> Void)?
    ) {
        lifecycleHandler = handler
    }

    public func close() async {
        guard await prepareToClose() else { return }
        await connection?.close()
        connection = nil
        initializationTask = nil
        if let process {
            Self.terminateAndReap(process, protection: refreshProtection)
        }
        process = nil
        stderrCaptureTask?.cancel()
        stderrCaptureTask = nil
        currentLogPaths = nil
        latestInitializeResult = nil
    }

    public func initializeResult() async throws -> InitializeResult {
        _ = try await ensureStartedAndInitialized()
        guard let latestInitializeResult else {
            throw AgentConnectionError.invalidResponse("initialize result was not retained")
        }
        return latestInitializeResult
    }

    private func ensureStartedAndInitialized() async throws -> AsyncJSONLConnection {
        let connection = try await ensureStarted()
        if let initializationTask {
            try await initializationTask.value
            return connection
        }

        let task = Task {
            Self.processLogger.info("initializing backend protocol")
            let result = try await connection.initialize()
            try await connection.sendInitialized()
            self.latestInitializeResult = result
            await self.lifecycleHandler?(.ready(result))
            Self.processLogger.info("backend protocol initialized")
        }
        initializationTask = task

        do {
            try await task.value
        } catch {
            let annotatedError = annotatedStartupFailure(error)
            Self.processLogger.error("backend initialization failed: \(annotatedError.localizedDescription, privacy: .private)")
            _ = await resetConnection()
            throw annotatedError
        }
        return connection
    }

    private func ensureStarted() async throws -> AsyncJSONLConnection {
        if let process, process.isRunning, let connection {
            if await connection.isActive {
                return connection
            }
            guard await resetConnection() else {
                throw AgentConnectionError.failedToStart("The core connection is unavailable. Its live adapters have been preserved; retry after Claude finishes refreshing sign-in.")
            }
        }

        // Actor reentrancy: startProcess suspends before self.process is assigned,
        // so concurrent callers must share one spawn. A second spawn would race the
        // first for the OTTAM_HOME lock and orphan the winner (which then holds
        // the lock forever while every replacement dies with coreAlreadyRunning).
        if let startupTask {
            return try await startupTask.value
        }
        let task = Task {
            defer { self.startupTask = nil }
            return try await self.startProcess()
        }
        startupTask = task
        return try await task.value
    }

    private func startProcess() async throws -> AsyncJSONLConnection {
        guard FileManager.default.isExecutableFile(atPath: executableURL.path) else {
            Self.processLogger.error("backend executable missing at \(self.executableURL.path, privacy: .private)")
            throw AgentConnectionError.executableNotFound(executableURL.path)
        }

        let logPaths = try Self.prepareBackendLogPaths()
        let process = Process()
        let inputPipe = Pipe()
        let outputPipe = Pipe()
        let stderrPipe = Pipe()

        process.executableURL = executableURL
        process.standardInput = inputPipe
        process.standardOutput = outputPipe
        process.standardError = stderrPipe
        var environment = ProcessInfo.processInfo.environment
        if environment["TERMINFO"]?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true,
           let terminfoURL = Bundle.main.resourceURL?.appendingPathComponent("terminfo", isDirectory: true),
           FileManager.default.fileExists(atPath: terminfoURL.path) {
            environment["TERMINFO"] = terminfoURL.path
        }
        environment["OTTAM_CORE_LOG_FILE"] = logPaths.traceLogURL.path
        environment["OTTAM_CORE_STDERR_LOG_FILE"] = logPaths.stderrLogURL.path
        process.environment = environment
        process.terminationHandler = { [weak self] terminatedProcess in
            Self.processLogger.info(
                "backend process exited status=\(terminatedProcess.terminationStatus, privacy: .public) traceLog=\(logPaths.traceLogURL.path, privacy: .private) stderrLog=\(logPaths.stderrLogURL.path, privacy: .private)"
            )
            Task {
                await self?.processDidTerminate(
                    pid: terminatedProcess.processIdentifier,
                    status: terminatedProcess.terminationStatus
                )
            }
        }

        Self.processLogger.info(
            "starting backend executable=\(self.executableURL.path, privacy: .private) traceLog=\(logPaths.traceLogURL.path, privacy: .private) stderrLog=\(logPaths.stderrLogURL.path, privacy: .private)"
        )
        await lifecycleHandler?(.starting)

        do {
            try process.run()
        } catch {
            Self.processLogger.error("failed to start backend: \(error.localizedDescription, privacy: .private)")
            throw AgentConnectionError.failedToStart(
                Self.diagnosticMessage(
                    "process launch failed: \(error.localizedDescription)",
                    logPaths: logPaths
                )
            )
        }

        let stderrTask = Self.captureBackendStderr(
            from: stderrPipe.fileHandleForReading,
            to: logPaths.stderrLogURL
        )
        let transport = ProcessJSONLTransport(
            inputHandle: inputPipe.fileHandleForWriting,
            outputHandle: outputPipe.fileHandleForReading
        )
        let connection = AsyncJSONLConnection(
            transport: transport,
            maximumMessageBytes: maximumMessageBytes
        )
        await connection.setNotificationHandler(notificationHandler)
        await connection.setClientRequestHandler(clientRequestHandler)

        self.process = process
        self.connection = connection
        self.stderrCaptureTask = stderrTask
        self.currentLogPaths = logPaths
        initializationTask = nil
        Self.processLogger.info("backend process started pid=\(process.processIdentifier, privacy: .public)")
        return connection
    }

    private func processDidTerminate(pid: Int32, status: Int32) async {
        guard process?.processIdentifier == pid else { return }
        let terminatedConnection = connection
        await terminatedConnection?.waitForReadCompletion()
        let exitError = await terminatedConnection?.coreExitError
        guard process?.processIdentifier == pid else { return }
        connection = nil
        initializationTask = nil
        process = nil
        stderrCaptureTask?.cancel()
        stderrCaptureTask = nil
        currentLogPaths = nil
        latestInitializeResult = nil
        await lifecycleHandler?(.terminated(status: status, error: exitError))
    }

    private func resetConnection() async -> Bool {
        guard await prepareToClose() else { return false }
        await connection?.close()
        connection = nil
        initializationTask = nil
        if let process {
            Self.terminateAndReap(process, protection: refreshProtection)
        }
        process = nil
        stderrCaptureTask?.cancel()
        stderrCaptureTask = nil
        currentLogPaths = nil
        latestInitializeResult = nil
        return true
    }

    /// Quit must not start a core that has failed startup or never ran.
    public func shutdownReadinessIfRunning() async throws -> AgentShutdownReadinessResult? {
        guard latestInitializeResult != nil, let process, process.isRunning else { return nil }
        guard let connection, await connection.isActive else { throw AgentConnectionError.failedToStart("The running core is not responding.") }
        let result: AgentShutdownReadinessResult = try await connection.request(method: "agent.shutdownReadiness", params: AgentListParams())
        refreshProtection.update(paths: result.refreshLockPaths, hasSharedHomes: result.hasSharedHomes)
        return result
    }

    public func prepareShutdownIfRunning(operationKey: String) async throws {
        guard latestInitializeResult != nil, let process, process.isRunning else { return }
        guard let connection, await connection.isActive else { throw AgentConnectionError.failedToStart("The running core is not responding.") }
        try await withTaskCancellationHandler {
            let _: AgentEmptyResult = try await connection.request(method: "agent.prepareShutdown", params: AgentShutdownOperationParams(operationKey: operationKey))
        } onCancel: {
            Task {
                let _: AgentEmptyResult? = try? await connection.request(method: "agent.cancelShutdown", params: AgentShutdownOperationParams(operationKey: operationKey))
            }
        }
    }

    public func cancelShutdownIfRunning(operationKey: String) async throws {
        guard let process, process.isRunning, let connection else { return }
        let _: AgentEmptyResult = try await connection.request(method: "agent.cancelShutdown", params: AgentShutdownOperationParams(operationKey: operationKey))
    }

    private func prepareToClose() async -> Bool {
        guard latestInitializeResult != nil, let process, process.isRunning else { return true }
        // If a live core no longer answers, we cannot prove teardown is safe.
        // Keep its process and transports rather than applying the kill fallback.
        guard let connection, await connection.isActive else { return false }
        let operationKey = UUID().uuidString
        do {
            let readiness: AgentShutdownReadinessResult = try await connection.request(method: "agent.shutdownReadiness", params: AgentListParams())
            refreshProtection.update(paths: readiness.refreshLockPaths, hasSharedHomes: readiness.hasSharedHomes)
            guard readiness.ready, !Task.isCancelled else { return false }
            try await prepareShutdownIfRunning(operationKey: operationKey)
            guard !Task.isCancelled, !refreshProtection.isRefreshing else {
                try? await cancelShutdownIfRunning(operationKey: operationKey)
                return false
            }
            return true
        } catch {
            try? await cancelShutdownIfRunning(operationKey: operationKey)
            return false
        }
    }

    /// The core exits on stdin EOF, so closing the connection normally suffices;
    /// it gets `exitGrace` to finish before SIGTERM. Managed-only cores retain
    /// the wedged-process fallback. A core with shared Claude homes must
    /// finish gracefully without a timed force-kill.
    static func terminateAndReap(
        _ process: Process,
        protection: CoreRefreshProtection,
        exitGrace: Duration = .seconds(3)
    ) {
        guard process.isRunning else { return }
        let pid = process.processIdentifier
        Task.detached(priority: .utility) {
            let graceDeadline = ContinuousClock.now + exitGrace
            while process.isRunning && ContinuousClock.now < graceDeadline {
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
            while process.isRunning && protection.isRefreshing {
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
            guard process.isRunning else { return }
            process.terminate()
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            while process.isRunning && protection.isRefreshing {
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
            guard process.isRunning else { return }
            if !protection.canForceTerminate {
                processLogger.notice("preserving shared-home core until graceful shutdown completes")
                while process.isRunning {
                    try? await Task.sleep(nanoseconds: 100_000_000)
                }
                process.waitUntilExit()
                return
            }
            processLogger.error("backend process ignored SIGTERM; sending SIGKILL pid=\(pid, privacy: .public)")
            kill(pid, SIGKILL)
        }
    }

    private func annotatedStartupFailure(_ error: Error) -> Error {
        if case AgentConnectionError.coreExited = error { return error }
        guard let currentLogPaths else {
            return error
        }
        return AgentConnectionError.failedToStart(
            Self.diagnosticMessage(
                "initialization failed: \(error.localizedDescription)",
                logPaths: currentLogPaths
            )
        )
    }

    private static func prepareBackendLogPaths() throws -> BackendProcessLogPaths {
        let logsDirectory = OttamPaths.logsDirectory()
        do {
            return try prepareBackendLogPaths(in: logsDirectory)
        } catch {
            let fallbackDirectory = fallbackLogDirectory()
            Self.processLogger.error(
                "configured backend log directory unavailable; falling back to temporary logs: \(error.localizedDescription, privacy: .private)"
            )
            do {
                return try prepareBackendLogPaths(in: fallbackDirectory)
            } catch {
                throw error
            }
        }
    }

    private static func prepareBackendLogPaths(in logsDirectory: URL) throws -> BackendProcessLogPaths {
        try FileManager.default.createDirectory(
            at: logsDirectory,
            withIntermediateDirectories: true
        )

        let stamp = ISO8601DateFormatter.backendLogStamp.string(from: Date())
        let suffix = "\(stamp)-\(UUID().uuidString)"
        let traceLogURL = logsDirectory.appendingPathComponent("core-\(suffix).log")
        let stderrLogURL = logsDirectory.appendingPathComponent("core-\(suffix).stderr.log")
        try createPrivateLogFile(traceLogURL)
        try createPrivateLogFile(stderrLogURL)
        return BackendProcessLogPaths(traceLogURL: traceLogURL, stderrLogURL: stderrLogURL)
    }

    private static func fallbackLogDirectory() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam/logs")
            .standardizedFileURL
    }

    private static func createPrivateLogFile(_ url: URL) throws {
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: Int16(0o600))],
            ofItemAtPath: url.path
        )
    }

    private static func captureBackendStderr(
        from readHandle: FileHandle,
        to stderrLogURL: URL
    ) -> Task<Void, Never> {
        Task.detached(priority: .utility) {
            guard let outputHandle = try? FileHandle(forWritingTo: stderrLogURL) else {
                Self.processLogger.error("failed to open backend stderr log \(stderrLogURL.path, privacy: .private)")
                return
            }
            defer {
                try? outputHandle.close()
                try? readHandle.close()
            }
            _ = try? outputHandle.seekToEnd()

            while !Task.isCancelled {
                let data = readHandle.availableData
                if data.isEmpty {
                    break
                }
                try? outputHandle.write(contentsOf: data)
                try? FileHandle.standardError.write(contentsOf: data)
            }
        }
    }

    private static func diagnosticMessage(_ message: String, logPaths: BackendProcessLogPaths) -> String {
        var parts = [
            message,
            "Backend trace log: \(logPaths.traceLogURL.path)",
            "Backend stderr log: \(logPaths.stderrLogURL.path)"
        ]
        let stderrTail = tail(logPaths.stderrLogURL, maximumBytes: 8_192, maximumLines: 40)
        if !stderrTail.isEmpty {
            parts.append("Backend stderr tail:\n\(stderrTail)")
        }
        let traceTail = tail(logPaths.traceLogURL, maximumBytes: 8_192, maximumLines: 40)
        if !traceTail.isEmpty {
            parts.append("Backend trace tail:\n\(traceTail)")
        }
        return parts.joined(separator: "\n")
    }

    private static func tail(_ url: URL, maximumBytes: Int, maximumLines: Int) -> String {
        guard let data = try? Data(contentsOf: url), !data.isEmpty else {
            return ""
        }
        let suffix = data.suffix(maximumBytes)
        guard var text = String(data: suffix, encoding: .utf8) else {
            return "<\(suffix.count) bytes of non-UTF8 log data>"
        }
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        if lines.count > maximumLines {
            text = lines.suffix(maximumLines).joined(separator: "\n")
            return "[truncated]\n\(text)"
        }
        if suffix.count < data.count {
            return "[truncated]\n\(text)"
        }
        return text
    }
}

private struct BackendProcessLogPaths: Sendable {
    var traceLogURL: URL
    var stderrLogURL: URL
}

private extension ISO8601DateFormatter {
    static let backendLogStamp: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        // Basic format (20261113T035159.972Z): colons in file names break
        // artifact uploads and non-macOS filesystems.
        formatter.formatOptions = [.withYear, .withMonth, .withDay, .withTime, .withFractionalSeconds, .withTimeZone]
        return formatter
    }()
}

final class ProcessJSONLTransport: JSONLTransport, @unchecked Sendable {
    private let inputHandle: FileHandle
    private let outputHandle: FileHandle
    private let shutdownPipe: Pipe
    private let shutdownLock = NSLock()
    private var shutdownSignalled = false
    private let writeLock = NSLock()
    private let readLock = NSLock()
    private let outputLock = NSLock()
    private var outputClosed = false
    private var bufferedOutput = Data()
    private var isClosed = false

    init(inputHandle: FileHandle, outputHandle: FileHandle, shutdownPipe: Pipe = Pipe()) {
        self.inputHandle = inputHandle
        self.outputHandle = outputHandle
        self.shutdownPipe = shutdownPipe
    }

    func writeLine(_ line: Data) async throws {
        try await Task.detached(priority: .userInitiated) {
            try self.writeLock.withLock {
                guard !self.isClosed else {
                    throw AgentConnectionError.processTerminated
                }
                try self.inputHandle.write(contentsOf: line)
            }
        }.value
    }

    func readLine(maximumBytes: Int) async throws -> Data? {
        try await Task.detached(priority: .userInitiated) {
            try self.readLineBlocking(maximumBytes: maximumBytes)
        }.value
    }

    func close() async {
        // A single unread byte persists even if close wins the race with poll,
        // and does not depend on inherited writer descriptors reaching EOF.
        // Never acquire outputLock first: the reader holds it during the wait.
        shutdownLock.withLock {
            guard !shutdownSignalled else { return }
            shutdownSignalled = true
            var signal: UInt8 = 1
            // This pipe is empty and receives exactly one byte, so it cannot
            // fill. Its read end stays open until after this lock is released.
            while Darwin.write(shutdownPipe.fileHandleForWriting.fileDescriptor, &signal, 1) < 0 && errno == EINTR {}
            try? shutdownPipe.fileHandleForWriting.close()
        }
        writeLock.withLock {
            guard !isClosed else { return }
            isClosed = true
            try? inputHandle.close()
        }
        outputLock.withLock {
            guard !outputClosed else { return }
            outputClosed = true
            try? outputHandle.close()
            try? shutdownPipe.fileHandleForReading.close()
        }
    }

    private func readLineBlocking(maximumBytes: Int) throws -> Data? {
        readLock.lock()
        defer { readLock.unlock() }

        let newline = UInt8(ascii: "\n")
        while true {
            if let newlineIndex = bufferedOutput.firstIndex(of: newline) {
                let line = Data(bufferedOutput[..<newlineIndex])
                bufferedOutput.removeSubrange(...newlineIndex)
                return line
            }

            guard let chunk = try readAvailableChunk() else {
                return nil
            }
            bufferedOutput.append(chunk)
            if bufferedOutput.count > maximumBytes {
                throw AgentConnectionError.responseTooLarge(maximumBytes)
            }
        }
    }

    private func readAvailableChunk() throws -> Data? {
        while true {
            // Keep descriptor access and close mutually exclusive; shutdown
            // interrupts the wait through its own pipe rather than a timeout.
            let chunk: Data? = try outputLock.withLock {
                guard !outputClosed else { return Data() }
                var descriptors = [
                    pollfd(fd: outputHandle.fileDescriptor, events: Int16(POLLIN), revents: 0),
                    pollfd(fd: shutdownPipe.fileHandleForReading.fileDescriptor, events: Int16(POLLIN), revents: 0),
                ]
                let ready = Darwin.poll(&descriptors, 2, -1)
                if ready < 0 && errno == EINTR { return nil }
                guard ready > 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
                if descriptors[1].revents != 0 { return Data() }
                var bytes = [UInt8](repeating: 0, count: 64 * 1024)
                let count = Darwin.read(descriptors[0].fd, &bytes, bytes.count)
                if count < 0 && errno == EINTR { return nil }
                guard count >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
                return Data(bytes.prefix(count))
            }
            guard let chunk else { continue }
            return chunk.isEmpty ? nil : chunk
        }
    }
}
