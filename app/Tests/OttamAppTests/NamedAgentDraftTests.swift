import OttamBackendClient
import OttamWorkspaceLogic
import Testing

@testable import OttamApp

@Suite("Named agent drafts")
struct NamedAgentDraftTests {
    private func agent(name: String, seed: Int?) -> NamedAgent {
        NamedAgent(
            namedAgentID: NamedAgentID("a"), name: name, engineID: EngineID("codex"),
            instructions: "", avatarSeed: seed, createdAt: "", updatedAt: ""
        )
    }

    @Test("an agent saved before tiles keeps the tile its name drew, through a rename")
    func seedlessAgentKeepsItsTile() {
        var draft = NamedAgentDraft(agent(name: "review-bot", seed: nil))
        let drawn = AgentAvatar.seed(forName: "review-bot")
        #expect(draft.config.avatarSeed == Int(drawn))
        draft.name = "linter"
        #expect(draft.avatar == AgentAvatar(seed: drawn))
        #expect(draft.config.avatarSeed == Int(drawn))
    }

    @Test("a stored seed round-trips, and a new agent starts with one")
    func storedAndNewSeeds() {
        #expect(NamedAgentDraft(agent(name: "review-bot", seed: 11)).config.avatarSeed == 11)
        #expect(NamedAgentDraft(engineID: "codex").config.avatarSeed != nil)
    }
}
