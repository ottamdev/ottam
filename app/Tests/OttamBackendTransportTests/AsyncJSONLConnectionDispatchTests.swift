import Foundation
import Testing
@testable import OttamBackendClient

/// Responses must not wait for notification handlers: a handler that awaits
/// core would otherwise hold the read loop that carries its own reply.
@Suite("JSONL connection notification dispatch", .serialized, .timeLimit(.minutes(1)))
struct AsyncJSONLConnectionDispatchTests {
    @Test("a notification handler can await a request on the same connection")
    func handlerAwaitsRequest() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let handlerFinished = Flag()
        transport.push(#"{"jsonrpc":"2.0","method":"test.ping","params":{}}"#)
        await connection.setNotificationHandler { _ in
            let _: Empty? = try? await connection.request(method: "test.echo", params: Empty())
            handlerFinished.set()
        }

        let started: Empty? = await withTimeout(.seconds(2)) {
            try await connection.request(method: "test.start", params: Empty())
        }
        #expect(started != nil)
        #expect(await handlerFinished.wait(for: .seconds(2)))
        await connection.close()
    }

    @Test("a slow notification handler does not delay a later response")
    func slowHandlerDoesNotDelayResponse() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        transport.push(#"{"jsonrpc":"2.0","method":"test.slow","params":{}}"#)
        await connection.setNotificationHandler { _ in
            try? await Task.sleep(for: .seconds(1))
        }

        let clock = ContinuousClock()
        let started = clock.now
        let _: Empty = try await connection.request(method: "test.start", params: Empty())
        #expect(started.duration(to: clock.now) < .milliseconds(500))
        await connection.close()
    }

    @Test("notifications reach the handler one at a time, in wire order")
    func notificationsStayOrdered() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let received = Recorder()
        for index in 0..<50 {
            transport.push(#"{"jsonrpc":"2.0","method":"test.n\#(index)","params":{}}"#)
        }
        await connection.setNotificationHandler { notification in
            await received.begin(notification.method)
            if notification.method == "test.n3" { try? await Task.sleep(for: .milliseconds(50)) }
            await received.end()
        }
        let _: Empty = try await connection.request(method: "test.start", params: Empty())
        #expect(await received.waitForCount(50, within: .seconds(2)))
        #expect(await received.methods == (0..<50).map { "test.n\($0)" })
        #expect(await received.maxConcurrent == 1)
        await connection.close()
    }

    @Test("read completion includes notifications read before EOF")
    func readCompletionDrainsNotifications() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let received = Recorder()
        for index in 0..<5 {
            transport.push(#"{"jsonrpc":"2.0","method":"test.n\#(index)","params":{}}"#)
        }
        await connection.setNotificationHandler { notification in
            try? await Task.sleep(for: .milliseconds(20))
            await received.begin(notification.method)
            await received.end()
        }
        let _: Empty = try await connection.request(method: "test.start", params: Empty())
        transport.finish()
        await connection.waitForReadCompletion()
        #expect(await received.methods.count == 5)
    }

    @Test("notifications a handler has not taken stop the reader once they pass the byte bound")
    func queuedNotificationsBoundTheReader() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let release = Flag()
        let received = Recorder()
        // 8 MiB of notifications, twice the bound, ahead of the response.
        let padding = String(repeating: "x", count: 256 * 1024)
        for index in 0..<32 {
            transport.push(#"{"jsonrpc":"2.0","method":"test.n\#(index)","params":{"pad":"\#(padding)"}}"#)
        }
        await connection.setNotificationHandler { notification in
            _ = await release.wait(for: .seconds(30))
            await received.begin(notification.method)
            await received.end()
        }
        let start = Task { () -> Empty in
            try await connection.request(method: "test.start", params: Empty())
        }

        try await Task.sleep(for: .milliseconds(300))
        // The reader stopped with lines still in the pipe; core sees that.
        #expect(transport.unreadLineCount > 0)

        release.set()
        let delivered = await received.waitForCount(32, within: .seconds(5))
        #expect(delivered)
        // Closing releases a stuck reader, so a regression fails, not hangs.
        if !delivered { await connection.close() }
        #expect((try? await start.value) != nil)
        #expect(await received.methods == (0..<32).map { "test.n\($0)" })
        await connection.close()
    }

    @Test("a handler can await a request while the queue is past its bound")
    func handlerAwaitsRequestPastBound() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let received = Recorder()
        let padding = String(repeating: "x", count: 256 * 1024)
        for index in 0..<32 {
            transport.push(#"{"jsonrpc":"2.0","method":"test.n\#(index)","params":{"pad":"\#(padding)"}}"#)
        }
        await connection.setNotificationHandler { notification in
            // Its reply is behind 8 MiB of notifications the reader must pass.
            if notification.method == "test.n0" {
                try? await Task.sleep(for: .milliseconds(100))
                let _: Empty? = try? await connection.request(method: "test.echo", params: Empty())
            }
            await received.begin(notification.method)
            await received.end()
        }
        let start = Task { () -> Empty in
            try await connection.request(method: "test.start", params: Empty())
        }
        let delivered = await received.waitForCount(32, within: .seconds(5))
        #expect(delivered)
        // Closing releases a deadlocked reader, so a regression fails, not hangs.
        if !delivered { await connection.close() }
        #expect((try? await start.value) != nil)
        await connection.close()
    }

