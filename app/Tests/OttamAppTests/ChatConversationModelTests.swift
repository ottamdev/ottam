import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// A conversation's engine, model and effort are its own: the picker shows
/// what its session confirmed, a pick in it waits for its next message, and
/// only a new chat's pick changes the saved new-chat defaults.
@Suite("Chat conversation model")
@MainActor
struct ChatConversationModelTests {
    private let claude = EngineID("claude")
    private let codex = EngineID("codex")
    private let launch = ConversationID("launch-readiness")
    private let other = ConversationID("other")

    private func makeModel() -> (ChatWorkspaceModel, UserDefaults) {
        let defaults = UserDefaults(suiteName: "ChatConversationModelTests.\(UUID().uuidString)")!
        let model = ChatWorkspaceModel(defaults: defaults)
        model.selectedEngineID = claude
        model.selectModel("opus")
        model.selectThoughtLevel("high")
        return (model, defaults)
    }

    private func run(
        _ id: String, _ conversationID: ConversationID, engine: EngineID,
        model: String?, level: String?
    ) -> ConversationRunSummary {
        ConversationRunSummary(
            runID: .init(id), conversationID: conversationID,
            engineID: engine, modelID: model, modeID: nil, thoughtLevelID: level, status: .completed,
            startedAt: "2026-11-17T10:00:00Z", agentSessionID: AgentSessionID("external-\(id)"),
            cost: .object([:]), error: .object([:])
        )
    }

    private func session(
        _ id: String, _ conversationID: ConversationID, engine: EngineID,
        model: String?, level: String?
    ) -> AgentSessionSummary {
        AgentSessionSummary(
            agentSessionID: AgentSessionID(id), conversationID: conversationID,
            engineID: engine, state: .active, sourceScopeFingerprint: "scope",
            createdAt: "2026-11-17T10:00:00Z", lastUsedAt: "2026-11-17T10:00:00Z",
            currentModelID: model, currentThoughtLevelID: level
        )
    }

    /// Opens `conversationID` the way a loaded detail does.
    private func open(
        _ model: ChatWorkspaceModel, _ conversationID: ConversationID,
        runs: [ConversationRunSummary], sessions: [AgentSessionSummary]
    ) {
        model.selectedConversationID = conversationID
        model.noteConversationSelections(runs: runs, sessions: sessions, conversationID: conversationID)
    }

    @Test("An existing chat shows its own confirmed model, and New Chat keeps its pick")
    func chatShowsItsOwnModel() {
        let (model, defaults) = makeModel()
        open(model, launch,
             runs: [run("r1", launch, engine: claude, model: "opus", level: "high")],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        #expect(model.selectedModelID == "fable", "the session's word beats the run's request")
        #expect(model.effectiveThoughtLevelID == "xhigh")

        model.selectedConversationID = nil
        #expect(model.selectedModelID == "opus")
        #expect(model.effectiveThoughtLevelID == "high")
        #expect(defaults.string(forKey: "chat.selectedModelID.claude") == "opus")
        #expect(defaults.string(forKey: "chat.selectedThoughtLevelID.claude") == "high")
    }

    @Test("Switching between chats shows each one's model")
    func switchingChats() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        open(model, other, runs: [run("r2", other, engine: claude, model: nil, level: nil)],
             sessions: [session("s2", other, engine: claude, model: "sonnet", level: "low")])
        #expect(model.selectedModelID == "sonnet")
        model.selectedConversationID = launch
        #expect(model.selectedModelID == "fable")
        #expect(model.modelID(for: other) == "sonnet")
        #expect(model.thoughtLevelID(for: other) == "low")
    }

