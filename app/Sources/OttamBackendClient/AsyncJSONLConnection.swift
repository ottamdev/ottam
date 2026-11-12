import Foundation
import OSLog

public protocol JSONLTransport: Sendable {
    func writeLine(_ line: Data) async throws
    func readLine(maximumBytes: Int) async throws -> Data?
    func close() async
}

public actor AsyncJSONLConnection: AgentConnection {
    private static let protocolLogger = Logger(
        subsystem: "dev.ottam.Ottam",
        category: "BackendProtocol"
    )

    /// Core's inbound line limit (`MAX_JSONL_MESSAGE_BYTES`). A longer line
    /// gets an error core cannot address to any request.
    static let coreMaximumRequestBytes = 1_048_576

    /// Set while a notification handler runs, so a request it makes can let
    /// the reader past the queue bound to fetch the reply.
    @TaskLocal private static var isHandlingNotification = false

    private let transport: JSONLTransport
    private let maximumMessageBytes: Int
    private let maximumQueuedNotificationBytes: Int
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    private var readTask: Task<Void, Never>?
    /// Notifications wait here for one consumer, so the read loop goes straight
    /// on to the next line and a response never queues behind a handler. Past
    /// `maximumQueuedNotificationBytes` the reader stops instead, so core's
    /// stdout fills and its own coalescing and recovery markers take over;
    /// dropping here would lose data with no marker. A handler that awaits
    /// core lifts the bound until its reply arrives.
    private var notificationQueue: AsyncStream<AgentNotification>.Continuation?
    private var notificationTask: Task<Void, Never>?
    private var queuedNotificationBytes = 0
    private var readerAwaitingQueueSpace: CheckedContinuation<Void, Never>?
    private var requestsFromHandlers = 0
    private var nextRequestID: Int64 = 1
    private var pendingRequests: [RequestID: PendingRequest] = [:]
    private var clientRequestTasks: [RequestID: Task<Void, Never>] = [:]
    private var notificationHandler: (@Sendable (AgentNotification) async -> Void)?
    private var clientRequestHandler: AgentClientRequestHandler?
    private var isClosed = false
    public private(set) var coreExitError: CoreExitError?

    /// A process-exit callback can precede the final stdout read. Drain the
    /// existing reader, and the notifications it read, before publishing
    /// termination to retain core.exitError.
    func waitForReadCompletion() async {
        await readTask?.value
        await notificationTask?.value
    }

    private var terminationError: AgentConnectionError {
        coreExitError.map(AgentConnectionError.coreExited) ?? .processTerminated
    }

    public init(
        transport: JSONLTransport,
        maximumMessageBytes: Int = 1_048_576,
        maximumQueuedNotificationBytes: Int = 4 * 1_048_576
    ) {
        self.transport = transport
        self.maximumMessageBytes = maximumMessageBytes
        self.maximumQueuedNotificationBytes = maximumQueuedNotificationBytes
    }

    public var isActive: Bool {
        !isClosed
    }

    public func request<Params: Encodable & Sendable, Result: Decodable & Sendable>(
        method: String,
        params: Params
    ) async throws -> Result {
        try startReadLoopIfNeeded()
        guard !isClosed else {
            throw terminationError
        }

        let id = RequestID.number(nextRequestID)
        nextRequestID += 1

        let request = ProtocolRequest(id: id, method: method, params: params)
        let requestData: Data
        do {
            requestData = try encodeLine(request)
        } catch let error as AgentConnectionError {
            Self.protocolLogger.error("backend request not sent method=\(method, privacy: .public): \(error.localizedDescription, privacy: .public)")
            throw error
        }
        let started = Date()
        Self.protocolLogger.debug(
            "sending backend request method=\(method, privacy: .public) idKind=\(id.logKind, privacy: .public)"
        )

        if Self.isHandlingNotification {
            requestsFromHandlers += 1
            resumeReaderIfQueueHasSpace()
        }
        defer {
            if Self.isHandlingNotification { requestsFromHandlers -= 1 }
        }
        return try await withTaskCancellationHandler {
            try Task.checkCancellation()
            return try await awaitResponse(id: id, requestData: requestData, method: method, started: started)
        } onCancel: {
            Task { await self.resumePending(id: id, throwing: CancellationError()) }
        }
    }

    private func awaitResponse<Result: Decodable & Sendable>(
        id: RequestID, requestData: Data, method: String, started: Date
    ) async throws -> Result {
        try Task.checkCancellation()
        return try await withCheckedThrowingContinuation { continuation in
            pendingRequests[id] = PendingRequest(
                resumeData: { [decoder] responseData in
                    do {
                        let response = try decoder.decode(ProtocolResponse<Result>.self, from: responseData)
                        if let error = response.error {
                            Self.protocolLogger.error(
                                "backend request failed method=\(method, privacy: .public) code=\(error.code, privacy: .public) elapsedMs=\(started.elapsedMilliseconds, privacy: .public)"
                            )
                            continuation.resume(
                                throwing: AgentConnectionError.rpcError(
                                    code: error.code,
                                    message: error.message,
                                    data: error.data
                                )
                            )
                            return
                        }
                        guard let result = response.result else {
                            Self.protocolLogger.error(
                                "backend response missing result method=\(method, privacy: .public) elapsedMs=\(started.elapsedMilliseconds, privacy: .public)"
                            )
                            continuation.resume(
                                throwing: AgentConnectionError.invalidResponse("missing result")
                            )
                            return
                        }
                        Self.protocolLogger.info(
                            "backend request completed method=\(method, privacy: .public) elapsedMs=\(started.elapsedMilliseconds, privacy: .public)"
                        )
                        continuation.resume(returning: result)
                    } catch {
                        Self.protocolLogger.error(
                            "backend response decode failed method=\(method, privacy: .public) error=\(error.localizedDescription, privacy: .private)"
                        )
                        continuation.resume(throwing: error)
                    }
                },
                resumeError: { error in
                    // A caller dropping a superseded read is not a failure.
                    if error is CancellationError {
                        Self.protocolLogger.debug("backend request cancelled method=\(method, privacy: .public)")
                    } else {
                        Self.protocolLogger.error(
                            "backend request transport failed method=\(method, privacy: .public) error=\(error.localizedDescription, privacy: .private)"
                        )
                    }
                    continuation.resume(throwing: error)
                }
            )

            Task {
                do {
                    try await transport.writeLine(requestData)
                } catch {
                    self.resumePending(id: id, throwing: error)
                }
            }
        }
    }

    public func notify<Params: Codable & Sendable>(
        method: String,
        params: Params
    ) async throws {
        try startReadLoopIfNeeded()
        guard !isClosed else {
            throw terminationError
        }

        let notification = ProtocolNotification(method: method, params: params)
        Self.protocolLogger.debug("sending backend notification method=\(method, privacy: .public)")
        try await transport.writeLine(try encodeLine(notification))
    }

    public func setNotificationHandler(_ handler: (@Sendable (AgentNotification) async -> Void)?) async {
        notificationHandler = handler
    }

    public func setClientRequestHandler(_ handler: AgentClientRequestHandler?) async {
        clientRequestHandler = handler
    }

    public func close() async {
        guard !isClosed else {
            return
        }
        isClosed = true
        readTask?.cancel()
        readTask = nil
        notificationQueue?.finish()
        notificationTask?.cancel()
        releaseWaitingReader()
        cancelClientRequestTasks()
        await transport.close()
        failAllPending(with: terminationError)
    }

    private func startReadLoopIfNeeded() throws {
        guard !isClosed else {
            throw terminationError
        }
        guard readTask == nil else {
            return
        }
        let (notifications, queue) = AsyncStream<AgentNotification>.makeStream()
        notificationQueue = queue
        notificationTask = Task { [weak self] in
            for await notification in notifications {
                guard let self, !Task.isCancelled else { return }
                await self.deliver(notification)
            }
        }
        readTask = Task {
            await readLoop()
        }
    }

    private func deliver(_ notification: AgentNotification) async {
        defer {
            queuedNotificationBytes -= notification.payload.count
            resumeReaderIfQueueHasSpace()
        }
        guard let notificationHandler else {
            Self.protocolLogger.debug("backend notification/event ignored method=\(notification.method, privacy: .public)")
            return
        }
        // Handlers run one at a time in wire order. A slow one delays later
        // notifications, not responses; log it with the method that held them.
        let started = Date()
        await Self.$isHandlingNotification.withValue(true) {
            await notificationHandler(notification)
        }
        let elapsed = started.elapsedMilliseconds
        if elapsed >= 50 {
            Self.protocolLogger.info(
                "backend notification handled slowly method=\(notification.method, privacy: .public) bytes=\(notification.payload.count, privacy: .public) elapsedMs=\(elapsed, privacy: .public)"
            )
        }
    }

    private func readLoop() async {
        do {
            while !Task.isCancelled {
                guard let line = try await transport.readLine(maximumBytes: maximumMessageBytes) else {
                    Self.protocolLogger.error("backend read loop ended before response")
                    failFromReadLoop(terminationError)
                    return
                }
                if line.count > maximumMessageBytes {
                    Self.protocolLogger.error("backend response exceeded maximum bytes \(self.maximumMessageBytes, privacy: .public)")
                    await transport.close()
                    failFromReadLoop(AgentConnectionError.responseTooLarge(maximumMessageBytes))
                    return
                }
                try await route(line)
            }
        } catch {
            Self.protocolLogger.error("backend read loop failed: \(error.localizedDescription, privacy: .private)")
            await transport.close()
            failFromReadLoop(error)
        }
    }

    /// Holds the read loop while handlers have more than the bound queued,
    /// unless one of them awaits a reply that is still in the pipe.
    private func waitForQueueSpace() async {
        guard !isClosed, queuedNotificationBytes > maximumQueuedNotificationBytes, requestsFromHandlers == 0 else {
            return
        }
        Self.protocolLogger.info(
            "backend notifications backed up; pausing reads queuedBytes=\(self.queuedNotificationBytes, privacy: .public)"
        )
        await withCheckedContinuation { readerAwaitingQueueSpace = $0 }
    }

    private func resumeReaderIfQueueHasSpace() {
        guard queuedNotificationBytes <= maximumQueuedNotificationBytes || requestsFromHandlers > 0 else { return }
        releaseWaitingReader()
    }

    private func releaseWaitingReader() {
        let reader = readerAwaitingQueueSpace
        readerAwaitingQueueSpace = nil
        reader?.resume()
    }

    private func failFromReadLoop(_ error: Error) {
        isClosed = true
        readTask = nil
        // Let the consumer finish what was read before the end.
        notificationQueue?.finish()
        cancelClientRequestTasks()
        failAllPending(with: error)
    }

    private func route(_ line: Data) async throws {
        let envelope: ResponseEnvelope
        do {
            envelope = try decoder.decode(ResponseEnvelope.self, from: line)
        } catch {
            Self.protocolLogger.error("backend returned malformed JSONL envelope")
            throw AgentConnectionError.invalidResponse("malformed JSONL envelope")
        }

        if let id = envelope.id, let method = envelope.method {
            let request = AgentClientRequest(id: id, method: method, payload: line)
            let handler = clientRequestHandler
            clientRequestTasks[id]?.cancel()
            clientRequestTasks[id] = Task {
                let response = if let handler {
                    await handler(request)
                } else if request.method == "agent.requestPermission"
                    || request.method == "agent.askQuestion" {
                    AgentClientResponse.success(.object(["outcome": .string("cancelled")]))
                } else {
                    AgentClientResponse.failure(
                        AgentProtocolError(code: -32601, message: "method not found")
                    )
                }
                // Core withdrew it (`$/cancelRequest`) and no longer waits.
                guard !Task.isCancelled else { return }
                await self.sendClientResponse(id: id, response: response)
                self.clientRequestTasks.removeValue(forKey: id)
            }
            return
        }

        guard let id = envelope.id else {
            if envelope.method == nil,
               let error = try? decoder.decode(UnaddressedErrorEnvelope.self, from: line).error {
                // Core rejected a line it could not read an id from, so the
                // request it answers is unknown. Fail what is in flight rather
                // than leave one waiting forever; the connection still works.
                Self.protocolLogger.error(
                    "backend error without request id code=\(error.code, privacy: .public); failing \(self.pendingRequests.count, privacy: .public) pending requests"
                )
                failAllPending(with: AgentConnectionError.rpcError(code: error.code, message: error.message, data: error.data))
                return
            }
            let notification = try decoder.decode(NotificationEnvelope.self, from: line)
            if notification.method == "$/cancelRequest" {
                if let id = try? decoder.decode(
                    ProtocolNotification<CancelRequestParams>.self,
                    from: line
                ).params.id {
                    clientRequestTasks.removeValue(forKey: id)?.cancel()
                }
                return
            }
            if notification.method == "core.exitError" {
                // Malformed optional exit diagnostics must not hide the original
                // transport failure or prevent the normal logs fallback.
                coreExitError = try? decoder.decode(ProtocolNotification<CoreExitError>.self, from: line).params
            }
            queuedNotificationBytes += line.count
            notificationQueue?.yield(AgentNotification(method: notification.method, payload: line))
            await waitForQueueSpace()
            return
        }

        guard let pending = pendingRequests.removeValue(forKey: id) else {
            Self.protocolLogger.debug("backend response had no pending request idKind=\(id.logKind, privacy: .public)")
            return
        }
        pending.resumeData(line)
    }

    private func sendClientResponse(id: RequestID, response: AgentClientResponse) async {
        let envelope: ClientResponseEnvelope
        switch response {
        case .success(let result):
            envelope = ClientResponseEnvelope(id: id, result: result, error: nil)
        case .failure(let error):
            envelope = ClientResponseEnvelope(id: id, result: nil, error: error)
        }
        do {
            try await transport.writeLine(try encodeLine(envelope))
        } catch {
            Self.protocolLogger.error(
                "failed to send client response: \(error.localizedDescription, privacy: .private)"
            )
            await failFromClientResponse(error)
        }
    }

    private func failFromClientResponse(_ error: Error) async {
        guard !isClosed else {
            return
        }
        isClosed = true
        readTask?.cancel()
        readTask = nil
        notificationQueue?.finish()
        releaseWaitingReader()
        cancelClientRequestTasks()
        await transport.close()
        failAllPending(with: error)
    }

    private func resumePending(id: RequestID, throwing error: Error) {
        let pending = pendingRequests.removeValue(forKey: id)
        pending?.resumeError(error)
    }

    private func failAllPending(with error: Error) {
        let pending = pendingRequests.values
        pendingRequests.removeAll()
        for request in pending {
            request.resumeError(error)
        }
    }

    private func cancelClientRequestTasks() {
        let tasks = clientRequestTasks.values
        clientRequestTasks.removeAll()
        for task in tasks {
            task.cancel()
        }
    }

    private func encodeLine<T: Encodable>(_ value: T) throws -> Data {
        var data = try encoder.encode(value)
        if data.count > maximumMessageBytes {
            throw AgentConnectionError.responseTooLarge(maximumMessageBytes)
        }
        // Lines core would reject never leave: its error could not name the
        // request, so failing here keeps the connection and the right caller.
        if data.count > Self.coreMaximumRequestBytes {
            throw AgentConnectionError.requestTooLarge(Self.coreMaximumRequestBytes)
        }
        data.append(UInt8(ascii: "\n"))
        return data
    }
}

private extension RequestID {
    var logKind: String {
        switch self {
        case .null:
            return "null"
        case .number:
            return "number"
        case .string:
            return "string"
        }
    }
}

private extension Date {
    var elapsedMilliseconds: Int {
        Int(Date().timeIntervalSince(self) * 1_000)
    }
}

private struct PendingRequest {
    let resumeData: (Data) -> Void
    let resumeError: (Error) -> Void
}

private struct ResponseEnvelope: Decodable {
    var id: RequestID?
    var method: String?
}

private struct UnaddressedErrorEnvelope: Decodable {
    var error: AgentProtocolError
}

private struct NotificationEnvelope: Decodable {
    var method: String
}

private struct ClientResponseEnvelope: Encodable {
    var id: RequestID
    var result: JSONValue?
    var error: AgentProtocolError?

    private enum CodingKeys: String, CodingKey { case id, result, error }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encodeIfPresent(result, forKey: .result)
        try container.encodeIfPresent(error, forKey: .error)
    }
}
