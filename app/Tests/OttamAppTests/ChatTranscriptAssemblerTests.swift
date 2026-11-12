import OttamBackendClient
import OttamWorkspaceLogic
import OttamMarkdownEngine
import Testing
import Foundation
@testable import OttamApp

@Suite("Chat transcript assembler")
@MainActor
struct ChatTranscriptAssemblerTests {
    @Test("session signals invalidate snapshots, reject stale pages, and clear after termination and restart")
    func sessionSignalRecovery() {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        let run = ConversationRunID("run-1")
        let state = ConversationSessionState(runID: run, signal: .init(sessionId: "claude:one", activity: "retrying"), live: true)
        store.merge(.init(sessionStates: [state], coreInstanceID: .init("core"), sequence: 2, items: []), conversationID: conversation)
        #expect(store.sessionStates[run]?.signal.activity == "retrying")
        let before = ChatTranscriptSnapshot(sessionStates: store.sessionStates)
        var recovered = state
        recovered.signal.activity = "active"
        _ = store.merge(.init(sessionStates: [recovered], workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 3, items: [], bodyAppends: []))
        #expect(before != ChatTranscriptSnapshot(sessionStates: store.sessionStates))
        store.merge(.init(sessionStates: [state], coreInstanceID: .init("core"), sequence: 2, items: []), conversationID: conversation)
        #expect(store.sessionStates[run]?.signal.activity == "active")
        store.disconnectSessionActivity()
        #expect(store.sessionStates[run]?.signal.activity == nil)
        store.merge(.init(sessionStates: [state], coreInstanceID: .init("core"), sequence: 3, items: []), conversationID: conversation)
        #expect(store.sessionStates[run]?.live == false)
        store.merge(.init(sessionStates: [state], coreInstanceID: .init("core"), sequence: 4, items: []), conversationID: conversation)
        #expect(store.sessionStates[run]?.live == true)
        #expect(store.sessionStates[run]?.signal.activity == "retrying")
        store.endSessionActivity(run)
        store.merge(.init(sessionStates: [state], coreInstanceID: .init("core"), sequence: 5, items: []), conversationID: conversation)
        #expect(store.sessionStates[run]?.live == false)
        #expect(store.sessionStates[run]?.signal.activity == nil)
        store.merge(.init(coreInstanceID: .init("new-core"), sequence: 1, items: []), conversationID: conversation)
        #expect(store.sessionStates.isEmpty)
    }
    @Test("capture-only deltas invalidate the native snapshot comparison", arguments: [false, true])
    func captureOnlySnapshotChanges(gapOnly: Bool) {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        let run = ConversationRunID("run-1")
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1,
            items: [item("retained", position: 1, body: "Unchanged reply")]), conversationID: conversation)
        func snapshot() -> ChatTranscriptSnapshot {
            .init(normalizedItems: store.orderedItems, normalizedBodies: store.bodies,
                  captureProblems: store.captureProblems, captureGaps: store.captureGaps)
        }
        let before = snapshot()
        _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 2, items: [], bodyAppends: [], capture: [
                .init(runID: run, state: "partial", problem: gapOnly ? nil : "Capture may be incomplete",
                      gaps: gapOnly ? [.init(startPosition: 2, endPosition: 3)] : [])
            ]))
        let changed = snapshot()
        #expect(changed.normalizedItems == before.normalizedItems)
        #expect(changed.normalizedBodies == before.normalizedBodies)
        #expect(changed != before, "MacAgentTranscriptView must publish this capture-only change")
        #expect(ChatTranscriptAssembler.plan(changed) != ChatTranscriptAssembler.plan(before))
        if gapOnly {
            var extended = changed
            extended.captureGaps[run] = [.init(startPosition: 2, endPosition: 4)]
            #expect(extended != changed, "range endpoints matter even when the marker ID is unchanged")
        }
        _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 3, items: [], bodyAppends: [], capture: [
                .init(runID: run, state: "complete")
            ]))
        #expect(snapshot() != changed, "clearing capture state must also invalidate the renderer")
        #expect(snapshot() == before)
    }

    @Test("capture loss ranges render between retained output and survive recovery")
    func positionedCaptureGaps() {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        let run = ConversationRunID("run-1")
        let capture = TranscriptCaptureStatus(runID: run, state: "partial", problem: "Some output was not saved",
            gaps: [.init(startPosition: 2, endPosition: 4), .init(startPosition: 6, endPosition: 6)])
        store.merge(.init(coreInstanceID: .init("core"), sequence: 3,
            items: [item("before", position: 1), item("prefix", position: 2), item("after", position: 5), item("last", position: 7)], capture: [capture]), conversationID: conversation)
        #expect(store.captureGaps[run] == capture.gaps)
        var snapshot = ChatTranscriptSnapshot(normalizedItems: store.orderedItems, captureGaps: store.captureGaps)
        let ids = ChatTranscriptAssembler.plan(snapshot).map(\.id)
        #expect(ids == ["before", "prefix", "capture-gap/run-1/2", "after", "capture-gap/run-1/6", "last"])
        // A stale page must not clear ranges any more than it can clear warnings.
        store.merge(.init(coreInstanceID: .init("core"), sequence: 2, items: [], capture: [.init(runID: run, state: "complete")]), conversationID: conversation)
        #expect(store.captureGaps[run] == capture.gaps)
        _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 4, items: [], bodyAppends: [], capture: [
                .init(runID: run, state: "partial", problem: capture.problem, gaps: [
                    .init(startPosition: 2, endPosition: 4), .init(startPosition: 2, endPosition: 6)
                ])
            ]))
        snapshot.captureGaps = store.captureGaps
        #expect(ChatTranscriptAssembler.plan(snapshot).filter { $0.id.hasPrefix("capture-gap/") }.map(\.id) == ["capture-gap/run-1/2"])
        #expect(store.captureGaps[run]?.first?.endPosition == 6)
        store.reset()
        #expect(store.captureGaps.isEmpty)
    }

    @Test("capture warnings leave successful turn status intact and clear after recovery")
    func captureWarnings() {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        let run = ConversationRunID("run-1")
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [], capture: [
            .init(runID: run, state: "open", problem: "Capture may be incomplete")
        ]), conversationID: conversation)
        #expect(store.captureProblems[run] == "Capture may be incomplete")
        var snapshot = TranscriptFixture.snapshot()
        snapshot.captureProblems = store.captureProblems
        let entries = ChatTranscriptAssembler.plan(snapshot)
        #expect(entries.contains { $0.id == "capture/run-1" })
        #expect(snapshot.runStatuses[run] == .completed)
        _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 2, items: [], bodyAppends: [], capture: [
                .init(runID: run, state: "complete", problem: nil)
            ]))
        #expect(store.captureProblems.isEmpty)
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [], capture: [
            .init(runID: run, state: "open", problem: "Capture may be incomplete")
        ]), conversationID: conversation)
        #expect(store.captureProblems.isEmpty, "an older in-flight snapshot must not revive a cleared warning")
    }

    @Test("payload loss stays on its tool while unattributed failures render inline")
    func localizedCaptureWarnings() {
        let run = ConversationRunID("run-1")
        var tool = item("tool", position: 2, kind: .tool)
        tool.tool = .init(title: "Inspect output", arguments: .null, locations: [], partialMetadata: false,
                          payloads: [.init(role: "output", availability: "partial", locallyTruncated: true)])
        var snapshot = ChatTranscriptSnapshot(normalizedItems: [item("before", position: 1), tool],
            captureProblems: [run: "Some transcript output was not saved"])
        var entries = ChatTranscriptAssembler.plan(snapshot)
        #expect(!entries.contains { $0.id == "capture/run-1" })
        guard case .card(.activityGroup(let activities, _)) = entries.last?.content else {
            Issue.record("Expected the actual tool group"); return
        }
        #expect(activities.count == 1)
        #expect(activities.first?.captureWarning == "Partial")
        snapshot.captureProblems[run] = "Some tool details could not be saved completely"
        #expect(!ChatTranscriptAssembler.plan(snapshot).contains { $0.id == "capture/run-1" })
        // When the affected tool is outside the loaded page, keep the precise
        // aggregate visible instead of implying that assistant text is lost.
        snapshot.normalizedItems = [item("before", position: 1)]
        #expect(ChatTranscriptAssembler.plan(snapshot).contains {
            $0.id == "capture/run-1" && $0.content == .markdown(
                "⚠︎ Some tool details could not be saved completely", syntax: .full, container: nil)
        })
        snapshot.captureProblems[run] = "Transcript could not be saved; retrying"
        entries = ChatTranscriptAssembler.plan(snapshot)
        #expect(entries.first?.id == "capture/run-1")
        guard case .markdown = entries.first?.content else {
            Issue.record("Unattributed failure must be inline text, not an activity card"); return
        }
    }

    @Test("recovery revisits loaded older items and stops at its coverage boundary")
    func boundedRecoveryCoverage() async throws {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [item("older", position: 50, body: "old")]), conversationID: conversation)
        var requests = 0
        try await store.resync(conversation) { cursor in
            requests += 1
            if cursor == nil {
                return .init(coreInstanceID: .init("core"), sequence: 5, items: [item("new", position: 100, body: "new")], nextCursor: "core:75", coverage: .init(startPosition: 75, endPosition: 100))
            }
            var updated = item("older", position: 50, body: "changed while disconnected")
            updated.revision = 2
            return .init(coreInstanceID: .init("core"), sequence: 5, items: [updated], nextCursor: "core:25", coverage: .init(startPosition: 25, endPosition: 74))
        }
        #expect(requests == 2)
        #expect(store.bodies["older"] == "changed while disconnected")
        #expect(store.nextCursor == "core:25")
    }
    @Test("append-only deltas keep the held preview, merged ranges are contiguous, and gaps resync")
    func appendOnlyDeltas() {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        var open = item("text", position: 1, body: "AB")
        open.completion = .open
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [open]), conversationID: conversation)
        func grow(_ sequence: UInt64, from start: UInt64? = nil, offset: UInt64, _ text: String) -> Bool {
            var header = open
            header.revision = sequence
            header.bodyByteCount = offset + UInt64(text.utf8.count)
            header.bodyPreview = ""
            header.previewOmitted = true
            return store.merge(ConversationTranscriptDeltaParams(workspaceID: .init("workspace"),
                conversationID: conversation, coreInstanceID: .init("core"), sequence: sequence, sequenceStart: start,
                items: [header], bodyAppends: [.init(itemID: "text", revision: sequence, offset: offset, text: text)]))
        }
        #expect(grow(2, offset: 2, "C") == false)
        #expect(store.bodies["text"] == "ABC")
        #expect(store.items["text"]?.bodyPreview == "AB")
        #expect(store.items["text"]?.previewOmitted == false)
        // Core merged sequences 3...5 into one frame.
        #expect(grow(5, from: 3, offset: 3, "DEF") == false)
        #expect(store.bodies["text"] == "ABCDEF")
        // A frame after a lost one asks for a resync.
        #expect(grow(8, from: 7, offset: 8, "XY") == true)
        #expect(store.bodies["text"] == "ABCDEF")
    }

    @Test("a body held only as its preview fills the gap a live append skips", arguments: [
        TranscriptItemKind.assistantText, .reasoning,
    ])
    func previewOnlyBodyFillsGap(kind: TranscriptItemKind) async {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        // A reload mid-stream: the page carries the preview, not the body.
        var open = item("text", position: 1, body: "AB")
        open.kind = kind
        open.completion = .open
        open.bodyByteCount = 4
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [open]), conversationID: conversation)
        var header = open
        header.revision = 2
        header.bodyByteCount = 5
        header.bodyPreview = ""
        header.previewOmitted = true
        _ = store.merge(ConversationTranscriptDeltaParams(workspaceID: .init("workspace"),
            conversationID: conversation, coreInstanceID: .init("core"), sequence: 2,
            items: [header], bodyAppends: [.init(itemID: "text", revision: 2, offset: 4, text: "E")]))
        #expect(store.bodies["text"] == "AB")
        for task in store.loadBodies(read: { _, id, revision, offset in
            #expect(offset == 2)
            return .init(itemID: id, revision: revision, text: "CDE")
        }) { await task.value }
        #expect(store.bodies["text"] == "ABCDE")
    }

    @Test("failed transcript loading preserves independent saved detail and can recover")
    func loadingFallback() throws {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<4 { root.deleteLastPathComponent() }
        let data = try Data(contentsOf: root.appendingPathComponent("backend/crates/ottam-protocol/fixtures/conversation-get-response.json"))
        let detail = try JSONDecoder().decode(ProtocolResponse<ConversationGetResult>.self, from: data).result!.conversation
        let model = ChatWorkspaceModel()
        let id = detail.conversation.conversationID
        model.selectedConversationID = id
        model.transcriptStore.select(id)
        model.applyLoadedDetail(detail, transcript: .failure(URLError(.cannotConnectToHost)))
        #expect(model.selectedDetail == detail)
        #expect(!model.isLoadingDetail)
        #expect(model.transcriptLoadError != nil)
        model.applyLoadedDetail(detail, transcript: .success(.init(coreInstanceID: .init("core"), sequence: 0, items: [])))
        #expect(model.transcriptLoadError == nil)
        #expect(model.selectedDetail == detail)
        model.selectedConversationID = .init("other")
        model.applyLoadedDetail(detail, transcript: .failure(URLError(.timedOut)))
        #expect(model.transcriptLoadError == nil, "a stale response must not affect a new selection")
    }

    @Test("diagnostic hover preserves errors without exposing ordinary tool metadata")
    func diagnosticHover() {
        #expect(ChatLiveActivity(label: "Incomplete transcript", detail: "Missed 3 updates", kind: "streamGap").hoverText == "Missed 3 updates")
        #expect(ChatLiveActivity(label: "Could not load saved text", detail: "Connection lost", kind: "transcriptError").hoverText == "Connection lost")
        #expect(ChatLiveActivity(label: "Read core.md", detail: "hidden tool metadata", kind: "read", filePath: "/tmp/core.md").hoverText == "/tmp/core.md")
        #expect(ChatLiveActivity(label: "Ran command", detail: "hidden tool metadata", kind: "execute").hoverText == "Ran command")
    }

    @Test("trailer copy requires a settled turn with text, including partial results")
    func trailerCopyAvailability() {
        #expect(ChatTurnTrailerPresentation.running(runID: .init("run"), status: .running, startedAt: nil, activityCount: 0).copyBody == nil)
        #expect(ChatTurnTrailerPresentation.finished(runID: .init("run"), body: "", outcome: .complete).copyBody == nil)
        for outcome in [ChatTurnOutcome.complete, .stopped, .steered, .endedEarly] {
            #expect(ChatTurnTrailerPresentation.finished(runID: .init("run"), body: "partial", outcome: outcome).copyBody == "partial")
        }
    }

    /// Every plan must have distinct IDs: the engine keys updates and card
    /// carry-forward on them.
    private func plan(_ snapshot: ChatTranscriptSnapshot) -> [TranscriptItem] {
        let entries = ChatTranscriptAssembler.plan(snapshot)
        #expect(Set(entries.map(\.id)).count == entries.count, "duplicate IDs in \(entries.map(\.id))")
        return entries
    }

    @Test("a user turn is attachments, bubble, trailer, then recovery")
    func userTurnOrder() {
        let message = TranscriptFixture.message(
            "m1", role: .user, body: "look", attachments: [TranscriptFixture.attachment("a1")]
        )
        let inputs = [
            TranscriptFixture.runInput("ri-hidden", messageID: "m1", status: .pending),
            TranscriptFixture.runInput("ri-failed", messageID: "m1", status: .failed),
            TranscriptFixture.runInput("ri-retry", messageID: "m1", kind: .retry, status: .pending),
        ]
        let entries = plan(.init(messages: [.init(message: message, runInputs: inputs)]))
        // One trailer, whatever the attempts number: it carries the latest
        // one-line state, and only the recovery-class outcome keeps its card.
        #expect(entries.map(\.id) == ["m1/attachments", "m1", "message/m1/trailer", "runinput/ri-retry"])
        #expect(entries[1].content == .markdown("look", syntax: .inline, container: .bubble))
        #expect(entries[2].cardSpec == TranscriptFixture.userTrailer(
            body: "look", input: inputs[1], showsResend: true
        ))
        guard case .card(.deliveryRecovery) = entries[3].content else {
            Issue.record("expected a recovery card, got \(entries.map(\.content))"); return
        }
    }

    @Test("a settled delivery leaves the trailer's state slot empty")
    func deliveredIsSilent() {
        // The decision this pins: a message the agent visibly answers needs no
        // badge asserting it arrived. `delivered` is a claim about the adapter's
        // handoff rather than the agent's attention, and `sending` outlives the
        // whole turn for a prompt — the run's own trailer covers that. The row
        // still appears; it just carries only its timestamp and copy button.
        for status in [RunInputStatus.pending, .sending, .delivered] {
            let input = TranscriptFixture.runInput("ri", messageID: "m1", status: status)
            let entries = plan(.init(messages: [
                .init(message: TranscriptFixture.message("m1", role: .user), runInputs: [input]),
            ]))
            #expect(entries.map(\.id) == ["m1", "message/m1/trailer"])
            #expect(entries[1].cardSpec == TranscriptFixture.userTrailer())
        }
    }

    @Test("an image-only user message has no bubble, but still has its trailer")
    func imageOnlyUser() {
        let message = TranscriptFixture.message("m1", role: .user, body: "", attachments: [TranscriptFixture.attachment("a1")])
        #expect(plan(.init(messages: [.init(message: message)])).map(\.id) == ["m1/attachments", "message/m1/trailer"])
    }

    private func item(_ id: String, position: UInt64, kind: TranscriptItemKind = .assistantText, body: String = "") -> ConversationTranscriptItem {
        .init(itemID: id, conversationID: TranscriptFixture.conversationID, runID: .init("run-1"),
              position: position, revision: 1, kind: kind, completion: .open,
              createdAt: "2026-09-07T10:00:00Z", updatedAt: "2026-09-07T10:00:00Z",
              bodyPreview: body, bodyByteCount: UInt64(body.utf8.count))
    }

    @Test("Core positions interleave text and rejected steering without moving either")
    func normalizedSteeringOrder() {
        let user = TranscriptFixture.message("user", role: .user, body: "hello", position: 0)
        let steer = TranscriptFixture.message("steer", role: .user, body: "change", position: 2)
        let input = TranscriptFixture.runInput("rejected", messageID: "steer", status: .failed)
        let snapshot = ChatTranscriptSnapshot(messages: [.init(message: user), .init(message: steer, runInputs: [input])],
            normalizedItems: [item("B", position: 3, body: "After"), item("A", position: 1, body: "Before")])
        let ids = plan(snapshot).map(\.id)
        #expect(ids.firstIndex(of: "A")! < ids.firstIndex(of: "steer")!)
        #expect(ids.firstIndex(of: "steer")! < ids.firstIndex(of: "B")!)
        #expect(plan(snapshot) == plan(snapshot))
    }

    @Test("read filenames retain absolute hover paths and resolve relative paths")
    func readFilePaths() {
        let root = URL(fileURLWithPath: "/Users/example/Developer/ottam", isDirectory: true)
        #expect(ChatTranscriptAssembler.readFilePath("Read file 'docs/core.md'", root: root)
            == "/Users/example/Developer/ottam/docs/core.md")
        #expect(ChatTranscriptAssembler.readFilePath("Read file '/tmp/My Project/a.md'", root: nil)
            == "/tmp/My Project/a.md")
        #expect(ChatTranscriptAssembler.readFilePath("Read file 'docs/core.md'", root: nil) == nil)
        #expect(ChatTranscriptAssembler.readFilePath("Search files", root: root) == nil)
        var read = item("read", position: 1, kind: .tool)
        read.tool = .init(kind: "read", title: "Read file '/tmp/My Project/a.md'", arguments: .null,
                          locations: [], partialMetadata: false, payloads: [])
        guard case .card(.activityGroup(let activities, _)) = plan(.init(normalizedItems: [read]))[0].content else {
            Issue.record("expected read activity"); return
        }
        #expect(activities[0].label == "Read a.md")
        #expect(activities[0].filePath == "/tmp/My Project/a.md")
    }

    @Test("generic read tools use arguments and reflect active status")
    func genericReadTool() {
        var read = item("read", position: 1, kind: .tool, body: #"{"path":"/tmp/core.md"}"#)
        read.tool = .init(kind: "read", title: "read", status: "in_progress", arguments: .null,
                          locations: [], partialMetadata: false, payloads: [])
        guard case .card(.activityGroup(let activities, _)) = plan(.init(normalizedItems: [read]))[0].content else {
            Issue.record("expected read activity"); return
        }
        #expect(activities[0].label == "Reading core.md")
        #expect(activities[0].filePath == "/tmp/core.md")
    }

    @Test("a tool name sits in the detail without replacing a distinct title")
    func namedTool() {
        // Claude's shell call: SDK 2.x carries `name`, the adapter's title differs.
        var shell = item("shell", position: 1, kind: .tool, body: #"{"command":"ls"}"#)
        shell.tool = .init(name: "Bash", kind: "execute", title: "Terminal", arguments: .null,
                           locations: [], partialMetadata: false, payloads: [])
        guard case .card(.activityGroup(let activities, _)) = plan(.init(normalizedItems: [shell]))[0].content else {
            Issue.record("expected tool activity"); return
        }
        #expect(activities[0].label == "Terminal")
        #expect(activities[0].detail?.hasPrefix("Tool: Bash\nKind: execute") == true)
    }

    @Test("a captioned call reads as its caption; an MCP call names its tool beside it")
    func captionedTools() {
        func activity(_ tool: TranscriptTool, body: String) -> ChatLiveActivity? {
            var row = item("tool", position: 1, kind: .tool, body: body)
            row.tool = tool
            guard case .card(.activityGroup(let activities, _)) = plan(.init(normalizedItems: [row]))[0].content else { return nil }
            return activities.first
        }
        // Claude's shell call: the model's description; the command stays in the detail.
        let shell = activity(.init(name: "Bash", kind: "execute", title: "git status", arguments: .null,
                                   locations: [], partialMetadata: false, payloads: []),
                             body: #"{"command":"git status","description":"Show working tree status"}"#)
        #expect(shell?.label == "Show working tree status")
        #expect(shell?.subtitle == nil)
        // Without a description the command stays the title.
        let bare = activity(.init(name: "Bash", kind: "execute", title: "git status", arguments: .null,
                                  locations: [], partialMetadata: false, payloads: []),
                            body: #"{"command":"git status"}"#)
        #expect(bare?.label == "git status")
        #expect(bare?.subtitle == nil)
        // Oh-My-Pi's `xd://` MCP call, as core names it: omp's caption, then the tool.
        let mcp = activity(.init(name: "mcp__ottam_get_conversation_info", kind: "other", title: "Checking conversation model info",
                                 arguments: .null, locations: [], partialMetadata: false, payloads: []),
                           body: "{}")
        #expect(mcp?.label == "Checking conversation model info")
        #expect(mcp?.subtitle == "ottam · get_conversation_info")
        // Ottam's own delegation tools keep their titles, with nothing beside them.
        let parent = activity(.init(name: "mcp__ottam_message_parent", kind: "other", title: "Message parent done",
                                    arguments: .null, locations: [], partialMetadata: false, payloads: []),
                              body: #"{"message":"done"}"#)
        #expect(parent?.label == "Messaged the delegating agent")
        #expect(parent?.subtitle == nil)

        #expect(ChatTranscriptAssembler.mcpToolLabel("mcp__github__create_issue") == "github · create_issue")
        #expect(ChatTranscriptAssembler.mcpToolLabel("mcp__ottam_get_conversation_info") == "ottam · get_conversation_info")
        #expect(ChatTranscriptAssembler.mcpToolLabel("mcp__ottam__delegate") == "ottam · delegate")
        // A user's server that only starts with `ottam_` keeps its own name.
        #expect(ChatTranscriptAssembler.mcpToolLabel("mcp__ottam_notes__search") == "ottam_notes · search")
        #expect(ChatTranscriptAssembler.mcpToolLabel("mcp__context_mode_search") == nil)
        #expect(ChatTranscriptAssembler.mcpToolLabel("Bash") == nil)
    }

    @Test("outputless and older Runs retain trailers without a loaded Run page")
    func durableRunBoundary() {
        var boundary = item("boundary", position: 2, kind: .runBoundary)
        boundary.completion = .partial
        boundary.runStatus = .canceled
        var narration = item("narration", position: 1, body: "hidden pre-agent narration")
        narration.completion = .suppressed
        let entries = plan(.init(normalizedItems: [narration, boundary]))
        #expect(entries.map(\.id) == ["run/run-1/trailer"])
        #expect(entries[0].cardSpec == .turnTrailer(.persisted(runID: .init("run-1"), body: "",
            createdAt: boundary.updatedAt, outcome: .stopped)))
        let racing = plan(.init(liveRun: TranscriptFixture.liveRun("run-1"), normalizedItems: [narration, boundary]))
        #expect(racing == entries, "a delayed live-status notification must not create a second trailer")
    }

    @Test("a finished turn's trailer carries its run's usage")
    func trailerCarriesUsage() {
        var boundary = item("boundary", position: 2, kind: .runBoundary)
        boundary.runStatus = .canceled
        let text = item("text", position: 1, body: "partial")
        let entries = plan(.init(runUsage: [.init("run-1"): .unavailable], normalizedItems: [text, boundary]))
        #expect(entries.last?.cardSpec == .turnTrailer(.persisted(
            runID: .init("run-1"), body: "partial", createdAt: boundary.updatedAt,
            outcome: .stopped, usage: .unavailable
        )))
    }

    @Test("a turn that ends on a declined plan gets no Stopped chip; a declined tool keeps it")
    func declinedPlanEndsWithoutChip() {
        func trailerOutcome(plan markdown: String?, outcomeLabel: String = "No, keep planning") -> ChatTurnOutcome? {
            var permission = item("permission", position: 1, kind: .permission)
            permission.permission = .init(requestID: "r1", title: "Approve Plan", outcomeLabel: outcomeLabel,
                                          allowed: false, plan: markdown)
            var boundary = item("boundary", position: 2, kind: .runBoundary)
            boundary.runStatus = .canceled
            guard case .turnTrailer(.persisted(_, _, _, let outcome, _, _, _)) = plan(.init(normalizedItems: [permission, boundary])).last?.cardSpec
            else { return nil }
            return outcome
        }
        #expect(trailerOutcome(plan: "# Plan") == .complete)
        #expect(trailerOutcome(plan: nil) == .stopped)
        #expect(trailerOutcome(plan: "# Plan", outcomeLabel: "Cancelled") == .stopped, "Stop with a plan pending")
    }

    @Test("a run refused before the agent took the turn leaves no trailer; the input row says why")
    func refusedRunLeavesNoTrailer() {
        var boundary = item("boundary", position: 1, kind: .runBoundary)
        boundary.completion = .partial
        boundary.runStatus = .failed
        let message = TranscriptFixture.message("m1", role: .user, body: "hello")
        let input = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed, targetRunID: "run-1")
        let reason = "This agent no longer offers the model \"opus[1m]\"."
        let entries = plan(.init(
            messages: [.init(message: message, runInputs: [input])],
            runFailures: [.init("run-1"): reason],
            normalizedItems: [boundary]
        ))
        #expect(entries.map(\.id) == ["m1", "message/m1/trailer"])
        guard case .card(.userTrailer(let row)) = entries[1].content else {
            Issue.record("expected the user row, got \(entries.map(\.content))"); return
        }
        #expect(row.failureReason == reason)
    }

    @Test("a run that failed after output keeps its trailer, labeled Failed with the reason")
    func failedRunTrailer() {
        var boundary = item("boundary", position: 2, kind: .runBoundary)
        boundary.completion = .partial
        boundary.runStatus = .failed
        let text = item("text", position: 1, body: "partial")
        let entries = plan(.init(runFailures: [.init("run-1"): "Adapter exited"], normalizedItems: [text, boundary]))
        #expect(entries.dropLast().last?.cardSpec == .turnTrailer(.persisted(
            runID: .init("run-1"), body: "partial", createdAt: boundary.updatedAt,
            outcome: .failed(reason: "Adapter exited")
        )))
        #expect(entries.last?.cardSpec == .runFailure(.init(runID: .init("run-1"), reason: "Adapter exited")))
        #expect(ChatTurnOutcome.failed(reason: "Adapter exited").label == "Failed")
        #expect(ChatTurnOutcome.failed(reason: "Adapter exited").explanation == "Adapter exited")
    }

    @Test("failure notices survive reload and keep retry only on the latest eligible turn")
    func persistentFailureNotice() {
        var boundary = item("boundary", position: 2, kind: .runBoundary)
        boundary.completion = .partial
        boundary.runStatus = .failed
        let runID = ConversationRunID("run-1")
        var snapshot = ChatTranscriptSnapshot(
            messages: [.init(
                message: TranscriptFixture.message("m", role: .user),
                runInputs: [TranscriptFixture.runInput("input", messageID: "m", status: .failed, targetRunID: "run-1")]
            )],
            runFailures: [runID: "Internal error: Go usage limit exceeded"],
            retryableFailedRunID: runID,
            normalizedItems: [item("text", position: 1, body: "partial review"), boundary]
        )
        let entries = plan(snapshot)
        #expect(entries.contains(where: { $0.markdownText == "partial review" }))
        let userRow = entries.compactMap(\.cardSpec).compactMap { spec -> ChatUserTrailerPresentation? in
            if case .userTrailer(let row) = spec { return row }; return nil
        }.first
        #expect(userRow?.showsResend == false, "the failure notice owns recovery after output, even if the input is failed")
        #expect(entries.last?.cardSpec == .runFailure(.init(runID: runID, reason: snapshot.runFailures[runID], canRetry: true)))
        #expect(entries == plan(snapshot), "reloaded saved state needs no transient failure event")
        snapshot.retryableFailedRunID = nil
        #expect(plan(snapshot).last?.cardSpec == .runFailure(.init(runID: runID, reason: snapshot.runFailures[runID])))
        #expect(snapshot.failureOwnedInputIDs == [.init("input")], "historical execution failures must not offer replay on their original input")
        snapshot.runFailures = [:]
        #expect(plan(snapshot).last?.cardSpec == .runFailure(.init(runID: runID, reason: nil)))
    }

    @Test("a resend swaps in the composer's pick only for a setting the engine stopped offering")
    func retrySelectionsReplaceOnlyStaleValues() {
        var engine = TranscriptFixture.engine()
        engine.models = [.init(modelID: "opus", displayName: "Opus 5.5")]
        engine.modes = [.init(id: "plan", displayName: "Plan"), .init(id: "auto", displayName: "Auto")]
        let run = ConversationRunSummary(
            runID: .init("run-1"), conversationID: TranscriptFixture.conversationID,
            engineID: engine.engineID, modelID: "opus[1m]", modeID: "plan", status: .failed,
            startedAt: "2026-09-23T17:27:49Z", cost: .object([:]), error: .object([:])
        )
        let picks: ChatWorkspaceModel.RetrySelections = ("opus", "auto", nil)
        let retry = ChatWorkspaceModel.retrySelections(for: run, refused: true, engine: engine, picks: picks)
        #expect(retry.modelID == "opus", "the stale model takes the composer's pick")
        #expect(retry.modeID == "plan", "a still-offered mode replays, whatever the composer says")
        #expect(retry.thoughtLevelID == nil)
        let otherEngine = ChatWorkspaceModel.retrySelections(
            for: run, refused: true, engine: engine, picks: (nil, nil, nil)
        )
        #expect(otherEngine.modelID == nil, "with the composer on another engine, the engine default")
        let unknown = ChatWorkspaceModel.retrySelections(for: run, refused: true, engine: nil, picks: picks)
        #expect(unknown.modelID == "opus[1m]", "an unlisted engine cannot prove a value stale")
    }

    @Test("effort levels and default follow the model the agent reported them for")
    func thoughtLevelsFollowTheModel() {
        let level = { (id: String) in EngineSelectOption(id: id, displayName: id) }
        var engine = TranscriptFixture.engine()
        engine.defaultModelID = "kimi"
        engine.thoughtLevels = [level("low"), level("high")]
        engine.defaultThoughtLevelID = "high"
        engine.models = [
            .init(modelID: "kimi", displayName: "Kimi"),
            .init(modelID: "glm", displayName: "GLM", thoughtLevels: [level("low"), level("max")],
                  defaultThoughtLevelID: "max"),
            .init(modelID: "pickle", displayName: "Pickle", thoughtLevels: []),
            .init(modelID: "fresh", displayName: "Fresh")
        ]
        #expect(engine.thoughtLevelSelect(forModel: nil).defaultID == "high",
                "the engine-wide select was observed on the default model")
        #expect(engine.thoughtLevelSelect(forModel: "kimi").defaultID == "high")
        let glm = engine.thoughtLevelSelect(forModel: "glm")
        #expect(glm.levels.map(\.id) == ["low", "max"])
        #expect(glm.defaultID == "max", "omp runs glm at its own default, not the engine's")
        #expect(engine.thoughtLevelSelect(forModel: "pickle").levels.isEmpty, "this model has no effort")
        let fresh = engine.thoughtLevelSelect(forModel: "fresh")
        #expect(fresh.levels.map(\.id) == ["low", "high"])
        #expect(fresh.defaultID == nil, "an unobserved model's default is unknown")
    }

    @Test("with model and mode both stale, a resend swaps only the model the card names")
    func retrySelectionsReplaceOneStaleValuePerResend() {
        var engine = TranscriptFixture.engine()
        engine.models = [.init(modelID: "opus", displayName: "Opus 5.5")]
        engine.modes = [.init(id: "auto", displayName: "Auto")]
        let run = ConversationRunSummary(
            runID: .init("run-1"), conversationID: TranscriptFixture.conversationID,
            engineID: engine.engineID, modelID: "opus[1m]", modeID: "plan", status: .failed,
            startedAt: "2026-09-23T17:27:49Z", cost: .object([:]), error: .object([:])
        )
        let retry = ChatWorkspaceModel.retrySelections(
            for: run, refused: true, engine: engine, picks: ("opus", "auto", nil)
        )
        #expect(retry.modelID == "opus")
        #expect(retry.modeID == "plan", "the read-only mode is never widened by an undisclosed swap")
    }

    @Test("a resend of a run that failed for another reason replays exactly, stale mode and all")
    func retrySelectionsReplayUnrefusedRuns() {
        var engine = TranscriptFixture.engine()
        engine.modes = [.init(id: "auto", displayName: "Auto")]
        let run = ConversationRunSummary(
            runID: .init("run-1"), conversationID: TranscriptFixture.conversationID,
            engineID: engine.engineID, modeID: "plan", status: .failed,
            startedAt: "2026-09-23T17:27:49Z", cost: .object([:]), error: .object([:])
        )
        let retry = ChatWorkspaceModel.retrySelections(for: run, refused: false, engine: engine, picks: (nil, "auto", nil))
        #expect(retry.modeID == "plan", "a mode never changes behind a resend no card disclosed")
    }

    @Test("a refusal with nothing nameable as stale resends unchanged from the card, not the row")
    func selectionRecoveryWithoutStaleValue() {
        let input = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed, targetRunID: "run-1")
        let detail = "Could not restore permission mode \"plan\". Choose an available mode and resend."
        let recovery = ChatSelectionRecovery(
            engine: TranscriptFixture.engine(), setting: nil, staleValue: nil,
            replacement: "Default", detail: detail, undeliveredInput: input
        )
        #expect(recovery.canResend, "a transient restore failure must stay resendable")
        #expect(recovery.sendAgainTitle == "Send Again", "nothing is replaced, so the title names nothing")
        #expect(recovery.explanation == detail)
        let entries = plan(.init(messages: [.init(
            message: TranscriptFixture.message("m1", role: .user, body: "hello"),
            runInputs: [input],
            selectionRecovery: recovery
        )]))
        #expect(entries[1].cardSpec == TranscriptFixture.userTrailer(input: input, showsResend: false))
    }

    @Test("a refused run stays trailer-free after its input is superseded by a retry")
    func supersededRefusedRunLeavesNoTrailer() {
        var boundary = item("boundary", position: 1, kind: .runBoundary)
        boundary.completion = .partial
        boundary.runStatus = .failed
        let message = TranscriptFixture.message("m1", role: .user, body: "hello")
        let inputs = [
            TranscriptFixture.runInput("ri", messageID: "m1", status: .superseded, targetRunID: "run-1"),
            TranscriptFixture.runInput("ri-retry", messageID: "m1", kind: .retry, status: .pending),
        ]
        let entries = plan(.init(
            messages: [.init(message: message, runInputs: inputs)],
            runFailures: [.init("run-1"): "refused"],
            normalizedItems: [boundary]
        ))
        #expect(!entries.contains { $0.id == "run/run-1/trailer" })
    }

    @Test("the selection card names the stale value and what Send Again runs with")
    func selectionRecoveryCopy() {
        let input = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed, targetRunID: "run-1")
        let recovery = ChatSelectionRecovery(
            engine: TranscriptFixture.engine(), setting: .model, staleValue: "opus[1m]",
            replacement: "Opus 5.5", detail: nil, undeliveredInput: input
        )
        #expect(recovery.title == "Model no longer available")
        #expect(recovery.explanation.contains("Claude Code no longer offers “opus[1m]”"))
        #expect(recovery.explanation.contains("Choose an available model"))
        #expect(recovery.sendAgainTitle == "Send Again with Opus 5.5")
        let entries = plan(.init(messages: [.init(
            message: TranscriptFixture.message("m1", role: .user, body: "hello"),
            runInputs: [input],
            selectionRecovery: recovery
        )]))
        #expect(entries.map(\.id) == ["m1", "message/m1/trailer", "m1/selection-recovery"])
        #expect(entries[1].cardSpec == TranscriptFixture.userTrailer(input: input, showsResend: false),
                "the card owns the resend, so the row offers none")
        #expect(entries[2].cardSpec == .selectionRecovery(recovery, isMutating: false))
    }

    @Test("short reasoning headings occupy the activity line without a duplicate detail")
    func normalizedReasoning() {
        let reasoning = item("reasoning", position: 1, kind: .reasoning, body: "\n\n**Planning architecture overview reading**")
        let entries = plan(.init(normalizedItems: [reasoning]))
        guard case .card(.activityGroup(let activities, _)) = entries[0].content else {
            Issue.record("expected activity disclosure"); return
        }
        #expect(activities[0].id == "reasoning")
        #expect(activities[0].label == "Planning architecture overview reading")
        #expect(activities[0].detail == nil)
        var extended = reasoning
        extended.bodyPreview += "\n\nKeep this additional explanation."
        guard case .card(.activityGroup(let expanded, _)) = plan(.init(normalizedItems: [extended]))[0].content else {
            Issue.record("expected activity disclosure"); return
        }
        #expect(expanded[0].label == activities[0].label)
        #expect(expanded[0].detail == "Keep this additional explanation.")
    }

    @Test("activity grouping never crosses an intervening user message")
    func normalizedGroupsRespectMessages() {
        let first = item("reasoning-a", position: 1, kind: .reasoning, body: "A")
        let second = item("reasoning-b", position: 3, kind: .reasoning, body: "B")
        let steer = TranscriptFixture.message("steer", role: .user, position: 2)
        #expect(plan(.init(messages: [.init(message: steer)], normalizedItems: [first, second])).map(\.id)
            == ["reasoning-a/group", "steer", "message/steer/trailer", "reasoning-b/group"])
    }

    @Test("the trailing activity group stays active between calls until output moves past it")
    func normalizedActivityBurstLifetime() {
        var thought = item("thought", position: 1, kind: .reasoning, body: "Checking")
        thought.completion = .complete
        let live = TranscriptFixture.liveRun("run-1")
        func states(_ snapshot: ChatTranscriptSnapshot) -> [Bool] {
            plan(snapshot).compactMap {
                if case .card(.activityGroup(_, let active)) = $0.content { return active }
                return nil
            }
        }
        #expect(states(.init(liveRun: live, normalizedItems: [thought])) == [true])
        let answer = item("answer", position: 2, body: "Continuing")
        #expect(states(.init(liveRun: live, normalizedItems: [thought, answer])) == [false])
        let next = item("next-thought", position: 3, kind: .reasoning, body: "Next step")
        #expect(states(.init(liveRun: live, normalizedItems: [thought, answer, next])) == [false, true])
        #expect(states(.init(normalizedItems: [thought])) == [false])
        let steer = TranscriptFixture.message("steer", role: .user, position: 2)
        #expect(states(.init(messages: [.init(message: steer)], liveRun: live, normalizedItems: [thought])) == [false])
    }

    @Test("item revisions update content without changing renderer identity")
    func normalizedRevisionIdentity() {
        var text = item("text", position: 1, body: "A")
        let initial = plan(.init(normalizedItems: [text]))
        text.revision = 2
        text.bodyPreview = "AB"
        text.bodyByteCount = 2
        let updated = plan(.init(normalizedItems: [text]))
        #expect(initial.map(\.id) == updated.map(\.id))
        #expect(updated[0].content == .markdown("AB", syntax: .full, container: nil))
    }

    @Test("completed normalized turns retain their saved timestamp in the trailer")
    func normalizedTrailerTimestamp() {
        var answer = item("answer", position: 1, body: "Saved answer")
        answer.completion = .complete
        answer.updatedAt = "2026-09-07T10:01:00Z"
        let entries = plan(.init(runStatuses: [.init("run-1"): .completed], normalizedItems: [answer]))
        #expect(entries.last?.cardSpec == .turnTrailer(.persisted(
            runID: .init("run-1"), body: "Saved answer",
            createdAt: "2026-09-07T10:01:00Z", outcome: .complete
        )))
    }

    @Test("turn copy uses only the fully loaded final assistant message")
    func finalMessageCopy() {
        let earlier = item("earlier", position: 1, body: "I will investigate.")
        var final = item("final", position: 3, body: "Final")
        let full = "Final answer 🐈"
        final.bodyByteCount = UInt64(full.utf8.count)
        var snapshot = ChatTranscriptSnapshot(runStatuses: [.init("run-1"): .completed],
            normalizedItems: [earlier, final])
        func copyBody() -> String? {
            guard case .turnTrailer(let trailer) = plan(snapshot).compactMap(\.cardSpec).last(where: {
                if case .turnTrailer = $0 { return true }; return false
            }) else { return nil }
            return trailer.copyBody
        }
        #expect(copyBody() == nil)
        snapshot.normalizedBodies["final"] = full
        #expect(copyBody() == full)
        // Earlier history need not be loaded to copy the final message.
        snapshot.normalizedItems = [final]
        #expect(copyBody() == full)
        snapshot.bodyErrors["final"] = "Read failed"
        #expect(copyBody() == nil)
        snapshot.bodyErrors = [:]
        for status in [ConversationRunStatus.canceled, .interrupted, .failed] {
            snapshot.runStatuses[.init("run-1")] = status
            #expect(copyBody() == full)
        }
    }

    @Test("the normalized cache ignores stale revisions and clears a previous Core")
    func normalizedStoreMerge() {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        let first = item("text", position: 1, body: "A")
        store.merge(.init(coreInstanceID: .init("core-1"), sequence: 1, items: [first]), conversationID: conversation)
        var newer = first
        newer.revision = 2
        newer.bodyPreview = "AB"
        newer.bodyByteCount = 2
        #expect(!store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core-1"), sequence: 2, items: [newer], bodyAppends: [])))
        store.merge(.init(coreInstanceID: .init("core-1"), sequence: 1, items: [first]), conversationID: conversation)
        #expect(store.items["text"]?.revision == 2)
        #expect(store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core-2"), sequence: 1, items: [], bodyAppends: [])))
        #expect(store.items.isEmpty)
        #expect(store.bodies.isEmpty)
    }

    @Test("gap recovery scans older pages without clobbering racing live revisions")
    func gapRecovery() async throws {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        var old = item("old", position: 1, body: "A")
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [old]), conversationID: conversation)
        #expect(store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 3, items: [], bodyAppends: [])))
        var cursors: [String?] = []
        try await store.resync(conversation) { cursor in
            cursors.append(cursor)
            if cursor == nil {
                old.revision = 3
                old.bodyPreview = "Newest"
                old.bodyByteCount = 6
                _ = store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
                    coreInstanceID: .init("core"), sequence: 4, items: [old], bodyAppends: []))
                return .init(coreInstanceID: .init("core"), sequence: 3, items: [], nextCursor: "older")
            }
            return .init(coreInstanceID: .init("core"), sequence: 3, items: [item("old", position: 1, body: "stale")])
        }
        #expect(cursors == [nil, "older"])
        #expect(store.bodies["old"] == "Newest")
        #expect(store.sequence == 4)
        try await store.resync(conversation) { _ in
            .init(coreInstanceID: .init("restarted"), sequence: 0, items: [])
        }
        #expect(store.coreInstanceID == .init("restarted"))
        #expect(store.items.isEmpty)
    }

    @Test("body read failures surface and a successful retry clears the error")
    func bodyReadFailureAndRetry() async {
        let store = ChatTranscriptStore()
        var text = item("text", position: 1, body: "A")
        text.bodyByteCount = 2
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [text]), conversationID: text.conversationID)
        for task in store.loadBodies(read: { _, _, _, _ in throw URLError(.cannotConnectToHost) }) { await task.value }
        #expect(store.bodyErrors["text"] != nil)
        for task in store.loadBodies(read: { _, id, revision, offset in
            #expect(offset == 1)
            return .init(itemID: id, revision: revision, text: "B")
        }) { await task.value }
        #expect(store.bodies["text"] == "AB")
        #expect(store.bodyErrors["text"] == nil)
    }

    @Test("a late body page cannot cross a conversation switch")
    func bodyReadSelectionCancellation() async {
        let store = ChatTranscriptStore()
        var text = item("text", position: 1, body: "A")
        text.bodyByteCount = 2
        store.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [text]), conversationID: text.conversationID)
        let (started, startedSignal) = AsyncStream<Void>.makeStream()
        let (responses, responseSignal) = AsyncStream<String>.makeStream()
        let tasks = store.loadBodies { _, id, revision, _ in
            startedSignal.yield(())
            var iterator = responses.makeAsyncIterator()
            return .init(itemID: id, revision: revision, text: await iterator.next() ?? "")
        }
        var starts = started.makeAsyncIterator()
        await starts.next()
        store.select(.init("other-conversation"))
        responseSignal.yield("B")
        responseSignal.finish()
        startedSignal.finish()
        for task in tasks { await task.value }
        #expect(store.bodies.isEmpty)
        #expect(store.bodyErrors.isEmpty)
    }

    @Test("live body appends merge preview overlaps, duplicates, gaps, and stale revisions")
    func liveBodyAppends() {
        let store = ChatTranscriptStore()
        let conversation = TranscriptFixture.conversationID
        var text = item("text", position: 1, body: "é")
        text.bodyByteCount = 8
        store.select(conversation)
        let delta = ConversationTranscriptDeltaParams(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 1, items: [text], bodyAppends: [
                .init(itemID: "text", revision: 1, offset: 0, text: "é🧠ab")
            ])
        #expect(!store.merge(delta))
        #expect(store.bodies["text"] == "é🧠ab")
        #expect(!store.merge(delta))
        #expect(store.bodies["text"] == "é🧠ab")
        text.revision = 3
        text.bodyByteCount = 12
        #expect(store.merge(.init(workspaceID: .init("workspace"), conversationID: conversation,
            coreInstanceID: .init("core"), sequence: 3, items: [text], bodyAppends: [
                .init(itemID: "text", revision: 3, offset: 10, text: "ef")
            ])))
        #expect(store.bodies["text"] == "é🧠ab") // missing prefix requires paged recovery
        #expect(!store.merge(delta))
        #expect(store.items["text"]?.revision == 3)
        #expect(store.bodies["text"] == "é🧠ab")
    }

    @Test("system messages are plain inline markdown")
    func systemMessage() {
        let entries = plan(.init(messages: [.init(message: TranscriptFixture.message("s1", role: .system, body: "Run cancelled"))]))
        #expect(entries == [.init(id: "s1", content: .markdown("Run cancelled", syntax: .inline, container: nil))])
    }

    @Test("recovery cards trail the message that carries them")
    func recoveryCards() {
        let recovery = ChatAuthRecovery(engine: TranscriptFixture.engine(), phase: .signedOut, undeliveredInput: nil)
        let runtime = ChatRuntimeRecovery(engine: TranscriptFixture.engine(), problem: .missing(component: "node"), detail: nil)
        let message = TranscriptFixture.message("m2", role: .user, body: "Start this task")
        let entries = plan(.init(messages: [.init(message: message, authRecovery: recovery, runtimeRecovery: runtime)]))
        #expect(entries.map(\.id) == ["m2", "message/m2/trailer", "m2/runtime-recovery", "m2/auth-recovery"])
        #expect(entries[2].cardSpec == .runtimeRecovery(runtime, install: nil, isChecking: false))
        #expect(entries[3].cardSpec == .authRecovery(recovery, failure: nil, isTerminalLogin: false, isMutating: false))
    }

    @Test("the turn-wide flags ride into every card that reads them")
    func flagsReachCards() {
        let input = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed)
        let recovery = ChatAuthRecovery(engine: TranscriptFixture.engine(), phase: .signedOut, undeliveredInput: input)
        var snapshot = ChatTranscriptSnapshot(messages: [
            .init(message: TranscriptFixture.message("m1", role: .user), runInputs: [input], authRecovery: recovery),
        ])
        snapshot.isMutating = true
        let entries = plan(snapshot)
        // The auth card owns this input's resend, and a live run would forbid it anyway.
        #expect(entries[1].cardSpec == TranscriptFixture.userTrailer(input: input, isMutating: true))
        // Without the auth card the resend is offered — until a run is live.
        snapshot.messages[0].authRecovery = nil
        snapshot.isMutating = false
        #expect(plan(snapshot)[1].cardSpec == TranscriptFixture.userTrailer(input: input, showsResend: true))
        snapshot.liveRun = TranscriptFixture.liveRun("run-9")
        #expect(plan(snapshot)[1].cardSpec == TranscriptFixture.userTrailer(input: input))
    }

    @Test("the resend has one owner, wherever the auth card hangs")
    func resendOwnership() {
        let input = TranscriptFixture.runInput("ri", messageID: "m1", status: .failed)
        let recovery = ChatAuthRecovery(engine: TranscriptFixture.engine(), phase: .signedOut, undeliveredInput: input)
        // The card trails a later assistant turn — the narrated failure an
        // older Core persisted — and still owns the user turn's resend.
        var snapshot = ChatTranscriptSnapshot(messages: [
            .init(message: TranscriptFixture.message("m1", role: .user), runInputs: [input]),
            .init(message: TranscriptFixture.message("m2", role: .system, body: "Failed to authenticate"), authRecovery: recovery),
        ])
        #expect(plan(snapshot)[1].cardSpec == TranscriptFixture.userTrailer(input: input))
        // A signed-out engine withholds the offer even with no card in sight.
        snapshot.messages[1].authRecovery = nil
        snapshot.unsendableInputIDs = [input.runInputID]
        #expect(plan(snapshot)[1].cardSpec == TranscriptFixture.userTrailer(input: input))
    }

    @Test("superseded inputs leave no row behind")
    func supersededInputsAreHidden() {
        let original = TranscriptFixture.runInput("ri-old", messageID: "m1", status: .superseded)
        let retry = TranscriptFixture.runInput("ri-new", messageID: "m1", kind: .retry, status: .failed)
        let entries = plan(.init(messages: [
            .init(message: TranscriptFixture.message("m1", role: .user), runInputs: [original, retry]),
        ]))
        #expect(entries.map(\.id) == ["m1", "message/m1/trailer"])
    }

    @Test("pending permissions come last and compare by identity")
    func pendingPermissionsLast() {
        let permission = HostedPermissionCardTests.previewPermission()
        let live = TranscriptFixture.liveRun("run-9")
        let snapshot = TranscriptFixture.snapshot(liveRun: live, pending: [permission])
        let entries = plan(snapshot)
        #expect(entries.map(\.id) == [
            "m1", "message/m1/trailer", "answer",
            "run/run-9/trailer",
            "permission/\(permission.id.uuidString)",
        ])
        #expect(entries.last?.cardSpec == .pendingPermission(permission))
        #expect(entries.last?.cardSpec != .pendingPermission(HostedPermissionCardTests.previewPermission()))
        #expect(ChatTranscriptAssembler.plan(snapshot) == entries, "planning is deterministic")
    }

    @Test("a delegation key matches core's for the same prompt")
    func delegationKeyMatchesCore() {
        // The first 16 hex digits of SHA-256("Review app/Package.swift").
        #expect(ChatTranscriptAssembler.delegationKey("  Review app/Package.swift\n") == "7331e98c99c4c150")
    }

    @Test("a delegate call whose input is its body names the agent and shows its child")
    func delegationCallReadsBody() {
        let body = #"{"agent":"review-bot","prompt":"Review app/Package.swift"}"#
        var item = TranscriptFixture.item("call", body: body)
        item.kind = .tool
        item.tool = .init(name: "mcp__ottam__delegate", title: "mcp__ottam__delegate", status: "completed",
                          arguments: .null, locations: [], partialMetadata: false, payloads: [])
        var snapshot = ChatTranscriptSnapshot()
        let starting = ChatTranscriptAssembler.delegationCall(item, snapshot: snapshot)
        #expect(starting == ChatDelegationPresentation(kind: .delegated(agent: "review-bot", failed: false, child: nil, stopCause: nil)))
        #expect(starting?.title == "Delegated to review-bot")
        #expect(starting?.subtitle == "review-bot · Starting")

        let activity = DelegationActivity(latestRunStatus: .running, commands: 10, reads: 1, edits: 0, searches: 0,
                                          fetches: 0, other: 0, subagents: 2)
        snapshot.delegatedChildren["7331e98c99c4c150"] = [ChatDelegatedChild(
            conversationID: ConversationID("child"), agentName: "review-bot", title: "Package review", activity: activity
        )]
        let working = ChatTranscriptAssembler.delegationCall(item, snapshot: snapshot)
        #expect(working?.openConversationID == ConversationID("child"))
        #expect(working?.title == "Package review")
        #expect(working?.subtitle == "review-bot · Working · 10 commands, 1 read, 2 subagents")

        snapshot.delegatedChildren["7331e98c99c4c150"]?[0].needsApproval = true
        #expect(ChatTranscriptAssembler.delegationCall(item, snapshot: snapshot)?.state == "Approval required")

        // A stopped child says why, as its latest report does.
        snapshot.delegatedChildren["7331e98c99c4c150"]?[0].needsApproval = false
        snapshot.delegatedChildren["7331e98c99c4c150"]?[0].activity?.latestRunStatus = .canceled
        var noteMessage = TranscriptFixture.message("note", role: .system, body: "", position: 1)
        noteMessage.note = DelegationNote(
            childConversationID: ConversationID("child"), childName: "review-bot", kind: .report,
            state: .stopped, excerpt: "The turn was stopped.", waitsForUser: false, deferred: false, stopCause: .quit
        )
        snapshot.messages = [.init(message: noteMessage)]
        #expect(ChatTranscriptAssembler.delegationCall(item, snapshot: snapshot)?.state == "Stopped when Ottam quit")

        // A run Ottam's exit interrupted is over too, not working.
        snapshot.delegatedChildren["7331e98c99c4c150"]?[0].activity?.latestRunStatus = .interrupted
        let interrupted = ChatTranscriptAssembler.delegationCall(item, snapshot: snapshot)
        #expect(interrupted?.state == "Stopped when Ottam quit")
        #expect(interrupted?.isWorking == false)
    }

    @Test("a child whose call the transcript does not name shows its card after the call that started it")
    func unclaimedChildFollowsItsCall() {
        // OpenCode calls `delegate` from inside a code-mode `execute`.
        func tool(_ id: String, position: UInt64, createdAt: String) -> ConversationTranscriptItem {
            var item = TranscriptFixture.item(id, position: position, body: #"{"code":"await tools.ottam.delegate({})"}"#)
            item.kind = .tool
            item.createdAt = createdAt
            item.tool = .init(name: "execute", title: "execute", status: "completed",
                              arguments: .null, locations: [], partialMetadata: false, payloads: [])
            return item
        }
        var snapshot = TranscriptFixture.snapshot()
        snapshot.normalizedItems = [
            tool("search", position: 1, createdAt: "2026-08-21T10:00:01.000Z"),
            tool("execute", position: 2, createdAt: "2026-08-21T10:00:02.000Z"),
            TranscriptFixture.item("answer", position: 3, body: "Started it."),
        ]
        snapshot.delegatedChildren["key"] = [ChatDelegatedChild(
            conversationID: ConversationID("child"), agentName: "OpenCode", title: "Count MCP specs",
            fromRunID: ConversationRunID("run-1"), createdAt: "2026-08-21T10:00:02.500Z"
        )]
        let entries = ChatTranscriptAssembler.plan(snapshot)
        let ids = entries.map(\.id)
        let card = ids.firstIndex(of: "delegation/child/child")
        #expect(card != nil)
        #expect(card.map { $0 < ids.firstIndex(of: "answer")! } == true)
        guard let card, case .card(.delegation(let presentation)) = entries[card].content else {
            Issue.record("the child's card is a delegation card")
            return
        }
        #expect(presentation.openConversationID == ConversationID("child"))
        #expect(presentation.title == "Count MCP specs")

        // A child its call names shows once, on that call.
        var named = tool("execute", position: 2, createdAt: "2026-08-21T10:00:02.000Z")
        named.bodyPreview = #"{"engine":"opencode","prompt":"Count"}"#
        named.bodyByteCount = UInt64(named.bodyPreview.utf8.count)
        named.tool?.name = "mcp__ottam__delegate"
        snapshot.normalizedItems[1] = named
        snapshot.delegatedChildren = [ChatTranscriptAssembler.delegationKey("Count"): snapshot.delegatedChildren["key"]!]
        let claimed = ChatTranscriptAssembler.plan(snapshot).map(\.id)
        #expect(!claimed.contains("delegation/child/child"))
        #expect(claimed.contains("execute/delegation"))

        // A call whose prompt matches no key takes the child its turn
        // started after it, instead of a second card for that child.
        snapshot.delegatedChildren = ["other": snapshot.delegatedChildren.values.first!]
        let adopted = ChatTranscriptAssembler.plan(snapshot)
        #expect(!adopted.map(\.id).contains("delegation/child/child"))
        let call = adopted.first { $0.id == "execute/delegation" }
        guard case .card(.delegation(let adoptedCard))? = call?.content else {
            Issue.record("the call keeps its delegation card")
            return
        }
        #expect(adoptedCard.openConversationID == ConversationID("child"))

        // So does a call whose preview was cut off before its prompt.
        snapshot.normalizedItems[1].bodyPreview = #"{"engine":"opencode","prompt":"Count the"#
        snapshot.normalizedItems[1].bodyByteCount = 8192
        let truncated = ChatTranscriptAssembler.plan(snapshot)
        #expect(!truncated.map(\.id).contains("delegation/child/child"))
        guard case .card(.delegation(let truncatedCard))? = truncated.first(where: { $0.id == "execute/delegation" })?.content else {
            Issue.record("the cut-off call keeps its delegation card")
            return
        }
        #expect(truncatedCard.openConversationID == ConversationID("child"))
    }

    @Test("a hidden delegate call does not take a child from the calls the transcript shows")
    func suppressedDelegateCallClaimsNoChild() {
        func call(_ id: String, position: UInt64, createdAt: String) -> ConversationTranscriptItem {
            var item = TranscriptFixture.item(id, position: position, body: #"{"engine":"opencode","prompt":"Count"}"#)
            item.kind = .tool
            item.createdAt = createdAt
            item.tool = .init(name: "mcp__ottam__delegate", title: "mcp__ottam__delegate", status: "completed",
                              arguments: .null, locations: [], partialMetadata: false, payloads: [])
            return item
        }
        func child(_ id: String, createdAt: String) -> ChatDelegatedChild {
            ChatDelegatedChild(conversationID: ConversationID(id), agentName: "OpenCode", title: "Count",
                               fromRunID: ConversationRunID("run-1"), createdAt: createdAt)
        }
        var snapshot = TranscriptFixture.snapshot()
        var hidden = call("retried", position: 1, createdAt: "2026-08-21T10:00:01.000Z")
        hidden.completion = .suppressed
        snapshot.normalizedItems = [
            hidden,
            call("shown", position: 2, createdAt: "2026-08-21T10:00:02.000Z"),
            TranscriptFixture.item("answer", position: 3, body: "Started them."),
        ]
        snapshot.delegatedChildren[ChatTranscriptAssembler.delegationKey("Count")] = [
            child("first", createdAt: "2026-08-21T10:00:01.500Z"),
            child("second", createdAt: "2026-08-21T10:00:02.500Z"),
        ]
        let entries = ChatTranscriptAssembler.plan(snapshot)
        let opened = entries.compactMap { entry -> ConversationID? in
            guard case .card(.delegation(let card)) = entry.content else { return nil }
            return card.openConversationID
        }
        #expect(opened.sorted { $0.rawValue < $1.rawValue } == [ConversationID("first"), ConversationID("second")])
    }

    @Test("a delegate call without a named agent names its engine, or else the caller's own agent")
    func adHocDelegationCallNamesItsAgent() {
        func call(_ body: String) -> ConversationTranscriptItem {
            var item = TranscriptFixture.item("call", body: body)
            item.kind = .tool
            item.tool = .init(name: "mcp__ottam__delegate", title: "mcp__ottam__delegate", status: "completed",
                              arguments: .null, locations: [], partialMetadata: false, payloads: [])
            return item
        }
        var snapshot = ChatTranscriptSnapshot()
        let engine = ChatTranscriptAssembler.delegationCall(call(#"{"engine":"codex","prompt":"Review it"}"#), snapshot: snapshot)
        #expect(engine?.title == "Delegated to codex")
        let unknown = ChatTranscriptAssembler.delegationCall(call(#"{"prompt":"Review it"}"#), snapshot: snapshot)
        #expect(unknown?.title == "Delegated to an agent")
        snapshot.agentName = "Claude Code"
        let own = ChatTranscriptAssembler.delegationCall(call(#"{"prompt":"Review it"}"#), snapshot: snapshot)
        #expect(own?.title == "Delegated to Claude Code")
    }

    @Test("calls that repeat a prompt take its children in order; a refused one takes none")
    func repeatedDelegationPrompt() {
        func call(status: String) -> ConversationTranscriptItem {
            var item = TranscriptFixture.item("call", body: #"{"agent":"review-bot","prompt":"Review app/Package.swift"}"#)
            item.kind = .tool
            item.tool = .init(name: "mcp__ottam__delegate", title: "mcp__ottam__delegate", status: status,
                              arguments: .null, locations: [], partialMetadata: false, payloads: [])
            return item
        }
        var snapshot = ChatTranscriptSnapshot()
        snapshot.delegatedChildren["7331e98c99c4c150"] = ["first", "second"].map {
            ChatDelegatedChild(conversationID: ConversationID($0), agentName: "review-bot", title: "Package review")
        }
        var calls: [String: Int] = [:]
        let opened = [call(status: "completed"), call(status: "failed"), call(status: "completed"), call(status: "pending")]
            .map { ChatTranscriptAssembler.delegationCall($0, snapshot: snapshot, calls: &calls)?.openConversationID }
        #expect(opened == [ConversationID("first"), nil, ConversationID("second"), nil])
    }

    @Test("reports read as Received message rows grouped with tools; idle ones wait for Continue")
    func delegationNotes() {
        var first = TranscriptFixture.message("note-1", role: .system, body: "Looks good", position: 1)
        first.note = DelegationNote(
            childConversationID: ConversationID("child-1"), childName: "review-bot", kind: .report,
            state: .completed, excerpt: "Looks good", waitsForUser: false, deferred: false
        )
        var second = TranscriptFixture.message("note-2", role: .system, body: "", position: 2)
        second.note = DelegationNote(
            childConversationID: ConversationID("child-2"), childName: "ask-bot", kind: .report,
            state: .stopped, excerpt: "The turn was stopped.", waitsForUser: true, deferred: false, stopCause: .parentStopped
        )
        var delivery = TranscriptFixture.message("delivery", role: .user, body: "[Ottam] Reports", position: 3)
        delivery.source = .noteDelivery
        var read = TranscriptFixture.item("read", position: 4, body: "")
        read.kind = .tool
        read.tool = .init(name: "Read", kind: "read", title: "Read notes.md", status: "completed",
                          arguments: .null, locations: [], partialMetadata: false, payloads: [])
        var snapshot = ChatTranscriptSnapshot(messages: [first, second].map { .init(message: $0) })
        snapshot.delegatedChildren["key"] = [ChatDelegatedChild(
            conversationID: ConversationID("child-1"), agentName: "review-bot", title: "Package review",
            avatar: AgentAvatar(seed: 7)
        )]

        // Idle, with no turn carrying them: one group, then Continue.
        let waiting = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec)
        guard case .activityGroup(let rows, false) = waiting.first else {
            Issue.record("expected the reports' group, got \(waiting)"); return
        }
        #expect(rows.map(\.label) == ["Package review", "ask-bot"])
        // A listed child's row carries its tile; one not listed has none.
        #expect(rows.map(\.noteAvatar) == [AgentAvatar(seed: 7), nil])
        #expect(rows.map(\.detail) == ["Looks good", ""])
        #expect(rows.map(\.receivedOutcome) == [nil, "stopped with this turn"])
        #expect(waiting.last == .delegation(ChatDelegationPresentation(kind: .waiting(count: 2, messages: false))))

        // Delivered: the prompt shows nothing, and the turn's tools join the
        // reports' group. The held report still waits for Continue.
        snapshot.messages[0].message.noteDeliveredAt = "2026-10-28T10:00:00.000Z"
        snapshot.messages.append(.init(message: delivery))
        snapshot.normalizedItems = [read]
        let delivered = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec)
        guard case .activityGroup(let grouped, _) = delivered.first else {
            Issue.record("expected one group, got \(delivered)"); return
        }
        #expect(grouped.map(\.kind) == ["receivedMessage", "receivedMessage", "read"])
        #expect(ChatActivityGroupRow.summary(of: grouped) == "Received 2 messages, read files")
        #expect(delivered.last == .delegation(ChatDelegationPresentation(kind: .waiting(count: 1, messages: false))))

        // Every report carried: nothing waits.
        snapshot.messages[1].message.noteDeliveredAt = "2026-10-28T10:01:00.000Z"
        let carried = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec)
        #expect(!carried.contains { if case .delegation = $0 { true } else { false } })
    }

    @Test("a parent's acknowledgement marks the notes it carried and shows no reply")
    func delegationAck() {
        var note = TranscriptFixture.message("note-1", role: .system, body: "Looks good", position: 1)
        note.note = DelegationNote(
            childConversationID: ConversationID("child-1"), childName: "review-bot", kind: .report,
            state: .completed, excerpt: "Looks good", waitsForUser: false, deferred: false
        )
        note.noteDeliveredAt = "2026-10-28T10:00:00.000Z"
        // A report held for the user, which the wake did not carry.
        var held = TranscriptFixture.message("note-2", role: .system, body: "Capped", position: 2)
        held.note = DelegationNote(
            childConversationID: ConversationID("child-2"), childName: "docs-bot", kind: .report,
            state: .completed, excerpt: "Capped", waitsForUser: true, deferred: false
        )
        var delivery = TranscriptFixture.message("delivery", role: .user, body: "[Ottam] Reports", position: 3)
        delivery.source = .noteDelivery
        // Quoted, as agents sometimes do.
        var ack = item("ack", position: 4, body: "`::ottam-ack{}`\n")
        ack.completion = .complete
        ack.updatedAt = "2026-10-28T10:00:05.000Z"
        var boundary = item("boundary", position: 5, kind: .runBoundary)
        boundary.runStatus = .completed
        var snapshot = ChatTranscriptSnapshot(messages: [note, held, delivery].map { .init(message: $0) })
        snapshot.normalizedItems = [ack, boundary]

        let specs = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec)
        guard case .activityGroup(let rows, _) = specs.first else {
            Issue.record("expected the notes' group, got \(specs)"); return
        }
        #expect(rows.map(\.receivedOutcome) == ["acknowledged", nil])
        // The group, then the trailer: no reply, and nothing to copy. The
        // held report still waits.
        #expect(specs.count == 3, "\(specs)")
        #expect(specs[1] == .turnTrailer(.persisted(
            runID: .init("run-1"), body: "", createdAt: boundary.updatedAt, outcome: .complete
        )))

        // A finished message that only looks like the start of one shows.
        var cut = item("ack", position: 4, body: "::ott")
        cut.completion = .complete
        snapshot.normalizedItems = [cut, boundary]
        #expect(ChatTranscriptAssembler.plan(snapshot).contains { if case .markdown = $0.content { true } else { false } })

        // Streaming in, the start of one shows nothing and marks nothing yet.
        snapshot.normalizedItems = [item("ack", position: 4, body: "::ott")]
        snapshot.liveRun = TranscriptFixture.liveRun("run-1")
        let streaming = ChatTranscriptAssembler.plan(snapshot)
        #expect(!streaming.contains { if case .markdown = $0.content { true } else { false } })
        guard case .activityGroup(let live, _) = streaming.compactMap(\.cardSpec).first else {
            Issue.record("expected the notes' group"); return
        }
        #expect(live.map(\.receivedOutcome) == [nil, nil])
    }

    @Test("a delegated child's turn offers Send to Parent while its report waits")
    func sendToParentTrailer() {
        let first = item("first", position: 1, body: "one")
        var second = item("second", position: 2, body: "two")
        second.runID = .init("run-2")
        var snapshot = ChatTranscriptSnapshot()
        snapshot.normalizedItems = [first, second]
        snapshot.runStatuses = [.init("run-1"): .completed, .init("run-2"): .completed]
        func offers() -> [String] {
            ChatTranscriptAssembler.plan(snapshot).compactMap {
                guard case .turnTrailer(.persisted(let runID, _, _, _, _, _, let sends)) = $0.cardSpec else { return nil }
                return "\(runID?.rawValue ?? ""):\(sends)"
            }
        }
        #expect(offers() == ["run-1:false", "run-2:false"])
        // The turn the report is of, not the latest.
        snapshot.deferredReportRunID = .init("run-1")
        #expect(offers() == ["run-1:true", "run-2:false"])
    }

    @Test("Ottam's tools read as what the agent did, and to whom")
    func ottamToolRows() {
        func title(_ name: String, _ body: String, in snapshot: ChatTranscriptSnapshot) -> String? {
            let tool = TranscriptTool(name: "mcp__ottam__" + name, title: "mcp__ottam__" + name, status: "completed",
                                      arguments: .null, locations: [], partialMetadata: false, payloads: [])
            let arguments = body.data(using: .utf8).flatMap { try? JSONDecoder().decode(JSONValue.self, from: $0) }
            return ChatTranscriptAssembler.ottamToolTitle(tool, arguments: arguments, snapshot: snapshot)
        }
        var parent = ChatTranscriptSnapshot()
        parent.delegatedChildren["key"] = [ChatDelegatedChild(
            conversationID: ConversationID("child"), agentName: "review-bot", title: "Package review"
        )]
        #expect(title("message_child", #"{"child":"Package review","message":"And the tests?"}"#, in: parent) == "Messaged review-bot")
        #expect(title("stop_child", #"{"child":"child"}"#, in: parent) == "Stopped review-bot")
        #expect(title("get_session_messages", #"{"conversation":"child"}"#, in: parent) == "Read review-bot's conversation")
        #expect(title("get_session_toolcall_output", #"{"conversation":"child","tool_call_id":"t1"}"#, in: parent)
            == "Read a tool call's output from review-bot")
        #expect(title("list_children", "{}", in: parent) == "Checked on delegated agents")
        #expect(title("delegate", "{}", in: parent) == nil)

        // A child names its parent by the agent that delegated to it.
        var task = TranscriptFixture.message("m1", role: .user, body: "Review")
        task.source = .delegation
        task.task = DelegationTask(title: "Package review", prompt: "Review", instructions: "", startedByEngineID: EngineID("claude"))
        var child = ChatTranscriptSnapshot(messages: [.init(message: task)])
        child.engineNames = [EngineID("claude"): "Claude Code"]
        child.delegationParent = ConversationSummary(
            conversationID: ConversationID("parent"), workspaceID: WorkspaceID("workspace-1"),
            workingRoot: WorkingRootSummary(kind: .workspaceFiles, displayName: "Workspace files",
                                            canonicalPath: "/tmp/workspace/files", availability: .available),
            title: "Ship the parser", createdAt: "2026-10-29T00:00:00Z", updatedAt: "2026-10-29T00:00:00Z",
            alwaysAllowToolActions: false
        )
        #expect(title("message_parent", #"{"message":"Found it"}"#, in: child) == "Messaged Claude Code")
        #expect(title("get_session_messages", "{}", in: child) == "Read Claude Code's conversation")
    }

    @Test("a parent's message reads as received in its child, and waits as a message")
    func parentMessageInChild() {
        var message = TranscriptFixture.message("note", role: .system, body: "And the tests?", position: 1)
        message.note = DelegationNote(
            childConversationID: ConversationID("child"), childName: "Claude Code", kind: .parentMessage,
            state: .completed, excerpt: "And the tests?", waitsForUser: true, deferred: false
        )
        var snapshot = ChatTranscriptSnapshot(messages: [.init(message: message)])
        var cards = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec)
        guard case .activityGroup(let rows, _) = cards.first else {
            Issue.record("expected the message's group, got \(cards)"); return
        }
        // With no parent listed, the row names its agent and links nowhere.
        #expect(rows.map(\.hoverText) == ["Received message from Claude Code"])
        #expect(rows.first?.noteSender == nil)
        guard case .delegation(let waiting) = cards.last else {
            Issue.record("expected the waiting row, got \(cards)"); return
        }
        #expect(waiting.title == "1 message waiting")

        // With its parent listed, the row names and opens the parent, as
        // "Started by" does.
        snapshot.delegationParent = ConversationSummary(
            conversationID: ConversationID("parent"), workspaceID: WorkspaceID("workspace-1"),
            workingRoot: WorkingRootSummary(kind: .workspaceFiles, displayName: "Workspace files",
                                            canonicalPath: "/tmp/workspace/files", availability: .available),
            title: "Ship the parser", createdAt: "2026-10-29T00:00:00Z", updatedAt: "2026-10-29T00:00:00Z",
            alwaysAllowToolActions: false
        )
        cards = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec)
        guard case .activityGroup(let named, _) = cards.first else {
            Issue.record("expected the message's group, got \(cards)"); return
        }
        #expect(named.map(\.hoverText) == ["Received message from Ship the parser"])
        #expect(named.first?.noteSender == ConversationID("parent"))
    }

    @Test("a child its parent's agent stopped says so by name")
    func stoppedByParentAgent() {
        var item = TranscriptFixture.item("call", body: #"{"agent":"review-bot","prompt":"Review app/Package.swift"}"#)
        item.kind = .tool
        item.tool = .init(name: "mcp__ottam__delegate", title: "mcp__ottam__delegate", status: "completed",
                          arguments: .null, locations: [], partialMetadata: false, payloads: [])
        var snapshot = ChatTranscriptSnapshot()
        snapshot.agentName = "Claude Code"
        snapshot.delegatedChildren["7331e98c99c4c150"] = [ChatDelegatedChild(
            conversationID: ConversationID("child"), agentName: "review-bot", title: "Package review",
            activity: DelegationActivity(latestRunStatus: .canceled, commands: 0, reads: 0, edits: 0, searches: 0,
                                         fetches: 0, other: 0, subagents: 0)
        )]
        var note = TranscriptFixture.message("note", role: .system, body: "", position: 1)
        note.note = DelegationNote(
            childConversationID: ConversationID("child"), childName: "review-bot", kind: .report,
            state: .stopped, excerpt: "The turn was stopped.", waitsForUser: false, deferred: false, stopCause: .parentAgent
        )
        note.noteDeliveredAt = "2026-10-29T10:00:00.000Z"
        snapshot.messages = [.init(message: note)]
        #expect(ChatTranscriptAssembler.delegationCall(item, snapshot: snapshot)?.state == "Stopped by Claude Code")
        let rows = ChatTranscriptAssembler.plan(snapshot).compactMap(\.cardSpec).compactMap { card -> [ChatLiveActivity]? in
            if case .activityGroup(let rows, _) = card { rows } else { nil }
        }.joined()
        #expect(rows.compactMap(\.receivedOutcome) == ["stopped by Claude Code"])
    }

    @Test("a delegated child opens on who started it and its task")
    func delegationTaskHeader() {
        let task = DelegationTask(title: "Package review", prompt: "Review app/Package.swift",
                                  instructions: "Review carefully.", startedByEngineID: EngineID("claude"))
        var message = TranscriptFixture.message("m1", role: .user, body: "Review carefully.\n\nReview app/Package.swift")
        message.source = .delegation
        message.task = task
        var snapshot = ChatTranscriptSnapshot(messages: [.init(message: message)])
        snapshot.engineNames = [EngineID("claude"): "Claude Code"]
        let entries = ChatTranscriptAssembler.plan(snapshot)
        #expect(entries.map(\.id) == ["m1/task", "message/m1/trailer"])
        #expect(entries.first?.cardSpec == .delegation(ChatDelegationPresentation(kind: .task(ChatDelegationTaskPresentation(
            task: task, startedBy: "Claude Code", parentConversationID: nil, parentTitle: nil
        )))))
        // The row under the card lines up with it, not with a bubble.
        guard case .userTrailer(let trailer) = entries.last?.cardSpec else {
            Issue.record("expected the message's trailer"); return
        }
        #expect(trailer.isLeading)
    }

    @Test("a child's header cuts a long parent title to one line")
    func delegationTaskParentTitle() {
        var task = ChatDelegationTaskPresentation(
            task: DelegationTask(title: "Review", prompt: "Review", instructions: ""),
            startedBy: "Codex", parentConversationID: ConversationID("p"), parentTitle: "  Ship the parser  "
        )
        #expect(task.parentTitleLabel == "Ship the parser")
        task.parentTitle = "Avatar walkthrough. Use Ottam's delegate tool three times, then stop without"
        #expect(task.parentTitleLabel == "Avatar walkthrough. Use Ottam's delegat…")
        task.parentTitle = " "
        #expect(task.parentTitleLabel == nil)
    }

    @Test("list_children reads as checking on delegated agents")
    func listChildrenLabel() {
        var item = TranscriptFixture.item("list", body: "{}")
        item.kind = .tool
        item.tool = .init(name: "mcp__ottam__list_children", title: "mcp__ottam__list_children", status: "completed",
                          arguments: .null, locations: [], partialMetadata: false, payloads: [])
        let entries = ChatTranscriptAssembler.plan(ChatTranscriptSnapshot(normalizedItems: [item]))
        guard case .activityGroup(let rows, _) = entries.first?.cardSpec else {
            Issue.record("expected a tool row"); return
        }
        #expect(rows.map(\.label) == ["Checked on delegated agents"])
    }
}