    @Test("a request core could not accept fails without being sent")
    func oversizeRequestStaysLocal() async throws {
        let transport = ScriptedTransport()
        // The client reads core's large responses; core reads at most 1 MiB.
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 16 * 1024 * 1024)
        let _: Empty = try await connection.request(method: "test.start", params: Empty())
        let writes = transport.writtenLineCount

        var failure: AgentConnectionError?
        do {
            let _: Empty = try await connection.request(
                method: "test.paste",
                params: ["text": String(repeating: "x", count: 1_200_000)]
            )
        } catch {
            failure = error as? AgentConnectionError
        }
        #expect(failure == .requestTooLarge(1_048_576))
        #expect(transport.writtenLineCount == writes)
        #expect(await connection.isActive)
    }

    @Test("an error core could not tie to a request fails what is in flight and keeps the connection")
    func unaddressedErrorKeepsConnection() async throws {
        let transport = ScriptedTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        transport.reply(
            to: "test.rejected",
            with: #"{"jsonrpc":"2.0","id":null,"error":{"code":-32001,"message":"JSONL message exceeds 1048576 bytes"}}"#
        )

        let rejected: Result<Empty, Error>? = await withTimeout(.seconds(2)) {
            do { return .success(try await connection.request(method: "test.rejected", params: Empty())) }
            catch { return .failure(error) }
        }
        guard case .failure(let error)? = rejected else {
            Issue.record("expected the in-flight request to fail, got \(String(describing: rejected))")
            return
        }
        #expect(error as? AgentConnectionError == .rpcError(
            code: -32001,
            message: "JSONL message exceeds 1048576 bytes",
            data: nil
        ))
        #expect(await connection.isActive)
        let after: Empty? = await withTimeout(.seconds(2)) {
            try await connection.request(method: "test.after", params: Empty())
        }
        #expect(after != nil)
        await connection.close()
    }
}

private struct Empty: Codable, Sendable {}

/// Replays queued lines to the reader and answers every request with `{}`,
/// or with the line set for its method.
private final class ScriptedTransport: JSONLTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var lines: [Data] = []
    private var readers: [CheckedContinuation<Data?, Never>] = []
    private var finished = false
    private var replies: [String: String] = [:]
    private var writes = 0

    var writtenLineCount: Int { lock.withLock { writes } }
    var unreadLineCount: Int { lock.withLock { lines.count } }

    func push(_ line: String) {
        deliver(Data(line.utf8))
    }

    func reply(to method: String, with line: String) {
        lock.withLock { replies[method] = line }
    }

    func finish() {
        let waiting = lock.withLock {
            finished = true
            defer { readers.removeAll() }
            return readers
        }
        for reader in waiting { reader.resume(returning: nil) }
    }

    func writeLine(_ line: Data) async throws {
        lock.withLock { writes += 1 }
        guard let object = try JSONSerialization.jsonObject(with: line) as? [String: Any],
              let method = object["method"] as? String,
              let id = object["id"] as? Int else { return }
        push(lock.withLock { replies[method] } ?? #"{"jsonrpc":"2.0","id":\#(id),"result":{}}"#)
    }

    func readLine(maximumBytes: Int) async throws -> Data? {
        await withCheckedContinuation { continuation in
            let ready: Data?? = lock.withLock {
                if !lines.isEmpty { return .some(lines.removeFirst()) }
                if finished { return .some(nil) }
                readers.append(continuation)
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    func close() async {
        finish()
    }

    private func deliver(_ line: Data) {
        let reader: CheckedContinuation<Data?, Never>? = lock.withLock {
            if !readers.isEmpty { return readers.removeFirst() }
            lines.append(line)
            return nil
        }
        reader?.resume(returning: line)
    }
}

private final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false

    func set() {
        lock.withLock { value = true }
    }

    func wait(for duration: Duration) async -> Bool {
        let deadline = ContinuousClock.now.advanced(by: duration)
        while ContinuousClock.now < deadline {
            if lock.withLock({ value }) { return true }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return false
    }
}

private actor Recorder {
    private(set) var methods: [String] = []
    private(set) var maxConcurrent = 0
    private var active = 0

    func begin(_ method: String) {
        methods.append(method)
        active += 1
        maxConcurrent = max(maxConcurrent, active)
    }

    func end() {
        active -= 1
    }

    func waitForCount(_ count: Int, within duration: Duration) async -> Bool {
        let deadline = ContinuousClock.now.advanced(by: duration)
        while methods.count < count, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(10))
        }
        return methods.count >= count
    }
}

private func withTimeout<T: Sendable>(
    _ duration: Duration,
    _ operation: @escaping @Sendable () async throws -> T
) async -> T? {
    await withTaskGroup(of: T?.self) { group in
        group.addTask { try? await operation() }
        group.addTask {
            try? await Task.sleep(for: duration)
            return nil
        }
        let first = await group.next() ?? nil
        group.cancelAll()
        return first
    }
}
