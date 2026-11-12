import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// Core refuses terminal input with a recoverable `terminalInputBusy` while
/// the program's queue is full. The writer waits it out instead of dropping.
@Suite("Terminal input writer")
struct TerminalInputWriterTests {
    private static let busy = AgentConnectionError.rpcError(
        code: -32000,
        message: "terminal input queue is full",
        data: .object(["kind": .string("terminalInputBusy"), "recoverable": .bool(true)])
    )

    @Test("a busy terminal is retried until every byte arrives, in order")
    func busyIsRetried() async throws {
        let core = FakeTerminalCore(busyReplies: 5)
        let input = Data((0..<(3 * TerminalInputWriter.chunkBytes + 100)).map { UInt8(truncatingIfNeeded: $0) })

        try await TerminalInputWriter.write(input, pause: { _ in }) { chunk in
            try await core.write(chunk)
        }

        #expect(await core.received == input)
        #expect(await core.refusals == 5)
    }

    @Test("a terminal that stays busy fails with core's error and sends nothing after it")
    func persistentBusyFails() async throws {
        let core = FakeTerminalCore(busyReplies: .max)
        let input = Data(repeating: 7, count: 2 * TerminalInputWriter.chunkBytes)
        var failure: Error?
        do {
            try await TerminalInputWriter.write(input, pause: { _ in }) { chunk in
                try await core.write(chunk)
            }
        } catch {
            failure = error
        }
        #expect(failure as? AgentConnectionError == Self.busy)
        #expect(await core.received.isEmpty)
    }

    @Test("other write errors are not retried")
    func otherErrorsFailAtOnce() async throws {
        let core = FakeTerminalCore(busyReplies: 0, failure: AgentConnectionError.processTerminated)
        var failure: Error?
        do {
            try await TerminalInputWriter.write(Data("ls\n".utf8), pause: { _ in }) { chunk in
                try await core.write(chunk)
            }
        } catch {
            failure = error
        }
        #expect(failure as? AgentConnectionError == .processTerminated)
        #expect(await core.attempts == 1)
    }

    private actor FakeTerminalCore {
        private var busyReplies: Int
        private let failure: Error?
        private(set) var received = Data()
        private(set) var refusals = 0
        private(set) var attempts = 0

        init(busyReplies: Int, failure: Error? = nil) {
            self.busyReplies = busyReplies
            self.failure = failure
        }

        func write(_ chunk: Data) throws {
            attempts += 1
            if let failure { throw failure }
            if busyReplies > 0 {
                busyReplies -= 1
                refusals += 1
                throw TerminalInputWriterTests.busy
            }
            received.append(chunk)
        }
    }
}
