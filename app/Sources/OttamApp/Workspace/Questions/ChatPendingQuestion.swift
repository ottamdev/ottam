import OttamBackendClient
import Foundation
import Observation
import SwiftUI

/// An agent's form waiting on the user, and the draft of its answers.
///
/// The draft lives here rather than in the panel's view state so it survives
/// switching conversations and back, and collapsing the panel.
@MainActor
@Observable
final class ChatPendingQuestion: Identifiable {
    let id: UUID
    let request: AgentAskQuestionParams
    /// The question on screen, an index into `request.questions`.
    var page = 0
    var isCollapsed = false
    /// Picked option values per question id, in the order they were picked.
    var selections: [String: [String]] = [:]
    /// Free text per question id: a select's Other answer, or a text or
    /// number field's value.
    var text: [String: String] = [:]
    var toggles: [String: Bool] = [:]
    @ObservationIgnored private var continuation: CheckedContinuation<AgentQuestionOutcome, Never>?

    init(
        id: UUID = UUID(),
        request: AgentAskQuestionParams,
        continuation: CheckedContinuation<AgentQuestionOutcome, Never>?
    ) {
        self.id = id
        self.request = request
        self.continuation = continuation
        for question in request.questions {
            switch question.kind {
            case .singleSelect(_, _, let value):
                if let value { selections[question.id] = [value] }
            case .multiSelect(_, _, _, _, let values):
                if !values.isEmpty { selections[question.id] = values }
            case .text(_, let value):
                if let value { text[question.id] = value }
            case .number(_, _, _, let value):
                if let value { text[question.id] = Self.format(value) }
            case .boolean(let value):
                if let value { toggles[question.id] = value }
            }
        }
    }

    var questions: [AgentQuestion] { request.questions }

    var current: AgentQuestion? {
        questions.indices.contains(page) ? questions[page] : nil
    }

    var isLastPage: Bool { page >= questions.count - 1 }

    func resolve(_ outcome: AgentQuestionOutcome) {
        continuation?.resume(returning: outcome)
        continuation = nil
    }

    // MARK: Editing

    func isSelected(_ value: String, in question: AgentQuestion) -> Bool {
        selections[question.id, default: []].contains(value)
    }

    /// Picks `value`: a single select replaces its pick, a multi select
    /// toggles it, refusing a pick past its maximum.
    func toggle(_ value: String, in question: AgentQuestion) {
        var picked = selections[question.id, default: []]
        switch question.kind {
        case .singleSelect:
            picked = [value]
        case .multiSelect(_, _, _, let maximum, _):
            if let index = picked.firstIndex(of: value) {
                picked.remove(at: index)
            } else if maximum.map({ picked.count < $0 }) ?? true {
                picked.append(value)
            }
        default:
            return
        }
        selections[question.id] = picked
    }

    func textBinding(_ questionID: String) -> Binding<String> {
        Binding(
            get: { self.text[questionID, default: ""] },
            set: { self.text[questionID] = $0 }
        )
    }

    // MARK: Answers

    func trimmedText(_ question: AgentQuestion) -> String? {
        let value = text[question.id]?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return value.isEmpty ? nil : value
    }

    /// Why `question`'s current answer can't be sent, if it can't.
    func problem(_ question: AgentQuestion) -> String? {
        switch question.kind {
        case .singleSelect:
            if question.required, selections[question.id, default: []].isEmpty,
               trimmedText(question) == nil {
                return "Choose an option."
            }
        case .multiSelect(_, _, let minimum, _, _):
            let count = selections[question.id, default: []].count
            if let minimum, count < minimum, count > 0 || question.required {
                return minimum == 1 ? "Choose at least one option." : "Choose at least \(minimum) options."
            }
            if question.required, count == 0, trimmedText(question) == nil {
                return "Choose an option."
            }
        case .text:
            if question.required, trimmedText(question) == nil {
                return "Enter an answer."
            }
        case .number(let integer, let minimum, let maximum, _):
            guard let raw = trimmedText(question) else {
                return question.required ? "Enter a number." : nil
            }
            guard let number = Double(raw), number.isFinite else { return "Enter a number." }
            if integer, number.rounded() != number { return "Enter a whole number." }
            if let minimum, number < minimum { return "Enter \(Self.format(minimum)) or more." }
            if let maximum, number > maximum { return "Enter \(Self.format(maximum)) or less." }
        case .boolean:
            if question.required, toggles[question.id] == nil {
                return "Choose yes or no."
            }
        }
        return nil
    }

    var canSubmit: Bool { questions.allSatisfy { problem($0) == nil } }

    func isAnswered(_ question: AgentQuestion) -> Bool {
        !answer(question).values.isEmpty || answer(question).other != nil
    }

    func answer(_ question: AgentQuestion) -> AgentQuestionAnswer {
        switch question.kind {
        case .singleSelect(let options, let allowsOther, _),
             .multiSelect(let options, let allowsOther, _, _, _):
            let picked = selections[question.id, default: []]
            // Option order, not pick order: the agent reads them as a list.
            let values = options.map(\.value).filter(picked.contains)
            return AgentQuestionAnswer(
                questionID: question.id,
                values: values,
                other: allowsOther ? trimmedText(question) : nil
            )
        case .text, .number:
            return AgentQuestionAnswer(
                questionID: question.id,
                values: trimmedText(question).map { [$0] } ?? []
            )
        case .boolean:
            return AgentQuestionAnswer(
                questionID: question.id,
                values: toggles[question.id].map { [$0 ? "true" : "false"] } ?? []
            )
        }
    }

    var answers: [AgentQuestionAnswer] {
        questions.map(answer).filter { !$0.values.isEmpty || $0.other != nil }
    }

    /// A whole number without its `.0`. The value is the agent's, so one
    /// past `Int`'s range keeps the floating-point form rather than trapping.
    nonisolated static func format(_ number: Double) -> String {
        Int(exactly: number).map(String.init) ?? String(number)
    }
}
