import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// A conversation's mode follows the agent: the picker shows what the agent
/// confirmed, a pick in an existing conversation waits for the next message,
/// and only a new chat's pick changes the per-engine default.
@Suite("Chat conversation mode")
@MainActor
struct ChatConversationModeTests {
    private func makeModel() -> ChatWorkspaceModel {
        let suite = "ChatConversationModeTests.\(UUID().uuidString)"
        let model = ChatWorkspaceModel(defaults: UserDefaults(suiteName: suite)!)
        model.selectedEngineID = EngineID("claude")
        return model
    }

    @Test("A new chat's pick sets the per-engine default")
    func draftPickSetsDefault() {
        let model = makeModel()
        model.selectMode("plan")
        #expect(model.selectedModeID == "plan")
        #expect(model.effectiveModeID == "plan")
        #expect(model.hasPendingMode == false)
    }

    @Test("An existing conversation shows the agent's confirmed mode, not the default")
    func confirmedModeWinsOverDefault() {
        let model = makeModel()
        model.selectMode("plan")
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        #expect(model.effectiveModeID == "plan", "nothing confirmed yet: the default")
        model.confirmMode("default", conversationID: conversation)
        #expect(model.effectiveModeID == "default")
        #expect(model.selectedModeID == "plan", "the new-chat default is untouched")
    }

    /// Core with nothing live: the app names no session, Core defers.
    private func noLiveSession(
        _: ConversationID, agentSessionID: AgentSessionID?, _: String
    ) async throws -> ConversationSetModeResult {
        #expect(agentSessionID == nil, "no session in the loaded detail")
        return ConversationSetModeResult(outcome: .deferred)
    }

    @Test("A pick with no live session waits for the next message")
    func pickInConversationIsPending() async {
        let model = makeModel()
        model.selectMode("plan")
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        model.confirmMode("default", conversationID: conversation)

        model.selectMode("acceptEdits")
        await model.applyPendingMode(send: noLiveSession)
        #expect(model.effectiveModeID == "acceptEdits")
        #expect(model.hasPendingMode)
        #expect(model.selectedModeID == "plan")

        // The agent changing mode on its own mid-run keeps the user's pick.
        model.confirmMode("plan", conversationID: conversation)
        #expect(model.effectiveModeID == "acceptEdits")
        // The agent confirming the pick completes it.
        model.confirmMode("acceptEdits", conversationID: conversation)
        #expect(model.hasPendingMode == false)
        #expect(model.effectiveModeID == "acceptEdits")
    }

    @Test("Re-picking the confirmed mode clears the pending pick")
    func repickingConfirmedClearsPending() async {
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        model.confirmMode("default", conversationID: conversation)
        model.selectMode("plan")
        await model.applyPendingMode(send: noLiveSession)
        #expect(model.hasPendingMode)
        model.selectMode("default")
        #expect(model.hasPendingMode == false)
        #expect(model.effectiveModeID == "default")
    }

    @Test("Each conversation keeps its own mode")
    func conversationsAreIsolated() {
        let model = makeModel()
        model.selectMode("plan")
        let first = ConversationID("conversation-a")
        let second = ConversationID("conversation-b")
        model.confirmMode("default", conversationID: first)
        model.confirmMode("acceptEdits", conversationID: second)

        model.selectedConversationID = first
        #expect(model.effectiveModeID == "default")
        model.selectMode("bypassPermissions")
        model.selectedConversationID = second
        #expect(model.effectiveModeID == "acceptEdits")
        #expect(model.hasPendingMode == false)
        #expect(model.modeID(for: first) == "bypassPermissions")
        model.selectedConversationID = nil
        #expect(model.effectiveModeID == "plan", "a new chat still starts from the default")
    }

