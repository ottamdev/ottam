import AppKit
import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Payload detail loading") @MainActor
struct ChatPayloadDetailStoreTests {
    private func activity(length: UInt64 = 3, availability: String = "available") -> ChatLiveActivity {
        .init(id: "tool", label: "Run command", kind: "execute", payloads: [
            .init(role: "output", availability: availability, payloadID: "payload", byteCount: length, locallyTruncated: false, revision: 1,
                  parts: [.init(name: "Output", format: "text/plain", byteCount: length)])
        ], revision: 1)
    }
    private func response(length: UInt64 = 3, unsaved: Bool = false) -> ConversationReadTranscriptPayloadResult {
        .init(payloadID: "payload", revision: 1, part: 0, offset: 0, data: Data("out".utf8).base64EncodedString(), readableLength: length, availability: unsaved ? "pending" : "available", unsaved: unsaved)
    }
    private func settle(_ condition: () -> Bool) async {
        for _ in 0..<10_000 { if condition() { return }; await Task.yield() }
        Issue.record("Detail request did not settle")
    }

    @Test("Collapsed details make no reads and expansion fetches only the selected payload")
    func demandOnly() async {
        let store = ChatPayloadDetailStore()
        var requests: [String] = []
        let activity = activity()
        store.configure([activity], readPayload: { id, _, _, _ in requests.append(id); return response() }, readBody: nil)
        #expect(requests.isEmpty)
        store.toggle(activity)
        await settle { !store.loading }
        #expect(requests == ["payload"])
        #expect(store.text == "out")
        store.close()
        #expect(store.data.isEmpty)
    }

    @Test("Pending output refreshes on the slow clock and collapse stops it")
    func slowRefresh() async {
        let store = ChatPayloadDetailStore()
        let (ticks, continuation) = AsyncStream<Void>.makeStream()
        var iterator = ticks.makeAsyncIterator()
        store.waitForRefresh = { guard await iterator.next() != nil else { throw CancellationError() } }
        var count = 0
        store.configure([activity(availability: "pending")], readPayload: { _, _, _, _ in count += 1; return response(length: UInt64(count + 2), unsaved: true) }, readBody: nil)
        store.toggle(activity(availability: "pending"))
        await settle { !store.loading }
        store.configure([activity(length: 4, availability: "pending")], readPayload: { _, _, _, _ in count += 1; return response(length: 4, unsaved: true) }, readBody: nil)
        #expect(count == 1)
        continuation.yield(())
        await settle { count == 2 && !store.loading }
        store.close()
        continuation.yield(())
        continuation.finish()
        await Task.yield()
        #expect(count == 2)
    }

    @Test("Command and output load together and copy independently")
    func combinedCommandAndOutput() async throws {
        let store = ChatPayloadDetailStore()
        let arguments = #"{"command":"git status --short\nprintf 'done'","workdir":"/tmp/repo"}"#
        var tool = activity()
        tool.bodyByteCount = UInt64(arguments.utf8.count)
        var reads: [String] = []
        store.configure([tool], readPayload: { _, _, _, _ in reads.append("output"); return response() }, readBody: { id, revision, _ in
            reads.append("input")
            return .init(itemID: id, revision: revision, text: arguments)
        })
        #expect(reads.isEmpty)
        #expect(store.input == nil)
        store.toggle(tool)
        await settle { !store.loading && store.input?.loading == false }
        #expect(Set(reads) == ["input", "output"])
        #expect(try await store.input?.textForCopy() == "git status --short\nprintf 'done'")
        #expect(try await store.textForCopy() == "out")
        let input = try #require(store.input)
        store.close()
        #expect(input.expandedID == nil)
        #expect(input.data.isEmpty)
    }

    @Test("Copy reads the complete output without changing the displayed page")
    func copyPagedOutput() async throws {
        let store = ChatPayloadDetailStore()
        let tool = activity(length: 6)
        var offsets: [UInt64] = []
        store.configure([tool], readPayload: { _, _, _, offset in
            offsets.append(offset)
            return .init(payloadID: "payload", revision: 1, part: 0, offset: offset,
                         data: Data((offset == 0 ? "out" : "put").utf8).base64EncodedString(),
                         nextOffset: offset == 0 ? 3 : nil, readableLength: 6, availability: "available", unsaved: false)
        }, readBody: nil)
        store.toggle(tool)
        await settle { !store.loading }
        #expect(store.text == "out")
        #expect(try await store.textForCopy() == "output")
        #expect(offsets == [0, 0, 3])
        #expect(store.text == "out")
        store.close()
    }

