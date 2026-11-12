import OttamBackendClient
import Foundation

/// What a sign-in needs from core, and nothing else.
///
/// A chat window signs in through its `DiffReviewModel`, which is already in
/// hand there; Settings has no workspace and no diff, so it signs in through the
/// process client directly. Both satisfy this, which is what lets one controller
/// and one terminal sheet serve both surfaces.
///
/// Mirrors `TerminalSurfaceBacking`: name the capability, not the owner.
@MainActor
protocol AgentSignInBackend: AnyObject {
    func subscribeToBackendEvents(
        filter: AgentEventFilter,
        handler: @escaping @Sendable (AgentNotification) async -> Void
    ) async -> AgentEventSubscriptionToken
    func unsubscribeFromBackendEvents(_ token: AgentEventSubscriptionToken) async

    func signInToAgent(
        agentID: AgentID,
        workspaceID: WorkspaceID?,
        methodID: String?
    ) async throws -> AgentSignInResult
    func cancelAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws
    func finishAgentSignIn(agentID: AgentID, authSessionID: AgentAuthSessionID) async throws

    func writeTerminal(sessionID: TerminalSessionID, data: Data) async throws
    func resizeTerminal(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) async throws
    func closeTerminal(sessionID: TerminalSessionID) async throws
    func replayTerminal(
        workspaceID: WorkspaceID?,
        terminalID: TerminalID,
        maxBytes: UInt64?
    ) async throws -> TerminalReplayResult
}

// The signatures already match; the conformance is the whole change.
extension DiffReviewModel: AgentSignInBackend {}

/// Settings talks to core with no workspace and no diff model in between. The
/// two renames are the only difference: `DiffReviewModel` disambiguates its
/// event methods from its own Git subscriptions, and the client does not need to.
extension OttamCoreProcessClient: AgentSignInBackend {
    func subscribeToBackendEvents(
        filter: AgentEventFilter,
        handler: @escaping @Sendable (AgentNotification) async -> Void
    ) async -> AgentEventSubscriptionToken {
        await subscribeToEvents(filter: filter, handler: handler)
    }

    func unsubscribeFromBackendEvents(_ token: AgentEventSubscriptionToken) async {
        await unsubscribeFromEvents(token)
    }
}
