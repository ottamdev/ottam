import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("Agent avatars")
struct AgentAvatarTests {
    @Test("a name's seed is FNV-1a over its lowercased bytes, so it never changes between launches")
    func nameSeed() {
        #expect(AgentAvatar.seed(forName: "") == 2_166_136_261)
        #expect(AgentAvatar.seed(forName: "a") == 0xE40C_292C)
        #expect(AgentAvatar.seed(forName: "Review-Bot") == AgentAvatar.seed(forName: "review-bot"))
        #expect(AgentAvatar.seed(forName: "review-bot") != AgentAvatar.seed(forName: "ask-bot"))
    }

    @Test("a stored seed wins over the name, and a wide value keeps its low 32 bits")
    func storedSeed() {
        #expect(AgentAvatar(seed: 7, name: "review-bot") == AgentAvatar(seed: 7))
        #expect(AgentAvatar(seed: nil, name: "review-bot") == AgentAvatar(seed: AgentAvatar.seed(forName: "review-bot")))
        #expect(AgentAvatar(seed: Int(UInt32.max) + 8, name: "x") == AgentAvatar(seed: 7))
    }

    @Test("a seed always draws the same tile, and neighboring seeds reach every color and shape")
    func tileParts() {
        let avatar = AgentAvatar(seed: 2_654_435_769)
        #expect(avatar.paletteIndex == AgentAvatar(seed: 2_654_435_769).paletteIndex)
        #expect(avatar.shape == AgentAvatar(seed: 2_654_435_769).shape)
        let tiles = (0..<200).map { AgentAvatar(seed: UInt32($0)) }
        #expect(Set(tiles.map(\.paletteIndex)) == Set(0..<AgentAvatar.paletteCount))
        #expect(Set(tiles.map(\.shape)) == Set(AgentAvatar.Shape.allCases))
        // Consecutive seeds do not walk the palette in step.
        #expect(zip(tiles, tiles.dropFirst()).contains { $0.paletteIndex + 1 != $1.paletteIndex })
    }

    @Test("only a delegated child has a tile; a named agent always has one")
    func summaries() {
        var child = Fixture.workspaceFilesConversation("Codex: Review")
        #expect(child.delegatedAvatar == nil)
        child.delegatedAgentName = "Codex"
        #expect(child.delegatedAvatar == AgentAvatar(seed: AgentAvatar.seed(forName: "Codex")))
        child.delegatedAvatarSeed = 42
        #expect(child.delegatedAvatar == AgentAvatar(seed: 42))
    }
}
