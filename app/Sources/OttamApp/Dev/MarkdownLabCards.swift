#if DEBUG
import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// A real `ChatPermissionCard` hosted as a native collection item. The
/// `markdown-lab.cards` scenario proves rendering, buttons, height invalidation
/// and SwiftUI accessibility through the production AppKit transcript path.
struct MarkdownLabCardsView: View {
    @State private var resolved: String?
    @State private var permission: ChatPendingPermission

    init() {
        _permission = State(initialValue: Self.previewPermission())
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(resolved.map { "Resolved: \($0)" } ?? "Pending approval")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(AccessibilityID.markdownEngineLabCardResolved)
                    .accessibilityValue(resolved ?? "pending")
                Spacer()
            }
            .padding(12)
            Divider()
            MarkdownLabNativeTranscript(
                plan: Self.plan(permission: permission),
                makeCards: { theme, environment in
                    let card = HostedPermissionCard.permission(
                        permission,
                        theme: theme,
                        environment: environment
                    ) { optionID in
                        resolved = optionID ?? "cancelled"
                    }
                    return ["permission-1": card]
                }
            )
        }
    }

    private static func plan(permission: ChatPendingPermission) -> [TranscriptItem] {
        [
            TranscriptItem(
                id: "user-1",
                content: .markdown(
                    "Can you probe how macOS 26 carries Markdown list intents?",
                    syntax: .full,
                    container: .bubble
                )
            ),
            TranscriptItem(
                id: "assistant-1",
                content: .markdown(Self.lead, syntax: .full, container: nil)
            ),
            TranscriptItem(
                id: "permission-1",
                content: .card(.pendingPermission(permission))
            ),
            TranscriptItem(
                id: "assistant-2",
                content: .markdown(Self.tail, syntax: .full, container: nil)
            ),
        ]
    }

    private static let lead = """
    I'll verify whether `TextEditor` expands list support or merely shares the \
    same inline/paragraph-attribute set. The probe only inspects how Foundation's \
    full Markdown parser carries list intents into attributed text; it won't touch \
    the project.
    """

    /// Long enough to scroll the card out of the viewport and back through
    /// native collection-item reuse.
    private static let tail = (1...30).map { index in
        "Follow-up paragraph \(index): the retry succeeded, and the macOS 26 bridge carries " +
        "unordered-list and list-item presentation intents into `NSAttributedString`."
    }.joined(separator: "\n\n")
}

extension MarkdownLabCardsView {
    /// A pending permission with nothing awaiting it; the tool call is long
    /// enough to wrap and to give the details disclosure something to show.
    static func previewPermission() -> ChatPendingPermission {
        ChatPendingPermission(previewRequest: AgentRequestPermissionParams(
            agentID: AgentID("codex"),
            sessionID: AgentSessionID("lab"),
            toolCall: .object([
                "toolCallId": .string("lab-call-1"),
                "title": .string("Run a command"),
                "command": .string(
                    "xcrun swift -e 'import Foundation; import AppKit; "
                    + "let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .full); "
                    + "print(try AttributedString(markdown: \"- one\\n- two\", options: options))'"
                ),
            ]),
            options: [
                AgentPermissionOption(optionID: "allow_once", name: "Allow Once", kind: .allowOnce),
                AgentPermissionOption(optionID: "allow_always", name: "Allow Always", kind: .allowAlways),
                AgentPermissionOption(optionID: "reject", name: "Reject", kind: .rejectOnce),
            ]
        ))
    }
}
#endif
