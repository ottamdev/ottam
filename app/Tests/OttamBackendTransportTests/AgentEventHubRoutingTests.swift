import Foundation
import Testing
@testable import OttamBackendClient

/// Which subscribers each notification reaches. The hub reads routing IDs from
/// the payload; these pin that behavior, field by field.
@Suite("Agent event hub routing")
struct AgentEventHubRoutingTests {
    private func notification(_ method: String, _ params: String?) -> AgentNotification {
        let line = params.map { #"{"jsonrpc":"2.0","method":"\#(method)","params":\#($0)}"# }
            ?? #"{"jsonrpc":"2.0","method":"\#(method)"}"#
        return AgentNotification(method: method, payload: Data(line.utf8))
    }

    /// Publishes `notification` and returns the labels of the filters it reached.
    private func reached(_ notification: AgentNotification, filters: [String: AgentEventFilter]) async -> Set<String> {
        let hub = AgentEventHub()
        let hits = Hits()
        for (label, filter) in filters {
            await hub.subscribe(filter: filter) { _ in await hits.add(label) }
        }
        await hub.publish(notification)
        return await hits.labels
    }

    @Test("each ID filter matches only its own field")
    func idFilters() async {
        let filters: [String: AgentEventFilter] = [
            "all": .all,
            "workspace": AgentEventFilter(workspaceID: WorkspaceID("w1")),
            "otherWorkspace": AgentEventFilter(workspaceID: WorkspaceID("w2")),
            "project": AgentEventFilter(projectID: ProjectID("p1")),
            "terminal": AgentEventFilter(terminalSessionID: TerminalSessionID("s1")),
            "agent": AgentEventFilter(agentID: AgentID("a1")),
            "agentSession": AgentEventFilter(agentSessionID: AgentSessionID("s1")),
            "conversation": AgentEventFilter(conversationID: ConversationID("c1")),
            "run": AgentEventFilter(conversationRunID: ConversationRunID("r1")),
            "method": AgentEventFilter(methods: ["test.event"]),
            "otherMethod": AgentEventFilter(methods: ["test.other"]),
        ]
        let full = notification("test.event", #"""
            {"workspaceID":"w1","projectID":"p1","sessionID":"s1","agentID":"a1","conversationID":"c1","runID":"r1"}
            """#)
        #expect(await reached(full, filters: filters) == [
            "all", "workspace", "project", "terminal", "agent", "agentSession", "conversation", "run", "method",
        ])

        let workspaceOnly = notification("test.event", #"{"workspaceID":"w2"}"#)
        #expect(await reached(workspaceOnly, filters: filters) == ["all", "otherWorkspace", "method"])
    }

    @Test("a malformed field fails only the filters that read it")
    func malformedField() async {
        let filters: [String: AgentEventFilter] = [
            "workspace": AgentEventFilter(workspaceID: WorkspaceID("w1")),
            "terminal": AgentEventFilter(terminalSessionID: TerminalSessionID("s1")),
        ]
        let malformed = notification("test.event", #"{"workspaceID":"w1","sessionID":{"nested":true}}"#)
        #expect(await reached(malformed, filters: filters) == ["workspace"])
    }

    @Test("notifications without params match only unfiltered IDs")
    func missingParams() async {
        let filters: [String: AgentEventFilter] = [
            "all": .all,
            "workspace": AgentEventFilter(workspaceID: WorkspaceID("w1")),
            "conversation": AgentEventFilter(conversationID: ConversationID("c1")),
        ]
        #expect(await reached(notification("test.event", nil), filters: filters) == ["all"])
        let empty = AgentNotification(method: "test.event", payload: Data())
        #expect(await reached(empty, filters: filters) == ["all"])
    }

    @Test("transcript recovery reaches every subscriber to its method, whatever the IDs")
    func recoveryMarker() async {
        let filters: [String: AgentEventFilter] = [
            "conversation": AgentEventFilter(
                methods: ["conversation.transcriptRecoveryRequired"],
                workspaceID: WorkspaceID("w1"),
                conversationID: ConversationID("c9")
            ),
            "otherMethod": AgentEventFilter(methods: ["conversation.transcriptDelta"]),
        ]
        let marker = notification("conversation.transcriptRecoveryRequired", "{}")
        #expect(await reached(marker, filters: filters) == ["conversation"])
    }
}

private actor Hits {
    private(set) var labels: Set<String> = []
    func add(_ label: String) { labels.insert(label) }
}