    @Test("A pick in a chat is that chat's alone, and never the new-chat default")
    func pickIsPerChat() {
        let (model, defaults) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        open(model, other, runs: [run("r2", other, engine: claude, model: nil, level: nil)],
             sessions: [session("s2", other, engine: claude, model: "sonnet", level: "low")])

        model.selectedConversationID = launch
        model.selectModel("haiku")
        model.selectThoughtLevel("medium")
        #expect(model.selectedModelID == "haiku")
        #expect(model.effectiveThoughtLevelID == "medium")
        #expect(model.hasPendingModel)
        #expect(model.modelID(for: other) == "sonnet", "other chats are untouched")
        #expect(model.draftModelID == "opus")
        #expect(defaults.string(forKey: "chat.selectedModelID.claude") == "opus")

        // Re-picking what the session runs drops the pick.
        model.selectModel("fable")
        #expect(model.pendingModelIDs[launch] == nil)

        // The agent confirming the pick completes it.
        model.selectModel("haiku")
        model.confirmModel("haiku", thoughtLevelID: "medium", engineID: claude, conversationID: launch)
        #expect(model.hasPendingModel == false)
        #expect(model.selectedModelID == "haiku")
    }

    @Test("A run cancelled before its switch took shows what the agent confirmed")
    func cancelledRunSettlesPicks() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        model.selectModel("haiku")
        let carrying = ConversationRunID("r2")
        model.notePicksCarried(by: carrying, engineID: claude, conversationID: launch,
                               modelID: "haiku", thoughtLevelID: "xhigh")
        model.noteRunEnded(ConversationRunID("unrelated"), conversationID: launch)
        #expect(model.hasPendingModel, "another run's end leaves the pick")
        model.noteRunEnded(carrying, conversationID: launch)
        #expect(model.hasPendingModel == false)
        #expect(model.selectedModelID == "fable")
    }

    @Test("The carrying run's confirmation settles the picks, whatever it confirmed")
    func carryingRunConfirmationSettles() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        model.selectModel("haiku")
        let carrying = ConversationRunID("r2")
        model.notePicksCarried(by: carrying, engineID: claude, conversationID: launch,
                               modelID: "haiku", thoughtLevelID: "xhigh")
        // The agent fell back to another model.
        model.confirmModel("sonnet", thoughtLevelID: nil, engineID: claude, conversationID: launch, runID: carrying)
        #expect(model.hasPendingModel == false)
        #expect(model.selectedModelID == "sonnet")
        #expect(model.effectiveThoughtLevelID == nil, "the old model's level is not the new one's")
        model.confirmModel(nil, thoughtLevelID: "low", engineID: claude, conversationID: launch)
        #expect(model.selectedModelID == "sonnet", "a field it did not report keeps its value")
    }

    @Test("A message queued before the pick does not carry it")
    func queuedMessageWithoutPickDoesNotSettle() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        model.selectModel("haiku")
        let drained = ConversationRunID("r2")
        model.notePicksCarried(by: drained, engineID: claude, conversationID: launch,
                               modelID: "fable", thoughtLevelID: "xhigh")
        model.noteRunEnded(drained, conversationID: launch)
        #expect(model.hasPendingModel)
    }

    @Test("A pick made after the carrying run was sent waits for the next message")
    func pickAfterSendSurvivesSettle() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        model.selectModel("haiku")
        let carrying = ConversationRunID("r2")
        model.notePicksCarried(by: carrying, engineID: claude, conversationID: launch,
                               modelID: "haiku", thoughtLevelID: "xhigh")
        // While it runs, the user picks for the message after it.
        model.selectModel("sonnet")
        model.confirmModel("haiku", thoughtLevelID: nil, engineID: claude, conversationID: launch, runID: carrying)
        #expect(model.pendingModelIDs[launch]?.value == "sonnet")
        model.noteRunEnded(carrying, conversationID: launch)
        #expect(model.selectedModelID == "sonnet", "the later pick still rides the next message")
    }

    @Test("A run on another engine does not carry this engine's picks")
    func otherEngineRunDoesNotCarry() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        model.selectThoughtLevel("medium")
        let codexRun = ConversationRunID("r2")
        model.notePicksCarried(by: codexRun, engineID: codex, conversationID: launch,
                               modelID: "gpt-5", thoughtLevelID: "medium")
        model.noteRunEnded(codexRun, conversationID: launch)
        #expect(model.pendingThoughtLevelIDs[launch]?.value == "medium")
    }

    @Test("The agent's word about a run is filed under that run's engine")
    func confirmationUsesTheRunsEngine() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        let started = ConversationRunID("r2")
        model.notePicksCarried(by: started, engineID: claude, conversationID: launch,
                               modelID: "fable", thoughtLevelID: "xhigh")
        // The user picks Codex for the next message while the Claude run reports.
        model.selectedEngineID = codex
        #expect(model.confirmingEngineID(runID: started, conversationID: launch) == claude)
        #expect(model.confirmingEngineID(runID: .init("elsewhere"), conversationID: other) == nil,
                "an unopened chat waits for its detail, not the new-chat agent")
    }

    @Test("A session from before Core kept the model shows its latest run's request")
    func legacySessionFallsBackToRun() {
        let (model, _) = makeModel()
        open(model, launch,
             runs: [run("r2", launch, engine: claude, model: "fable", level: "xhigh"),
                    run("r1", launch, engine: claude, model: "sonnet", level: "low")],
             sessions: [session("s1", launch, engine: claude, model: nil, level: nil)])
        #expect(model.selectedModelID == "fable")
        #expect(model.effectiveThoughtLevelID == "xhigh")

        // A legacy chat on the engine default shows the default, not New Chat's pick.
        open(model, other, runs: [run("r3", other, engine: claude, model: nil, level: nil)], sessions: [])
        #expect(model.selectedModelID == nil)
        #expect(model.effectiveThoughtLevelID == nil)
    }

    @Test("A reloaded detail replaces what an earlier one confirmed")
    func detailReplacesConfirmed() {
        let (model, _) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: claude, model: "fable", level: "xhigh")], sessions: [])
        open(model, launch,
             runs: [run("r2", launch, engine: claude, model: nil, level: nil),
                    run("r1", launch, engine: claude, model: "fable", level: "xhigh")],
             sessions: [])
        #expect(model.selectedModelID == nil, "the latest run went back to the default")
    }

    @Test("Opening a chat on another agent leaves the new-chat agent alone")
    func engineDoesNotLeak() {
        let (model, defaults) = makeModel()
        open(model, launch, runs: [run("r1", launch, engine: codex, model: "gpt-5", level: "high")],
             sessions: [session("s1", launch, engine: codex, model: "gpt-5", level: "high")])
        #expect(model.selectedEngineID == codex)
        #expect(model.selectedModelID == "gpt-5")
        #expect(model.draftEngineID == claude)
        #expect(defaults.string(forKey: "chat.selectedEngineID") == "claude")

        model.selectedConversationID = nil
        #expect(model.selectedEngineID == claude)
        #expect(model.selectedModelID == "opus")
    }

    @Test("Switching a chat's agent is that chat's pick, with the agent's own defaults")
    func engineSwitchInsideChat() {
        let (model, defaults) = makeModel()
        defaults.set("gpt-5", forKey: "chat.selectedModelID.codex")
        open(model, launch, runs: [run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s1", launch, engine: claude, model: "fable", level: "xhigh")])

        model.selectedEngineID = codex
        #expect(model.selectedEngineID == codex)
        #expect(model.selectedModelID == "gpt-5", "Codex's saved default, not Claude's model")
        #expect(model.draftEngineID == claude)
        #expect(defaults.string(forKey: "chat.selectedEngineID") == "claude")

        model.selectModel("gpt-5-mini")
        #expect(defaults.string(forKey: "chat.selectedModelID.codex") == "gpt-5")

        // The next run lands on Codex: the pick is done, Codex is the chat's engine.
        open(model, launch,
             runs: [run("r2", launch, engine: codex, model: "gpt-5-mini", level: nil),
                    run("r1", launch, engine: claude, model: nil, level: nil)],
             sessions: [session("s2", launch, engine: codex, model: "gpt-5-mini", level: nil),
                        session("s1", launch, engine: claude, model: "fable", level: "xhigh")])
        #expect(model.pendingEngineIDs[launch] == nil)
        #expect(model.selectedModelID == "gpt-5-mini")

        // Back to Claude: its session's model, not the new-chat default.
        model.selectedEngineID = claude
        #expect(model.selectedModelID == "fable")
    }
}
