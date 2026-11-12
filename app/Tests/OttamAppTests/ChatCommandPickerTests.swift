import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Command picker")
@MainActor
struct ChatCommandPickerTests {
    private let modes = [
        EngineSelectOption(id: "default", displayName: "Default"),
        EngineSelectOption(id: "plan", displayName: "Plan"),
    ]
    private let commands = [
        ConversationSessionCommand(name: "review", description: "Review changes", inputHint: "what to review"),
        ConversationSessionCommand(name: "plan", description: "Switch to plan mode"),
        ConversationSessionCommand(name: "compact", description: "Summarize the conversation"),
    ]

    private func picker(_ draft: String, commands: [ConversationSessionCommand]? = nil) -> ChatCommandPicker {
        var picker = ChatCommandPicker()
        picker.update(draft: draft, modes: modes, currentModeID: "default", commands: commands ?? self.commands)
        return picker
    }

    @Test("only a bare `/word` draft is a query")
    func query() {
        #expect(ChatCommandPicker.query("/")?.text == "")
        #expect(ChatCommandPicker.query("/pl")?.text == "pl")
        #expect(ChatCommandPicker.query("$pu")?.trigger == .dollar)
        #expect(ChatCommandPicker.query("/plan write it") == nil)
        #expect(ChatCommandPicker.query("hello /plan") == nil)
        #expect(!picker("/review ").isOpen)
    }

    @Test("`/` lists skills in their own section; `$` lists only skills")
    func skills() {
        let withSkills = commands + [ConversationSessionCommand(name: "$push", description: "Commit and push")]
        let slash = picker("/pu", commands: withSkills)
        #expect(slash.skills.map(\.id) == ["command/$push"])
        #expect(slash.highlightedID == "command/$push")
        let dollar = picker("$", commands: withSkills)
        #expect(dollar.isOpen)
        #expect(dollar.items.map(\.id) == ["command/$push"])
        // An agent without skills leaves `$` as text.
        #expect(!picker("$", commands: commands).isOpen)
    }

    @Test("`/` lists modes, then commands, the first highlighted")
    func listsEverything() {
        let all = picker("/")
        #expect(all.isOpen)
        #expect(all.modes.map(\.id) == ["mode/default", "mode/plan"])
        #expect(all.commands.map(\.id) == ["command/review", "command/plan", "command/compact"])
        #expect(all.modes.first == .mode(modes[0], isCurrent: true))
        #expect(all.highlightedID == "mode/default")
    }

    @Test("name prefixes rank before descriptions")
    func filters() {
        let plan = picker("/pl")
        #expect(plan.items.map(\.id) == ["mode/plan", "command/plan"])
        // "conversation" is only in compact's description.
        #expect(picker("/conversation").items.map(\.id) == ["command/compact"])
        #expect(picker("/zzz").items.isEmpty)
        #expect(picker("/zzz").isOpen)
    }

    @Test("arrows wrap, and the highlight survives narrowing when its row does")
    func moves() {
        var all = picker("/")
        all.move(by: -1)
        #expect(all.highlightedID == "command/compact")
        all.move(by: 1)
        #expect(all.highlightedID == "mode/default")
        all.move(by: 3)
        #expect(all.highlightedID == "command/plan")
        all.update(draft: "/pl", modes: modes, currentModeID: "default", commands: commands)
        #expect(all.highlightedID == "command/plan")
        all.update(draft: "/rev", modes: modes, currentModeID: "default", commands: commands)
        #expect(all.highlightedID == "command/review")
    }

    @Test("an unmoved highlight follows the best row as the query narrows")
    func highlightFollowsTop() {
        var typed = ChatCommandPicker()
        typed.update(draft: "/", modes: [], currentModeID: nil, commands: commands)
        #expect(typed.highlightedID == "command/review")
        // review still matches "c" through its description, but compact leads.
        typed.update(draft: "/c", modes: [], currentModeID: nil, commands: commands)
        #expect(typed.highlightedID == "command/compact")
    }

    @Test("a name typed in full wins over a mode it prefixes")
    func exactNameWins() {
        var typed = ChatCommandPicker()
        let reviewing = [EngineSelectOption(id: "reviewing", displayName: "Reviewing")]
        typed.update(draft: "/review", modes: reviewing, currentModeID: nil, commands: commands)
        #expect(typed.items.first?.id == "mode/reviewing")
        #expect(typed.highlightedID == "command/review")
    }

    @Test("a name the query starts wins over a mode that only contains it")
    func prefixBeatsContains() {
        var typed = ChatCommandPicker()
        // Codex: "/rev" matched inside "Auto review" and Return switched mode.
        let autoReview = [EngineSelectOption(id: "agent", displayName: "Auto review")]
        typed.update(draft: "/rev", modes: autoReview, currentModeID: nil, commands: commands)
        #expect(typed.items.first?.id == "mode/agent")
        #expect(typed.highlightedID == "command/review")
        // With no name prefixed, the top row still leads.
        typed.update(draft: "/view", modes: autoReview, currentModeID: nil, commands: commands)
        #expect(typed.highlightedID == "mode/agent")
    }

    @Test("Esc closes until the draft stops being a query")
    func dismisses() {
        var open = picker("/pl")
        open.isDismissed = true
        open.update(draft: "/pla", modes: modes, currentModeID: "default", commands: commands)
        #expect(!open.isOpen)
        open.update(draft: "", modes: modes, currentModeID: "default", commands: commands)
        open.update(draft: "/", modes: modes, currentModeID: "default", commands: commands)
        #expect(open.isOpen)
    }

    @Test("before an agent reports commands, the card still lists modes")
    func noCommandsYet() {
        var empty = ChatCommandPicker()
        empty.update(draft: "/", modes: modes, currentModeID: nil, commands: nil)
        #expect(empty.isOpen)
        #expect(!empty.hasCommands)
        #expect(empty.items.count == 2)
    }

    @Test("an engine's last commands outlive the model, keyed by engine")
    func remembersEngineCommands() throws {
        let defaults = try #require(UserDefaults(suiteName: "ChatCommandPickerTests.\(UUID().uuidString)"))
        let model = ChatWorkspaceModel(defaults: defaults)
        let conversation = ConversationID("c1")
        model.selectedEngineID = EngineID("codex")
        model.selectedConversationID = conversation
        #expect(model.selectedCommands == nil)
        model.recordCommands(commands, engineID: EngineID("codex"), conversationID: conversation)
        #expect(model.selectedCommands == commands)
        // Switching the conversation to another engine doesn't offer Codex's.
        model.selectedEngineID = EngineID("claude")
        #expect(model.selectedCommands == nil)

        let relaunched = ChatWorkspaceModel(defaults: defaults)
        relaunched.selectedEngineID = EngineID("codex")
        relaunched.selectedConversationID = ConversationID("c2")
        #expect(relaunched.selectedCommands == commands)
        relaunched.selectedEngineID = EngineID("claude")
        #expect(relaunched.selectedCommands == nil)
    }
}