    @Test("Input refreshes independently when arguments change")
    func inputRevision() async throws {
        let store = ChatPayloadDetailStore()
        store.catchUpInterval = .zero
        var tool = activity()
        tool.bodyByteCount = 17
        store.configure([tool], readPayload: { _, _, _, _ in response() }, readBody: { id, revision, _ in
            .init(itemID: id, revision: revision, text: revision == 1 ? #"{"cmd":"old"}"# : #"{"cmd":"new"}"#)
        })
        store.toggle(tool)
        await settle { !store.loading && store.input?.loading == false }
        #expect(try await store.input?.textForCopy() == "old")
        tool.revision = 2
        store.configure([tool], readPayload: { _, _, _, _ in response() }, readBody: { id, revision, _ in
            .init(itemID: id, revision: revision, text: #"{"cmd":"new"}"#)
        })
        await settle { store.input?.loading == false }
        #expect(try await store.input?.textForCopy() == "new")
        #expect(store.text == "out")
        store.close()
    }

    @Test("Non-shell arguments and unknown command shapes remain intact")
    func inputPresentation() {
        let arguments = #"{"path":"notes.txt","command":"do not execute"}"#
        #expect(ChatToolInputPresentation(text: arguments, kind: "read").text == arguments)
        let arrayCommand = #"{"command":["git","status"]}"#
        #expect(ChatToolInputPresentation(text: arrayCommand, kind: "execute").text == arrayCommand)
        #expect(!ChatToolInputPresentation(text: arrayCommand, kind: "execute").isCommand)
    }

    @Test("Collapse rejects an in-flight response")
    func cancellation() async {
        let store = ChatPayloadDetailStore()
        var resume: CheckedContinuation<ConversationReadTranscriptPayloadResult, Never>?
        store.configure([activity()], readPayload: { _, _, _, _ in await withCheckedContinuation { resume = $0 } }, readBody: nil)
        store.toggle(activity())
        await settle { resume != nil }
        store.close()
        resume?.resume(returning: response())
        await Task.yield()
        #expect(store.expandedID == nil)
        #expect(store.data.isEmpty)
    }

    /// A live tool's output can change while a read is in flight; core then
    /// answers the old read with `stalePayload`. The read is not restarted
    /// per revision; one catch-up read follows it.
    @Test("A newer revision catches up after an in-flight read instead of showing it stale")
    func newerRevisionReplacesInFlightRead() async {
        struct Stale: Error {}
        let store = ChatPayloadDetailStore()
        store.catchUpInterval = .zero
        // Keep the pending-payload poll out of the count on a slow runner.
        store.waitForRefresh = { try await Task.sleep(for: .seconds(3600)) }
        var first: CheckedContinuation<ConversationReadTranscriptPayloadResult, Error>?
        var revisions: [UInt64] = []
        func pending(_ revision: UInt64) -> ChatLiveActivity {
            var tool = activity(availability: "pending")
            tool.payloads[0].revision = revision
            return tool
        }
        let reader: ChatPayloadDetailStore.PayloadReader = { _, revision, _, _ in
            revisions.append(revision)
            if revision == 1 { return try await withCheckedThrowingContinuation { first = $0 } }
            return .init(payloadID: "payload", revision: revision, part: 0, offset: 0, data: Data("new".utf8).base64EncodedString(), readableLength: 3, availability: "pending", unsaved: true)
        }
        store.configure([pending(1)], readPayload: reader, readBody: nil)
        store.toggle(pending(1))
        await settle { first != nil }
        store.configure([pending(2)], readPayload: reader, readBody: nil)
        store.configure([pending(3)], readPayload: reader, readBody: nil)
        #expect(revisions == [1])
        first?.resume(throwing: Stale())
        await settle { !store.loading }
        #expect(revisions == [1, 3])
        #expect(store.failure == nil)
        #expect(store.text == "new")
        store.close()
    }

    @Test("Streaming revisions are read at most once per catch-up interval")
    func streamingReadsAreThrottled() async {
        let store = ChatPayloadDetailStore()
        store.catchUpInterval = .seconds(60)
        // The slow-clock refresh is a separate path; keep it out of the count.
        store.waitForRefresh = { try await Task.sleep(for: .seconds(3600)) }
        var thought = ChatLiveActivity(id: "thought", label: "Thinking", kind: "think", revision: 1, bodyByteCount: 3)
        var reads: [UInt64] = []
        let reader: ChatPayloadDetailStore.BodyReader = { id, revision, _ in
            reads.append(revision)
            return .init(itemID: id, revision: revision, text: "r\(revision)")
        }
        store.configure([thought], readPayload: nil, readBody: reader)
        store.toggle(thought)
        await settle { !store.loading }
        for revision in 2...20 {
            thought.revision = UInt64(revision)
            store.configure([thought], readPayload: nil, readBody: reader)
            await Task.yield()
        }
        #expect(reads == [1], "later revisions wait for the interval")
        store.toggle(thought)
        #expect(store.expandedID == nil)
        store.toggle(thought)
        await settle { !store.loading }
        #expect(reads == [1, 20], "opening the row reads at once")
        store.close()
    }

    @Test("Layout follows text growth only up to the height cap")
    func layoutSignature() async {
        let store = ChatPayloadDetailStore()
        store.catchUpInterval = .zero
        var thought = ChatLiveActivity(id: "thought", label: "Thinking", kind: "think", revision: 1, bodyByteCount: 3)
        let lines = [1: 2, 2: 6, 3: 100, 4: 200]
        let reader: ChatPayloadDetailStore.BodyReader = { id, revision, _ in
            .init(itemID: id, revision: revision, text: Array(repeating: "line", count: lines[Int(revision)] ?? 1).joined(separator: "\n"))
        }
        var signatures: [AnyHashable] = []
        for revision in 1...4 {
            thought.revision = UInt64(revision)
            store.configure([thought], readPayload: nil, readBody: reader)
            if revision == 1 { store.toggle(thought) }
            await settle { !store.loading && store.text.split(separator: "\n").count == lines[revision] }
            signatures.append(store.layoutSignature)
        }
        #expect(signatures[0] != signatures[1])
        #expect(signatures[1] != signatures[2])
        #expect(signatures[2] == signatures[3])
        store.close()
        #expect(store.layoutSignature != signatures[0])
    }

    @Test("Wrapped layout signatures stop changing at the viewport cap")
    func wrappedLayoutSignature() async {
        let store = ChatPayloadDetailStore()
        store.catchUpInterval = .zero
        defer { store.close() }
        let font = AppTypography.Resolution.builtIn.nsFont(.code)
        let texts = [
            1: "Planning",
            2: String(repeating: "wrapped reasoning ", count: 6),
            3: String(repeating: "wrapped reasoning ", count: 200),
            4: String(repeating: "wrapped reasoning ", count: 400)
        ]
        let width: CGFloat = 220
        let cap = ChatPayloadTextView.height(forLines: ChatPayloadTextView.outputMaximumLines, font: font)
        #expect(ChatPayloadTextView.height(for: texts[2]!, width: width, font: font, maximumLines: 20) < cap)
        #expect(ChatPayloadTextView.height(for: texts[3]!, width: width, font: font, maximumLines: 20) == cap)

        var thought = ChatLiveActivity(id: "thought", label: "Thinking", kind: "think", revision: 1, bodyByteCount: 3)
        let reader: ChatPayloadDetailStore.BodyReader = { id, revision, _ in
            .init(itemID: id, revision: revision, text: texts[Int(revision)] ?? texts[4]!)
        }
        var signatures: [AnyHashable] = []
        for revision in 1...4 {
            thought.revision = UInt64(revision)
            thought.bodyByteCount = UInt64(texts[revision]!.utf8.count)
            store.configure([thought], readPayload: nil, readBody: reader)
            if revision == 1 { store.toggle(thought) }
            await settle { !store.loading && store.text == texts[revision] }
            signatures.append(store.layoutSignature(forWidth: width, font: font))
        }
        #expect(signatures[0] != signatures[1])
        #expect(signatures[1] != signatures[2])
        #expect(signatures[2] == signatures[3])
    }

    @Test("Requested diffs describe supplied changes without claiming application")
    func requestedDiff() {
        let diff = ChatPayloadDetailStore.requestedDiff(before: "keep\nold", after: "keep\nnew")
        #expect(diff.contains("--- Requested before"))
        #expect(diff.contains(" keep\n-old\n+new"))
        #expect(!diff.contains("Applied"))
    }

    @Test("Edit diff selection is independent of JSON field order")
    func unorderedEditParts() async {
        let store = ChatPayloadDetailStore()
        var edit = activity()
        edit.kind = "edit"
        edit.payloads = [.init(role: "fileChangeInput", availability: "available", payloadID: "input", byteCount: 6, locallyTruncated: false, revision: 1, parts: [
            .init(name: "After", format: "text/plain", byteCount: 3), .init(name: "Before", format: "text/plain", byteCount: 3)
        ])]
        store.configure([edit], readPayload: { _, _, part, _ in
            .init(payloadID: "input", revision: 1, part: part, offset: 0, data: Data((part == 0 ? "new" : "old").utf8).base64EncodedString(), readableLength: 3, availability: "available", unsaved: false)
        }, readBody: nil)
        store.toggle(edit)
        await settle { !store.loading }
        #expect(store.part == -1)
        #expect(store.text.contains("-old\n+new"))
        store.close()
    }
}
