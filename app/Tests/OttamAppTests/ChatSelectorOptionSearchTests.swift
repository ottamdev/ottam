import Testing
@testable import OttamApp

@Suite("Chat selector option search")
struct ChatSelectorOptionSearchTests {
    private let options: [(id: String, label: String)] = [
        (id: "zai/glm-5.2", label: "GLM-5.2"),
        (id: "zai/glm-5.3", label: "GLM-5.3"),
        (id: "zai/glm-5.3-flash", label: "GLM-5.3-Flash"),
        (id: "moonshot/kimi-k3", label: "Kimi K3"),
    ]

    private func labels(for query: String) -> [String] {
        ChatSelectorOptionSearch.matches(options, query: query, id: \.id, label: \.label).map(\.label)
    }

    @Test("Only pages longer than the threshold are searchable")
    func threshold() {
        #expect(!ChatSelectorOptionSearch.isSearchable(optionCount: ChatSelectorOptionSearch.threshold))
        #expect(ChatSelectorOptionSearch.isSearchable(optionCount: ChatSelectorOptionSearch.threshold + 1))
    }

    @Test("An empty or blank query keeps every option in order")
    func blankQuery() {
        #expect(labels(for: "") == ["GLM-5.2", "GLM-5.3", "GLM-5.3-Flash", "Kimi K3"])
        #expect(labels(for: "   ") == ["GLM-5.2", "GLM-5.3", "GLM-5.3-Flash", "Kimi K3"])
    }

    @Test("Every term must match, ignoring case")
    func allTermsMatch() {
        #expect(labels(for: "glm 5.3") == ["GLM-5.3", "GLM-5.3-Flash"])
        #expect(labels(for: "GLM flash") == ["GLM-5.3-Flash"])
        #expect(labels(for: "glm kimi").isEmpty)
    }

    @Test("A term can match the id when the label does not contain it")
    func matchesID() {
        #expect(labels(for: "moonshot") == ["Kimi K3"])
    }
}
