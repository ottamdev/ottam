import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Engine switch divider")
@MainActor
struct EngineSwitchDividerTests {
    private let names: [EngineID: String] = [EngineID("opencode"): "OpenCode", EngineID("claude"): "Claude Code"]

    private func run(
        _ id: String, _ engine: String, origin: SessionOrigin = .continued,
        replay: ConversationHistoryReplay? = nil
    ) -> ConversationRunSummary {
        ConversationRunSummary(
            runID: .init(id), conversationID: TranscriptFixture.conversationID,
            engineID: EngineID(engine), status: .completed, startedAt: "2026-11-02T04:00:00Z",
            sessionOrigin: origin, cost: .object([:]), historyReplay: replay, error: .object([:])
        )
    }

    @Test("a switch says whether the agent got the earlier turns or resumed its own session")
    func labels() {
        let switches = ChatTranscriptDivider.engineSwitches(
            in: [
                // Listed newest first, as a page arrives; ids sort in start order.
                run("run-5", "claude", origin: .new),
                run("run-4", "opencode", origin: .continued),
                run("run-3", "claude", origin: .continued),
                run("run-2", "claude", origin: .new, replay: .init(entries: 12, truncated: false)),
                run("run-1", "opencode", origin: .new),
            ],
            engineNames: names
        )
        #expect(switches[.init("run-1")] == nil, "the first loaded run has nothing to differ from")
        #expect(switches[.init("run-2")]?.title == "Switched to Claude Code · earlier turns shared")
        #expect(switches[.init("run-2")]?.help.contains("12 earlier entries") == true)
        #expect(switches[.init("run-3")] == nil, "the same agent continuing is no switch")
        #expect(switches[.init("run-4")]?.title == "Switched to OpenCode · resumed its earlier session")
        #expect(switches[.init("run-5")]?.title == "Switched to Claude Code")
    }

    @Test("a fresh session of the same agent that was sent the earlier turns says so")
    func freshSession() {
        let switches = ChatTranscriptDivider.engineSwitches(
            in: [
                run("run-1", "claude", origin: .new),
                run("run-2", "claude", origin: .new, replay: .init(entries: 40, truncated: true)),
            ],
            engineNames: names
        )
        let divider = switches[.init("run-2")]
        #expect(divider?.title == "Earlier turns shared with a fresh session")
        #expect(divider?.help.contains("latest 40 entries") == true)
    }

    @Test("a session that opened with a command gets the earlier turns with its next message")
    func afterCommand() {
        let switches = ChatTranscriptDivider.engineSwitches(
            in: [
                run("run-1", "opencode", origin: .new),
                run("run-2", "claude", origin: .new),
                run("run-3", "claude", origin: .continued, replay: .init(entries: 19, truncated: false)),
            ],
            engineNames: names
        )
        #expect(switches[.init("run-2")]?.title == "Switched to Claude Code")
        #expect(switches[.init("run-3")]?.title == "Earlier turns shared")
    }

    @Test("the divider sits above the switched run's first output")
    func placement() {
        let divider = ChatTranscriptDivider.engineSwitches(
            in: [run("run-1", "opencode", origin: .new), run("run-2", "claude", origin: .new)],
            engineNames: names
        )
        var earlier = ConversationTranscriptItem(
            itemID: "earlier", conversationID: TranscriptFixture.conversationID, runID: .init("run-1"),
            position: 1, revision: 1, kind: .assistantText, completion: .complete,
            createdAt: "2026-11-02T04:00:00Z", updatedAt: "2026-11-02T04:00:00Z",
            bodyPreview: "From OpenCode", bodyByteCount: 13
        )
        earlier.tool = nil
        var reply = earlier
        reply.itemID = "reply"
        reply.runID = .init("run-2")
        reply.position = 2
        reply.bodyPreview = "From Claude"
        var more = reply
        more.itemID = "more"
        more.position = 3
        let entries = ChatTranscriptAssembler.plan(.init(
            engineSwitches: divider, normalizedItems: [earlier, reply, more]
        ))
        #expect(entries.map(\.id) == ["earlier", "run/run-2/switched", "reply", "more"])
        #expect(entries[1].cardSpec == .engineSwitched(divider[.init("run-2")]!))
    }
}
