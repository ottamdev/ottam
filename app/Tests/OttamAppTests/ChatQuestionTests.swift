import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Agent questions")
@MainActor
struct ChatQuestionTests {
    private let options = [
        AgentQuestionOption(value: "refactor", label: "Refactor", description: "Smaller diff."),
        AgentQuestionOption(value: "rewrite", label: "Rewrite"),
        AgentQuestionOption(value: "defer", label: "Defer"),
    ]

    private func pending(_ questions: [AgentQuestion]) -> ChatPendingQuestion {
        ChatPendingQuestion(
            request: AgentAskQuestionParams(
                agentID: AgentID("claude"),
                sessionID: AgentSessionID("session"),
                title: "Please answer the following questions.",
                questions: questions
            ),
            continuation: nil
        )
    }

    @Test("a single select keeps one pick and sends its Other text beside it")
    func singleSelect() throws {
        let item = AgentQuestion(
            id: "q0", prompt: "Which approach?", required: true,
            kind: .singleSelect(options: options, allowsOther: true, default: nil)
        )
        let question = pending([item])
        #expect(question.problem(item) == "Choose an option.")
        #expect(!question.canSubmit)
        question.toggle("refactor", in: item)
        question.toggle("rewrite", in: item)
        question.text["q0"] = "  keep it small  "
        #expect(question.canSubmit)
        #expect(question.answers == [AgentQuestionAnswer(questionID: "q0", values: ["rewrite"], other: "keep it small")])
    }

    @Test("Other alone answers a required select")
    func otherAlone() {
        let item = AgentQuestion(
            id: "q0", prompt: "Which approach?", required: true,
            kind: .singleSelect(options: options, allowsOther: true, default: nil)
        )
        let question = pending([item])
        question.text["q0"] = "Something else"
        #expect(question.canSubmit)
        #expect(question.answers == [AgentQuestionAnswer(questionID: "q0", values: [], other: "Something else")])
    }

    @Test("a multi select sends picks in option order and honors its bounds")
    func multiSelect() {
        let item = AgentQuestion(
            id: "q1", prompt: "Which targets?", required: false,
            kind: .multiSelect(options: options, allowsOther: false, minSelected: 2, maxSelected: 2, default: [])
        )
        let question = pending([item])
        // Unanswered and optional: nothing to object to.
        #expect(question.problem(item) == nil)
        question.toggle("defer", in: item)
        #expect(question.problem(item) == "Choose at least 2 options.")
        question.toggle("refactor", in: item)
        question.toggle("rewrite", in: item)
        #expect(question.selections["q1"] == ["defer", "refactor"], "a pick past the maximum is refused")
        #expect(question.answers == [AgentQuestionAnswer(questionID: "q1", values: ["refactor", "defer"])])
        question.toggle("defer", in: item)
        #expect(question.selections["q1"] == ["refactor"])
    }

    @Test("defaults prefill the draft")
    func defaults() {
        let question = pending([
            AgentQuestion(id: "a", prompt: "A", required: true,
                          kind: .singleSelect(options: options, allowsOther: false, default: "rewrite")),
            AgentQuestion(id: "b", prompt: "B", required: true,
                          kind: .number(integer: true, minimum: 1, maximum: 10, default: 3)),
            AgentQuestion(id: "c", prompt: "C", required: true, kind: .boolean(default: false)),
        ])
        #expect(question.canSubmit)
        #expect(question.answers.map(\.values) == [["rewrite"], ["3"], ["false"]])
    }

    @Test("a number is checked against its kind and range")
    func number() {
        let item = AgentQuestion(id: "n", prompt: "How many?", required: true,
                                 kind: .number(integer: true, minimum: 1, maximum: 5, default: nil))
        let question = pending([item])
        #expect(question.problem(item) == "Enter a number.")
        question.text["n"] = "2.5"
        #expect(question.problem(item) == "Enter a whole number.")
        question.text["n"] = "9"
        #expect(question.problem(item) == "Enter 5 or less.")
        question.text["n"] = "4"
        #expect(question.problem(item) == nil)
    }

    @Test("a bound or default past Int's range shows without trapping")
    func hugeNumbers() {
        let item = AgentQuestion(id: "n", prompt: "How many?", required: true,
                                 kind: .number(integer: true, minimum: -1e300, maximum: 1e20, default: 1e19))
        let question = pending([item])
        #expect(question.text["n"] == String(1e19))
        question.text["n"] = "1e21"
        #expect(question.problem(item) == "Enter \(String(1e20)) or less.")
        #expect(ChatPendingQuestion.format(3) == "3")
        #expect(ChatPendingQuestion.format(2.5) == "2.5")
    }

    @Test("the tool row shows the answer, never a secret, or why there is none")
    func recordLabels() {
        let answered = TranscriptQuestion(requestID: "r", state: .answered, answers: [
            .init(prompt: "Approach", values: ["Refactor", "keep it small"], secure: false),
            .init(prompt: "Token", values: [], secure: true),
        ])
        #expect(ChatActivityGroupRow.questionLabel(answered) == "Refactor, keep it small · Answered")
        #expect(ChatActivityGroupRow.questionLabel(.init(requestID: "r", state: .skipped, answers: [])) == "Skipped")
        #expect(ChatActivityGroupRow.questionLabel(.init(requestID: "r", state: .notAnswered, answers: [])) == "Not answered")
    }

    @Test("the asking tool's activity carries its question record")
    func assembly() throws {
        var item = ConversationTranscriptItem(
            itemID: "ask", conversationID: TranscriptFixture.conversationID, runID: .init("run-1"),
            position: 1, revision: 1, kind: .tool, completion: .complete,
            createdAt: "2026-10-14T10:00:00Z", updatedAt: "2026-10-14T10:00:00Z",
            bodyPreview: "", bodyByteCount: 0
        )
        item.tool = .init(kind: "other", title: "AskUserQuestion", arguments: .null, locations: [],
                          partialMetadata: false, payloads: [])
        item.tool?.question = .init(requestID: "r", state: .skipped, answers: [])
        let entries = ChatTranscriptAssembler.plan(.init(normalizedItems: [item]))
        guard case .card(.activityGroup(let activities, _)) = entries.first?.content else {
            Issue.record("expected an activity group, got \(entries.map(\.content))"); return
        }
        #expect(try #require(activities.first).question?.state == .skipped)
    }
}
