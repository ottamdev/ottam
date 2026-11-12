import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Agent sign-in recovery")
@MainActor
struct AgentSignInControllerTests {
    @Test("resuming in Chat leaves Settings dismissed until Settings explicitly resumes")
    func resumeIsLocalToRequestingController() async throws {
        let backend = SignInBackend(delivery: .clientTerminal)
        let settings = AgentSignInController()
        let chat = AgentSignInController()
        let agent = AgentID("terminal-agent")
        await settings.configure(workspaceID: nil, backend: backend)
        await chat.configure(workspaceID: WorkspaceID("chat"), backend: backend)
        settings.signIn(agentID: agent, displayName: "Terminal Agent")
        try await waitForResponse(settings, agent: agent)
        try await backend.emit(phase: .awaitingUser)
        try await waitUntil { settings.revealed != nil }
        #expect(chat.revealed == nil)
        settings.dismissRevealedTerminal()

        chat.signIn(agentID: agent, displayName: "Terminal Agent")
        try await waitForResponse(chat, agent: agent)
        try await backend.emit(phase: .awaitingUser)
        try await waitUntil { chat.revealed != nil }
        #expect(settings.revealed == nil)
        #expect(settings.isSigningIn(agent))

        settings.signIn(agentID: agent, displayName: "Terminal Agent")
        try await waitForResponse(settings, agent: agent)
        try await backend.emit(phase: .awaitingUser)
        try await waitUntil { settings.revealed != nil }
        #expect(settings.revealed?.id == chat.revealed?.id)
        settings.dismissRevealedTerminal()
        chat.dismissRevealedTerminal()
    }

    @Test("dismissed terminals still receive final login state", arguments: [
        AgentAuthPhase.succeeded, .failed, .cancelled,
    ])
    func dismissalPreservesCompletion(phase: AgentAuthPhase) async throws {
        let backend = SignInBackend(delivery: .clientTerminal)
        let controller = AgentSignInController()
        let agent = AgentID("terminal-agent")
        var signedIn: [AgentID] = []
        controller.onSignedIn = { signedIn.append($0) }
        await controller.configure(workspaceID: nil, backend: backend)
        controller.signIn(agentID: agent, displayName: "Terminal Agent")
        try await waitForResponse(controller, agent: agent)
        try await backend.emit(phase: .awaitingUser)
        try await waitUntil { controller.revealed != nil }
        controller.dismissRevealedTerminal()
        try await backend.emit(phase: phase)
        try await waitUntil { !controller.isSigningIn(agent) }
        #expect(controller.revealed == nil)
        #expect(controller.hasJustSignedIn(agent) == (phase == .succeeded))
        #expect(signedIn == (phase == .succeeded ? [agent] : []))
        #expect(controller.failure(for: agent) == (phase == .failed ? "Login failed" : nil))
        if phase == .failed {
            try await waitUntil { backend.closed.count == 1 }
            #expect(backend.closed == [TerminalSessionID("login-pty")])
        } else {
            #expect(backend.closed.isEmpty)
        }
    }

    @Test("failed transcripts close only after the last window releases them", arguments: [0, 1, 2])
    func sharedFailureCleanup(dismissedCount: Int) async throws {
        let backend = SignInBackend(delivery: .clientTerminal)
        let controllers = [AgentSignInController(), AgentSignInController()]
        let agent = AgentID("terminal-agent")
        for controller in controllers {
            await controller.configure(workspaceID: nil, backend: backend)
            controller.signIn(agentID: agent, displayName: "Terminal Agent")
            try await waitForResponse(controller, agent: agent)
        }
        try await backend.emit(phase: .awaitingUser)
        try await waitUntil { controllers.allSatisfy { $0.revealed != nil } }
        for controller in controllers.prefix(dismissedCount) {
            controller.dismissRevealedTerminal()
        }
        #expect(backend.closed.isEmpty)
        try await backend.emit(phase: .failed)
        try await waitUntil { controllers.allSatisfy { !$0.isSigningIn(agent) } }
        for (index, controller) in controllers.enumerated() {
            #expect((controller.revealed == nil) == (index < dismissedCount))
        }
        for controller in controllers.dropFirst(dismissedCount) {
            #expect(backend.closed.isEmpty)
            controller.dismissRevealedTerminal()
        }
        try await waitUntil { backend.closed.count == 1 }
        #expect(backend.closed == [TerminalSessionID("login-pty")])
    }

    private func waitUntil(_ condition: () -> Bool) async throws {
        for _ in 0..<100 where !condition() {
            try await Task.sleep(for: .milliseconds(1))
        }
        try #require(condition())
    }

