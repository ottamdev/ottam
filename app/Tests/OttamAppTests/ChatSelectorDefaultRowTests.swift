import Testing
@testable import OttamApp

@Suite("Chat selector default row")
struct ChatSelectorDefaultRowTests {
    @Test("A name that already says default is used as-is")
    func labelKeepsSelfDescribingName() {
        #expect(ChatSelectorDefaultRow.label("Default (recommended)") == "Default (recommended)")
        #expect(ChatSelectorDefaultRow.label("default") == "default")
    }

    @Test("Any other name is qualified, and an unknown one falls back")
    func labelQualifiesOtherNames() {
        #expect(ChatSelectorDefaultRow.label("GPT-5") == "Default — GPT-5")
        #expect(ChatSelectorDefaultRow.label(nil) == "Default")
    }

    /// Claude's shape: a literal `default` model whose display name already
    /// reads "Default (recommended)". The synthesized row and the option row
    /// render the same string, so the page must show one of them.
    @Test("An option that reads identically absorbs the synthesized row")
    func identicalLabelCollapses() {
        let absorbed = ChatSelectorDefaultRow.absorbingOption(
            defaultID: "default",
            defaultName: "Default (recommended)",
            options: [
                (id: "default", label: "Default (recommended)"),
                (id: "opus", label: "Opus (1M context)")
            ]
        )
        #expect(absorbed == "default")
    }

    /// Codex's shape, and the reason the rule is not "is the default
    /// advertised". "Default — GPT-5" above a "GPT-5" row are two distinct
    /// choices: following the agent, or pinning the model it happens to run
    /// today. Collapsing here would delete the second one.
    @Test("An option carrying the default id but a different label keeps both rows")
    func differingLabelKeepsBothRows() {
        let absorbed = ChatSelectorDefaultRow.absorbingOption(
            defaultID: "gpt-5",
            defaultName: "GPT-5",
            options: [
                (id: "gpt-5", label: "GPT-5"),
                (id: "gpt-5-codex", label: "GPT-5 Codex")
            ]
        )
        #expect(absorbed == nil)
    }

    @Test("A default the agent does not advertise keeps the synthesized row")
    func unadvertisedDefaultKeepsSynthesizedRow() {
        let absorbed = ChatSelectorDefaultRow.absorbingOption(
            defaultID: "retired-model",
            defaultName: "Default (recommended)",
            options: [(id: "opus", label: "Default (recommended)")]
        )
        #expect(absorbed == nil)
    }

    @Test("An agent that reports no default keeps the synthesized row")
    func noDefaultKeepsSynthesizedRow() {
        let absorbed = ChatSelectorDefaultRow.absorbingOption(
            defaultID: nil,
            defaultName: nil,
            options: [(id: "default", label: "Default")]
        )
        #expect(absorbed == nil)
    }

    @Test("An agent with no options at all keeps the synthesized row")
    func emptyOptionsKeepSynthesizedRow() {
        let absorbed = ChatSelectorDefaultRow.absorbingOption(
            defaultID: "default",
            defaultName: "Default (recommended)",
            options: []
        )
        #expect(absorbed == nil)
    }
}