    /// A message queued behind a running turn is routed with the run's engine,
    /// but must take the pick made during that turn: otherwise the drain
    /// restores the old, possibly wider, mode before prompting.
    @Test("A message queued during a run takes the conversation's current mode")
    func queuedMessageTakesCurrentMode() {
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        model.confirmMode("acceptEdits", conversationID: conversation)
        var runEngine = TranscriptFixture.engine()
        runEngine.modes = [
            EngineSelectOption(id: "acceptEdits", displayName: "Accept edits"),
            EngineSelectOption(id: "plan", displayName: "Plan"),
        ]
        model.selectedConversationID = conversation
        model.selectMode("plan")
        #expect(model.conversationModeID(conversation, advertisedBy: runEngine) == "plan")

        runEngine.modes = [EngineSelectOption(id: "acceptEdits", displayName: "Accept edits")]
        #expect(
            model.conversationModeID(conversation, advertisedBy: runEngine) == "acceptEdits",
            "a mode the run's engine does not offer falls back to the confirmed one"
        )
    }

    /// Claude → Codex → Claude leaves both sessions active, Codex's newer.
    /// Picks belong to the session the latest run uses, not the newest one.
    @Test("After switching engines back, the latest run's session is the target")
    func engineSwitchBackTargetsLatestRunSession() {
        let conversation = TranscriptFixture.conversationID
        func session(_ id: String, _ engine: String) -> AgentSessionSummary {
            AgentSessionSummary(
                agentSessionID: AgentSessionID(id), conversationID: conversation,
                engineID: EngineID(engine), state: .active, sourceScopeFingerprint: "scope",
                createdAt: "2026-10-15T10:00:00Z", lastUsedAt: "2026-10-15T10:00:00Z"
            )
        }
        func run(_ engine: String) -> ConversationRunSummary {
            ConversationRunSummary(
                runID: .init("run-\(engine)"), conversationID: conversation,
                engineID: EngineID(engine), modelID: nil, modeID: "default", status: .running,
                startedAt: "2026-10-15T10:05:00Z", agentSessionID: AgentSessionID("external-\(engine)"),
                cost: .object([:]), error: .object([:])
            )
        }
        // Newest first, as the detail page lists them.
        let sessions = [session("session-codex", "codex"), session("session-claude", "claude")]
        let target = ChatWorkspaceModel.latestRunSession(runs: [run("claude")], sessions: sessions)
        #expect(target?.agentSessionID == AgentSessionID("session-claude"))
        #expect(ChatWorkspaceModel.latestRunSession(runs: [], sessions: sessions) == nil)
    }

    @Test("A pick applies to the live session at once")
    func pickAppliesLive() async {
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        let session = AgentSessionID("session-a")
        model.selectedConversationID = conversation
        model.noteActiveAgentSession(session, conversationID: conversation)
        model.confirmMode("default", conversationID: conversation)

        model.selectMode("acceptEdits")
        var sent: [String] = []
        await model.applyPendingMode { target, agentSessionID, modeID in
            #expect(target == conversation)
            #expect(agentSessionID == session)
            #expect(model.isWritingMode, "rows stay disabled while the agent answers")
            sent.append(modeID)
            return ConversationSetModeResult(outcome: .applied, currentModeID: modeID)
        }
        #expect(sent == ["acceptEdits"])
        #expect(model.effectiveModeID == "acceptEdits")
        #expect(model.hasPendingMode == false, "applied: no next-message note")
        #expect(model.isWritingMode == false)
    }

    @Test("A conversation started this launch applies through Core's active session")
    func pickWithoutKnownSessionStillApplies() async {
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        model.confirmMode("default", conversationID: conversation)

        model.selectMode("readOnly")
        await model.applyPendingMode { _, agentSessionID, modeID in
            #expect(agentSessionID == nil)
            return ConversationSetModeResult(outcome: .applied, currentModeID: modeID)
        }
        #expect(model.effectiveModeID == "readOnly")
        #expect(model.hasPendingMode == false)
    }

    @Test("A session that is not live defers the pick to the next message")
    func deferredOutcomeKeepsPending() async {
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        model.noteActiveAgentSession(AgentSessionID("session-a"), conversationID: conversation)
        model.confirmMode("default", conversationID: conversation)

        model.selectMode("plan")
        await model.applyPendingMode { _, _, _ in
            ConversationSetModeResult(outcome: .deferred, currentModeID: "default")
        }
        #expect(model.hasPendingMode)
        #expect(model.effectiveModeID == "plan")
        model.confirmMode("plan", conversationID: conversation)
        #expect(model.hasPendingMode == false, "the next message's confirmation completes it")
    }

    @Test("A refused pick keeps the confirmed mode and says why")
    func refusedPickKeepsConfirmed() async {
        struct Refused: LocalizedError {
            var errorDescription: String? { "Could not switch to permission mode \"plan\"." }
        }
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        model.noteActiveAgentSession(AgentSessionID("session-a"), conversationID: conversation)
        model.confirmMode("default", conversationID: conversation)

        model.selectMode("plan")
        await model.applyPendingMode { _, _, _ in throw Refused() }
        #expect(model.effectiveModeID == "default")
        #expect(model.hasPendingMode == false)
        #expect(model.selectedModeErrorText == "Could not switch to permission mode \"plan\".")
        model.selectMode("acceptEdits")
        #expect(model.selectedModeErrorText == nil, "a new pick clears the error")
    }

    @Test("A replaced session defers the pick instead of failing it")
    func staleSessionDefers() async {
        let model = makeModel()
        let conversation = ConversationID("conversation-a")
        model.selectedConversationID = conversation
        model.noteActiveAgentSession(AgentSessionID("session-old"), conversationID: conversation)
        model.confirmMode("default", conversationID: conversation)

        model.selectMode("plan")
        await model.applyPendingMode { _, _, _ in
            throw AgentConnectionError.rpcError(
                code: -32000,
                message: "This conversation's agent session has changed.",
                data: .object(["kind": .string("staleAgentSession")])
            )
        }
        #expect(model.hasPendingMode)
        #expect(model.selectedModeErrorText == nil)
    }
}
