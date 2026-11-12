import Foundation
import Darwin
import Testing
@testable import OttamBackendClient

@Suite("Process JSONL transport", .serialized, .timeLimit(.minutes(1)))
struct ProcessJSONLTransportTests {
    @Test("shutdown does not depend on the last writer reaching EOF")
    func duplicatedShutdownWriter() async throws {
        let input = Pipe()
        let output = Pipe()
        let shutdown = Pipe()
        let duplicate = Darwin.dup(shutdown.fileHandleForWriting.fileDescriptor)
        #expect(duplicate >= 0)
        defer { Darwin.close(duplicate) }
        let transport = ProcessJSONLTransport(
            inputHandle: input.fileHandleForWriting,
            outputHandle: output.fileHandleForReading,
            shutdownPipe: shutdown
        )
        let reader = Task { try await transport.readLine(maximumBytes: 1024) }
        try await Task.sleep(for: .milliseconds(20))
        await transport.close()
        #expect(try await reader.value == nil)
    }

    @Test("close releases both shutdown descriptors")
    func shutdownDescriptorsClosed() async {
        let input = Pipe()
        let output = Pipe()
        let shutdown = Pipe()
        let readFD = shutdown.fileHandleForReading.fileDescriptor
        let writeFD = shutdown.fileHandleForWriting.fileDescriptor
        let transport = ProcessJSONLTransport(
            inputHandle: input.fileHandleForWriting,
            outputHandle: output.fileHandleForReading,
            shutdownPipe: shutdown
        )
        await transport.close()
        #expect(Darwin.fcntl(readFD, F_GETFD) == -1)
        #expect(Darwin.fcntl(writeFD, F_GETFD) == -1)
    }

    @Test("shutdown ends an idle reader without aborting the process")
    func shutdownEndsRead() async throws {
        let input = Pipe()
        let output = Pipe()
        let transport = ProcessJSONLTransport(
            inputHandle: input.fileHandleForWriting,
            outputHandle: output.fileHandleForReading
        )
        let reader = Task { try await transport.readLine(maximumBytes: 1024) }
        try await Task.sleep(for: .milliseconds(100))
        let started = ContinuousClock.now
        await transport.close()
        let line = try await reader.value
        #expect(line == nil)
        #expect(started.duration(to: .now) < .seconds(1))
    }

    @Test("short lines arrive while the writer remains open")
    func shortLineAndEOF() async throws {
        let input = Pipe()
        let output = Pipe()
        let transport = ProcessJSONLTransport(
            inputHandle: input.fileHandleForWriting,
            outputHandle: output.fileHandleForReading
        )
        try output.fileHandleForWriting.write(contentsOf: Data("one\ntwo\n".utf8))
        let first = try await transport.readLine(maximumBytes: 1024)
        #expect(first == Data("one".utf8))
        let second = try await transport.readLine(maximumBytes: 1024)
        #expect(second == Data("two".utf8))
        try output.fileHandleForWriting.close()
        let end = try await transport.readLine(maximumBytes: 1024)
        #expect(end == nil)
        await transport.close()
    }

    @Test("fragmented lines arrive without waiting for EOF")
    func fragmentedLine() async throws {
        let input = Pipe()
        let output = Pipe()
        let transport = ProcessJSONLTransport(inputHandle: input.fileHandleForWriting, outputHandle: output.fileHandleForReading)
        let reader = Task { try await transport.readLine(maximumBytes: 1024) }
        try output.fileHandleForWriting.write(contentsOf: Data("par".utf8))
        try await Task.sleep(for: .milliseconds(20))
        let started = ContinuousClock.now
        try output.fileHandleForWriting.write(contentsOf: Data("tial\n".utf8))
        #expect(try await reader.value == Data("partial".utf8))
        #expect(started.duration(to: .now) < .seconds(1))
        await transport.close()
    }

    @Test("close interrupts concurrent readers and is repeatable", arguments: 0..<20)
    func closeRace(iteration: Int) async throws {
        let input = Pipe()
        let output = Pipe()
        let transport = ProcessJSONLTransport(inputHandle: input.fileHandleForWriting, outputHandle: output.fileHandleForReading)
        let first = Task { try await transport.readLine(maximumBytes: 1024) }
        let second = Task { try await transport.readLine(maximumBytes: 1024) }
        if iteration.isMultiple(of: 2) { try await Task.sleep(for: .milliseconds(10)) }
        async let closeOne: Void = transport.close()
        async let closeTwo: Void = transport.close()
        _ = await (closeOne, closeTwo)
        #expect(try await first.value == nil)
        #expect(try await second.value == nil)
        await transport.close()
        #expect(try await transport.readLine(maximumBytes: 1024) == nil)
    }

    @Test("oversized input still fails while the writer is open")
    func oversizedLine() async throws {
        let input = Pipe()
        let output = Pipe()
        let transport = ProcessJSONLTransport(inputHandle: input.fileHandleForWriting, outputHandle: output.fileHandleForReading)
        try output.fileHandleForWriting.write(contentsOf: Data(repeating: 65, count: 32))
        do {
            _ = try await transport.readLine(maximumBytes: 16)
            Issue.record("oversized line was accepted")
        } catch AgentConnectionError.responseTooLarge(let limit) {
            #expect(limit == 16)
        }
        await transport.close()
    }
}
