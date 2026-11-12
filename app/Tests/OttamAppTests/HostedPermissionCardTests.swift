import AppKit
import OttamBackendClient
import OttamMarkdownEngine
import SwiftUI
import Testing
@testable import OttamApp

@Suite("Hosted permission card")
@MainActor
struct HostedPermissionCardTests {
    private let theme = MarkdownTheme.ottam(.builtIn)
    private let environment = HostedCardEnvironment(typography: .builtIn)

    static func previewPermission() -> ChatPendingPermission {
        ChatPendingPermission(previewRequest: AgentRequestPermissionParams(
            agentID: AgentID("codex"),
            sessionID: AgentSessionID("s1"),
            workspaceID: nil, projectID: nil, conversationID: nil, runID: nil,
            toolCall: .object([
                "toolCallId": .string("call-1"),
                "title": .string("Run a command"),
                "command": .string("xcrun swift -e 'import Foundation; print(1)'"),
            ]),
            options: [
                AgentPermissionOption(optionID: "allow_once", name: "Allow Once", kind: .allowOnce),
                AgentPermissionOption(optionID: "reject", name: "Reject", kind: .rejectOnce),
            ]
        ))
    }

    @Test("the disclosure toggle is payload state and grows the measured height")
    func toggleIsPayloadState() {
        var resolved: String?? = nil
        let card = HostedPermissionCard.permission(
            Self.previewPermission(), theme: theme, environment: environment
        ) { resolved = .some($0) }
        let collapsed = card.height(forWidth: 500)
        card.actions.onToggleDetails()
        #expect(card.payload.showsDetails)
        #expect(card.height(forWidth: 500) > collapsed)
        card.actions.onResolve("allow_once")
        #expect(resolved == .some("allow_once"))
    }

    @Test("adopting a fresh payload keeps an open disclosure")
    func adoptKeepsDisclosure() {
        let permission = Self.previewPermission()
        let live = HostedPermissionCard.permission(permission, theme: theme, environment: environment) { _ in }
        live.actions.onToggleDetails()
        let fresh = HostedPermissionCard.permission(permission, theme: theme, environment: environment) { _ in }
        #expect(live.adoptPayload(from: fresh) == false, "same permission, disclosure kept: nothing changed")
        #expect(live.payload.showsDetails)
    }
}