    @Test("releasing the last controller closes its failed transcript")
    func releasedControllerClosesFailure() async throws {
        let backend = SignInBackend(delivery: .clientTerminal)
        var controller: AgentSignInController? = AgentSignInController()
        let isReleased = { [weak controller] in controller == nil }
        let agent = AgentID("terminal-agent")
        await controller?.configure(workspaceID: nil, backend: backend)
        controller?.signIn(agentID: agent, displayName: "Terminal Agent")
        try await waitUntil { controller?.isTerminalLogin(agent) == true }
        try await backend.emit(phase: .failed)
        try await waitUntil { controller?.revealed != nil }
        #expect(backend.closed.isEmpty)
        controller = nil
        try await waitUntil { backend.closed.count == 1 }
        #expect(isReleased())
    }

    @Test("a live terminal with no visible sheet can be joined again")
    func resumesHiddenTerminal() async throws {
        let backend = SignInBackend(delivery: .clientTerminal)
        let controller = AgentSignInController()
        let agent = AgentID("terminal-agent")
        await controller.configure(workspaceID: nil, backend: backend)
        controller.signIn(agentID: agent, displayName: "Terminal Agent")
        #expect(!controller.canStartOrResume(agent))
        try await waitForResponse(controller, agent: agent)
        #expect(controller.isSigningIn(agent))
        #expect(controller.canStartOrResume(agent))
        controller.signIn(agentID: agent, displayName: "Terminal Agent")
        try await waitForResponse(controller, agent: agent)
        #expect(backend.starts == 2)
    }

    @Test("a browser login remains protected against repeated clicks")
    func doesNotRepeatBrowserLogin() async throws {
        let backend = SignInBackend(delivery: .adapterBrowser)
        let controller = AgentSignInController()
        let agent = AgentID("codex")
        await controller.configure(workspaceID: nil, backend: backend)
        controller.signIn(agentID: agent, displayName: "Codex")
        for _ in 0..<100 where backend.starts == 0 {
            try await Task.sleep(for: .milliseconds(1))
        }
        #expect(backend.starts == 1)
        #expect(!controller.canStartOrResume(agent))
        controller.signIn(agentID: agent, displayName: "Codex")
        #expect(backend.starts == 1)
    }

    private func waitForResponse(_ controller: AgentSignInController, agent: AgentID) async throws {
        for _ in 0..<100 where !controller.isTerminalLogin(agent) {
            try await Task.sleep(for: .milliseconds(1))
        }
        #expect(controller.isTerminalLogin(agent))
    }
}

@MainActor
private final class SignInBackend: AgentSignInBackend {
    let delivery: AgentAuthDelivery
    var starts = 0
    var closed: [TerminalSessionID] = []
    private var signInHandlers: [@Sendable (AgentNotification) async -> Void] = []

    init(delivery: AgentAuthDelivery) { self.delivery = delivery }

    func subscribeToBackendEvents(
        filter: AgentEventFilter,
        handler: @escaping @Sendable (AgentNotification) async -> Void
    ) async -> AgentEventSubscriptionToken {
        if filter.methods?.contains("agent.signInEvent") == true {
            signInHandlers.append(handler)
        }
        return AgentEventSubscriptionToken()
    }
    func emit(phase: AgentAuthPhase) async throws {
        let event = AgentSignInEventParams(
            agentID: AgentID("terminal-agent"), authSessionID: AgentAuthSessionID("existing-login"),
            delivery: delivery, phase: phase, revealTerminal: true,
            terminal: AgentAuthTerminal(
                sessionID: TerminalSessionID("login-pty"), terminalID: TerminalID("login"),
                rows: 24, cols: 80
            ),
            error: phase == .failed ? "Login failed" : nil
        )
        let method = "agent.signInEvent"
        let payload = try JSONEncoder().encode(ProtocolNotification(method: method, params: event))
        for handler in signInHandlers {
            await handler(AgentNotification(method: method, payload: payload))
        }
        // Controller subscriptions enqueue handling on the main actor.
        await Task.yield()
    }
    func unsubscribeFromBackendEvents(_ token: AgentEventSubscriptionToken) async {}
    func signInToAgent(
        agentID: AgentID, workspaceID: WorkspaceID?, methodID: String?
    ) async throws -> AgentSignInResult {
        starts += 1
        return AgentSignInResult(
            agentID: agentID, authSessionID: AgentAuthSessionID("existing-login"),
            delivery: delivery, phase: .awaitingUser
        )
    }
    func cancelAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws {}
    func finishAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws {}
    func writeTerminal(sessionID: TerminalSessionID, data: Data) async throws {}
    func resizeTerminal(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) async throws {}
    func closeTerminal(sessionID: TerminalSessionID) async throws { closed.append(sessionID) }
    func replayTerminal(
        workspaceID: WorkspaceID?, terminalID: TerminalID, maxBytes: UInt64?
    ) async throws -> TerminalReplayResult { throw CancellationError() }
}
