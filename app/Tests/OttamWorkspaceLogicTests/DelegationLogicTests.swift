import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("Delegated conversations")
struct DelegationLogicTests {
    /// A parent and two children, the second created later.
    private func family() -> [ConversationSummary] {
        let parent = Fixture.workspaceFilesConversation("Ship the parser")
        var review = Fixture.workspaceFilesConversation("review-bot: Package review")
        review.parentConversationID = parent.conversationID
        review.delegatedAgentName = "review-bot"
        review.delegatedTaskTitle = "Package review"
        review.createdAt = "2026-10-29T10:00:00Z"
        var docs = Fixture.workspaceFilesConversation("Codex: Docs pass")
        docs.parentConversationID = parent.conversationID
        docs.delegatedAgentName = "Codex"
        docs.delegatedTaskTitle = "Docs pass"
        docs.createdAt = "2026-10-29T11:00:00Z"
        docs.delegationActivity = DelegationActivity(
            latestRunStatus: .canceled, commands: 0, reads: 0, edits: 0, searches: 0, fetches: 0, other: 0, subagents: 0
        )
        return [docs, parent, review]
    }

    @Test("a child is named by its task, any other conversation by its title")
    func taskTitle() {
        let conversations = family()
        #expect(conversations[2].taskTitle == "Package review")
        #expect(conversations[1].taskTitle == "Ship the parser")
        // A child core gave no task title keeps its own.
        var untitled = conversations[2]
        untitled.delegatedTaskTitle = nil
        #expect(untitled.taskTitle == "review-bot: Package review")
    }

    private func items(_ conversations: [ConversationSummary]) -> [SidebarConversationItem] {
        SidebarSectionBuilder.sections(
            projects: [], selectedProject: nil, conversations: conversations,
            attentionConversationIDs: [], runningConversationIDs: []
        )[0].allItems
    }

    @Test("Left folds a parent and moves from a child to it; Right unfolds")
    func keyboardFolding() {
        let conversations = family()
        let items = items(conversations)
        let parent = conversations[1].conversationID
        let child = conversations[2].conversationID

        #expect(SidebarKeyboardFold.move(.left, selected: parent, items: items, folded: []) == .fold(parent))
        #expect(SidebarKeyboardFold.move(.left, selected: parent, items: items, folded: [parent]) == nil)
        #expect(SidebarKeyboardFold.move(.right, selected: parent, items: items, folded: [parent]) == .unfold(parent))
        #expect(SidebarKeyboardFold.move(.right, selected: parent, items: items, folded: []) == nil)
        #expect(SidebarKeyboardFold.move(.left, selected: child, items: items, folded: []) == .selectParent(parent))
        #expect(SidebarKeyboardFold.move(.right, selected: child, items: items, folded: []) == nil)
        #expect(SidebarKeyboardFold.move(.left, selected: nil, items: items, folded: []) == nil)
    }

    @Test("the agents group is the parent, then its children oldest first")
    func agentGroup() throws {
        let conversations = family()
        let parent = conversations[1].conversationID
        let review = conversations[2].conversationID
        let fromChild = try #require(ConversationAgentGroup.of(
            review, conversations: conversations, agentNames: [parent: "Claude Code"],
            running: [review], attention: []
        ))
        #expect(fromChild.title == "Claude Code, review-bot, Codex")
        #expect(fromChild.members.map(\.title) == ["Ship the parser", "Package review", "Docs pass"])
        #expect(fromChild.members.map(\.state) == ["Idle", "Working", "Stopped"])
        #expect(fromChild.members.map(\.isParent) == [true, false, false])
        // The parent is its agent's brand; each child its own tile, drawn
        // from its agent's name when core stored no seed.
        #expect(fromChild.members.map(\.icon) == [
            .brand(nil),
            .avatar(AgentAvatar(seed: AgentAvatar.seed(forName: "review-bot"))),
            .avatar(AgentAvatar(seed: AgentAvatar.seed(forName: "Codex"))),
        ])
        // A one-line label cuts a long title to the cap, ending in "…".
        let long = "Avatar walkthrough. Use Ottam's delegate tool three times, then stop without"
        #expect(long.cappedTitle() == "Avatar walkthrough. Use Ottam's delegat…")
        #expect(long.cappedTitle().count == String.titleCap)
        #expect(String(repeating: "a", count: String.titleCap).cappedTitle().count == String.titleCap)
        #expect("  Ship the parser \n".cappedTitle() == "Ship the parser")
        // A cut that lands after a space drops it before the "…".
        #expect("abcd efgh".cappedTitle(limit: 6) == "abcd…")
        let branded = ConversationAgentGroup.of(
            review, conversations: conversations, agentNames: [:], agentBrands: [parent: "anthropic"],
            running: [], attention: []
        )
        #expect(branded?.members.first?.icon == .brand("anthropic"))

        let fromParent = ConversationAgentGroup.of(
            parent, conversations: conversations, agentNames: [:], running: [review], attention: []
        )
        #expect(fromParent?.members.first?.agentName == "Agent")
        #expect(fromParent?.members.count == 3)
    }

    @Test("a conversation that never delegated has no agents group")
    func noAgentGroup() {
        let lone = Fixture.workspaceFilesConversation("Lone")
        #expect(ConversationAgentGroup.of(
            lone.conversationID, conversations: [lone], agentNames: [:], running: [], attention: []
        ) == nil)
        #expect(ConversationAgentGroup.of(nil, conversations: family(), agentNames: [:], running: [], attention: []) == nil)
    }
}
