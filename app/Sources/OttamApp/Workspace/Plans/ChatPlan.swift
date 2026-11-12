import OttamBackendClient
import Foundation

/// A plan an agent asked to approve before it leaves plan mode. Core spots
/// plan requests and sends the markdown as `plan` on the request and on its
/// permission record. See "Plan Review And File Tabs" in
/// `docs/architecture/app/chat/README.md`.
struct ChatPlan: Equatable {
    /// The request's title, such as "Implement this plan?".
    var title: String
    var markdown: String

    /// The plan's first line of text, without heading, quote, or list
    /// markers. Inline markdown (code, emphasis) is kept for the caller to
    /// render.
    var summary: String {
        for line in markdown.split(whereSeparator: \.isNewline) {
            var text = Substring(line.trimmingCharacters(in: .whitespaces))
            // A list marker needs its space, so a bold first line keeps its `**`.
            if let marker = text.firstMatch(of: #/^(?:[#>]+\s*|[-*•+]\s+|\d+[.)]\s+)+/#) {
                text = text[marker.range.upperBound...]
            }
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            if !trimmed.isEmpty { return trimmed }
        }
        return ""
    }

    /// `summary` with its inline markdown applied.
    var attributedSummary: AttributedString {
        (try? AttributedString(
            markdown: summary,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        )) ?? AttributedString(summary)
    }
}

extension TranscriptPermission {
    /// The decided plan, as its transcript row opens it; nil for any other
    /// permission.
    var chatPlan: ChatPlan? { plan.map { ChatPlan(title: title, markdown: $0) } }
}

extension AgentRequestPermissionParams {
    /// The plan this request asks to approve, or nil for any other request.
    var chatPlan: ChatPlan? {
        guard let plan else { return nil }
        let title: String
        if case .object(let call) = toolCall, case .string(let value)? = call["title"], !value.isEmpty {
            title = value
        } else {
            title = "Approve this plan?"
        }
        return ChatPlan(title: title, markdown: plan)
    }
}

extension ChatPendingPermission {
    var plan: ChatPlan? { request.chatPlan }

    /// The option that keeps the agent planning, which Revise picks before
    /// the feedback goes out as the next prompt.
    var reviseOption: AgentPermissionOption? {
        request.options.first { $0.kind == .rejectOnce }
            ?? request.options.first { $0.kind == .rejectAlways }
    }

    /// The options shown as rows: every way to go ahead. Revise covers the
    /// option that keeps planning; with no such option, every option is a row.
    var planRows: [AgentPermissionOption] {
        guard let revise = reviseOption else { return request.options }
        return request.options.filter { $0.optionID != revise.optionID }
    }
}
