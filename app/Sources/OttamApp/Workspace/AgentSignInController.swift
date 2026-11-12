import OttamBackendClient
import OttamPresentationLogic
import Foundation
import Observation
import OSLog

/// Drives `agent.signIn` for one workspace.
///
/// Core runs the login — a browser the adapter opens, or a terminal Ottam
/// owns — and reports progress as `agent.signInEvent`. Both shapes look the
/// same from here: press once, wait, and only show a terminal if core asks for
/// one. The reveal is not a mode the user chose, so it can arrive at any point
/// in a flow that started invisibly.
@MainActor
@Observable
final class AgentSignInController {
    private final class Owner {
        weak var controller: AgentSignInController?
        init(_ controller: AgentSignInController) { self.controller = controller }
    }

    private struct RetainedTerminalKey: Hashable {
        let backend: ObjectIdentifier
        let session: AgentAuthSessionID
    }

    private struct RetainedTerminal {
        let backend: any AgentSignInBackend
        let agentID: AgentID
        let terminal: AgentAuthTerminal
    }

    private static var owners: [Owner] = []
    private static var retainedTerminals: [RetainedTerminalKey: RetainedTerminal] = [:]

    init() {
        Self.owners.removeAll { $0.controller == nil }
        Self.owners.append(Owner(self))
    }

    deinit {
        Task { @MainActor in Self.closeUnownedFailedTerminals() }
    }

    /// Failure events reach each window separately. Keep the transcript while
    /// another controller is still processing that login or displaying it.
    private static func closeUnownedFailedTerminals() {
        owners.removeAll { $0.controller == nil }
        for (key, retained) in retainedTerminals {
            let hasOwner = owners.contains { owner in
                guard let controller = owner.controller,
                      let backend = controller.backend,
                      ObjectIdentifier(backend) == key.backend else { return false }
                if controller.revealed?.id == key.session { return true }
                guard let live = controller.live[retained.agentID] else { return false }
                return live.authSessionID == nil || live.authSessionID == key.session
            }
            guard !hasOwner else { continue }
            retainedTerminals[key] = nil
            Task {
                try? await retained.backend.closeTerminal(sessionID: retained.terminal.sessionID)
            }
        }
    }

    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "AgentSignIn"
    )

    /// A revealed login terminal, ready to be put on screen.
    struct RevealedTerminal: Identifiable {
        let id: AgentAuthSessionID
        let agentID: AgentID
        let engineName: String
        let terminal: AgentAuthTerminal
        let model: AuthTerminalSessionModel
        let surfaceController: TerminalSurfaceController
    }

    private struct LiveSignIn {
        var agentID: AgentID
        var engineName: String
        var authSessionID: AgentAuthSessionID?
        var phase: AgentAuthPhase
        /// Unknown until the sign-in result or first event names it.
        var delivery: AgentAuthDelivery?
        /// Keep broadcasts from other windows from undoing a local dismissal.
        /// An explicit sign-in/resume replaces this state and permits reveals again.
        var terminalDismissed = false
    }

    private let decoder = JSONDecoder()
    /// Where a login terminal's working directory comes from, when there is
    /// one. Settings has none, and core then runs the login in the agent's own
    /// home — so this gates nothing.
    private var workspaceID: WorkspaceID?
    private var backend: (any AgentSignInBackend)?
    private var subscription: AgentEventSubscriptionToken?
    private var subscriptionGeneration = 0
    private var live: [AgentID: LiveSignIn] = [:]

    /// The terminal core asked to have shown, if any. At most one at a time —
    /// two logins racing for the same sheet is not a state worth modelling.
    private(set) var revealed: RevealedTerminal?
    /// Why the last sign-in failed, keyed by agent, cleared when one starts.
    private(set) var failures: [AgentID: String] = [:]
    /// Agents signed in from this window, so the card that asked can say so.
    /// Deliberately not derived from the engine's auth state: "signed in just
    /// now" is a different claim from "has working credentials", and only the
    /// first earns a green card on a run the user is still looking at.
    private(set) var succeeded: Set<AgentID> = []

    /// Called after a login succeeds, so the caller can re-read the engine list
    /// — the auth state it is showing is now out of date.
    var onSignedIn: ((AgentID) -> Void)?

    func isSigningIn(_ agentID: AgentID) -> Bool {
        live[agentID] != nil
    }

    /// A dismissed terminal can be recovered by joining the same core login.
    func canStartOrResume(_ agentID: AgentID) -> Bool {
        guard let current = live[agentID] else { return true }
        return current.delivery == .clientTerminal && revealed?.agentID != agentID
    }

    /// The login runs in a terminal Ottam shows, as opposed to a browser
    /// the adapter opened — the waiting card words itself accordingly.
    func isTerminalLogin(_ agentID: AgentID) -> Bool {
        live[agentID]?.delivery == .clientTerminal
    }

    func failure(for agentID: AgentID) -> String? {
        failures[agentID]
    }

    func hasJustSignedIn(_ agentID: AgentID) -> Bool {
        succeeded.contains(agentID)
    }

    /// Drop the remembered success — the caller has newer evidence, or has
    /// moved somewhere the success is no longer the thing being reported.
    func forgetSuccess(_ agentID: AgentID) {
        succeeded.remove(agentID)
    }

    func forgetSuccesses() {
        succeeded.removeAll()
    }

    func configure(workspaceID: WorkspaceID?, backend: any AgentSignInBackend) async {
        if self.workspaceID != workspaceID {
            reset()
        }
        self.workspaceID = workspaceID
        self.backend = backend
        guard subscription == nil else { return }
        let generation = subscriptionGeneration
        let token = await backend.subscribeToBackendEvents(
            filter: AgentEventFilter(methods: ["agent.signInEvent"])
        ) { [weak self] notification in
            Task { @MainActor in
                self?.handle(notification)
            }
        }
        // Detached, or superseded by a newer configure, while subscribing.
        guard generation == subscriptionGeneration, subscription == nil else {
            await backend.unsubscribeFromBackendEvents(token)
            return
        }
        subscription = token
    }

    /// Stops listening for sign-in events; a later `configure` listens again.
    /// Synchronous so a view that disappears and reappears in one pass ends up
    /// subscribed. The login itself runs on in core.
    func detach() {
        subscriptionGeneration &+= 1
        guard let subscription, let backend else { return }
        self.subscription = nil
        Task { await backend.unsubscribeFromBackendEvents(subscription) }
    }

    func signIn(engine: EngineSummary) {
        signIn(agentID: AgentID(engine.engineID.rawValue), displayName: engine.displayName)
    }

    /// Settings lists agents, not engines, and has only an `AgentSummary`.
    /// A nil `methodID` lets core choose the sign-in.
    func signIn(agentID: AgentID, displayName: String, methodID: String? = nil) {
        guard let backend else { return }
        guard canStartOrResume(agentID) else { return }
        failures[agentID] = nil
        succeeded.remove(agentID)
        live[agentID] = LiveSignIn(
            agentID: agentID,
            engineName: displayName,
            authSessionID: nil,
            phase: .starting
        )
        let workspaceID = self.workspaceID
        Task { [weak self] in
            do {
                let started = try await backend.signInToAgent(
                    agentID: agentID,
                    workspaceID: workspaceID,
                    methodID: methodID
                )
                guard let self else { return }
                // Events for this flow can land before the response does, so
                // never downgrade a phase that has already moved on.
                if var current = self.live[agentID], current.authSessionID == nil {
                    current.authSessionID = started.authSessionID
                    current.delivery = started.delivery
                    self.live[agentID] = current
                    Self.closeUnownedFailedTerminals()
                }
            } catch {
                guard let self else { return }
                self.live[agentID] = nil
                Self.closeUnownedFailedTerminals()
                self.failures[agentID] = Self.message(for: error)
                Self.logger.error(
                    "sign-in failed to start: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
    }

    /// Abandon the flow and kill anything it started. Dismissing a revealed
    /// terminal is not this: a browser login the user is midway through
    /// finishing should survive the sheet closing.
    func cancel(_ agentID: AgentID) {
        guard let backend, let session = live[agentID]?.authSessionID else {
            live[agentID] = nil
            Self.closeUnownedFailedTerminals()
            return
        }
        Task {
            try? await backend.cancelAgentSignIn(agentID: agentID, authSessionID: session)
        }
    }

    /// The user says the login is complete. Core closes the login terminal,
    /// restarts the adapter, and reports back through `agent.signInEvent` —
    /// a stale or already-finished session is a silent no-op there.
    func finish(_ agentID: AgentID) {
        guard let backend, let session = live[agentID]?.authSessionID else { return }
        Task {
            try? await backend.finishAgentSignIn(agentID: agentID, authSessionID: session)
        }
    }

    /// Close the sheet without touching the login behind it.
    func dismissRevealedTerminal() {
        guard let revealed else { return }
        if live[revealed.agentID]?.authSessionID == revealed.id {
            live[revealed.agentID]?.terminalDismissed = true
        }
        revealed.surfaceController.destroy()
        revealed.model.detach()
        self.revealed = nil
        Self.closeUnownedFailedTerminals()
    }

    private func reset() {
        dismissRevealedTerminal()
        live.removeAll()
        Self.closeUnownedFailedTerminals()
        failures.removeAll()
        succeeded.removeAll()
    }

    private func handle(_ notification: AgentNotification) {
        let event: AgentSignInEventParams
        do {
            event = try decoder.decode(
                ProtocolNotification<AgentSignInEventParams>.self,
                from: notification.payload
            ).params
        } catch {
            Self.logger.error(
                "failed to decode sign-in event: \(error.localizedDescription, privacy: .private)"
            )
            return
        }
        let agentID = event.agentID
        // Notifications are process-wide. A controller only owns flows that
        // were initiated from its workspace window; otherwise one sign-in
        // would disable controls or reveal a terminal in every open workspace.
        guard var current = live[agentID] else { return }
        // Also reject a late terminal event from a prior attempt after a new
        // session has already been accepted locally.
        if let currentSession = current.authSessionID,
           currentSession != event.authSessionID {
            return
        }
        current.authSessionID = event.authSessionID
        current.phase = event.phase
        current.delivery = event.delivery

        if event.revealTerminal, !current.terminalDismissed, let terminal = event.terminal {
            reveal(
                terminal,
                event: event,
                engineName: current.engineName
            )
        }

        switch event.phase {
        case .starting, .awaitingUser, .needsInput:
            live[agentID] = current
        case .succeeded:
            live[agentID] = nil
            failures[agentID] = nil
            succeeded.insert(agentID)
            // The terminal, if one was ever shown, has served its purpose; the
            // sheet closing is the confirmation.
            dismissRevealedTerminal(for: event.authSessionID)
            onSignedIn?(agentID)
        case .failed:
            live[agentID] = nil
            failures[agentID] = event.error ?? "Sign-in did not complete."
            if event.revealTerminal, let terminal = event.terminal, let backend {
                Self.retainedTerminals[RetainedTerminalKey(
                    backend: ObjectIdentifier(backend), session: event.authSessionID
                )] = RetainedTerminal(backend: backend, agentID: agentID, terminal: terminal)
            }
        case .cancelled:
            live[agentID] = nil
            failures[agentID] = nil
            dismissRevealedTerminal(for: event.authSessionID)
        }
        Self.closeUnownedFailedTerminals()
    }

    private func reveal(
        _ terminal: AgentAuthTerminal,
        event: AgentSignInEventParams,
        engineName: String
    ) {
        if revealed?.id == event.authSessionID {
            // Preserve the existing surface when a live login becomes a
            // retained failure transcript. Shared ownership handles cleanup.
            return
        }
        guard let backend else { return }
        dismissRevealedTerminal()
        let model = AuthTerminalSessionModel(terminal: terminal, backend: backend)
        let controller = TerminalSurfaceController(
            model: model,
            workingDirectory: NSHomeDirectory()
        )
        revealed = RevealedTerminal(
            id: event.authSessionID,
            agentID: event.agentID,
            engineName: engineName,
            terminal: terminal,
            model: model,
            surfaceController: controller
        )
        model.start()
    }

    private func dismissRevealedTerminal(for authSessionID: AgentAuthSessionID) {
        guard revealed?.id == authSessionID else { return }
        dismissRevealedTerminal()
    }

    private static func message(for error: Error) -> String {
        if case .rpcError(_, let message, _) = error as? AgentConnectionError {
            return message
        }
        return error.localizedDescription
    }
}

/// The terminal behind a revealed sign-in.
///
/// The PTY is already running by the time anyone looks at it — core started it
/// hidden and the reveal only decides whether it is on screen — so this attaches
/// to a live session rather than opening one: `terminal.replay` for what has
/// already happened, live deltas for the rest.
@MainActor
@Observable
final class AuthTerminalSessionModel: TerminalSurfaceBacking {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "AgentSignIn"
    )

    private let terminal: AgentAuthTerminal
    private let backend: any AgentSignInBackend
    private let decoder = JSONDecoder()
    private var subscription: AgentEventSubscriptionToken?
    private var outputSink: ((Data) -> Void)?
    private var outputSinkToken: TerminalOutputSinkToken?
    private var nextOutputSinkToken: UInt64 = 0
    private var scrollback: [Data] = []
    /// Deltas that arrive while the replay is still in flight. Applying them
    /// first would put live output above the history it follows.
    private var bufferedDeltas: [TerminalOutputDeltaParams] = []
    private var isReplaying = true
    private var lastSequence: UInt64 = 0
    private var rows: UInt16
    private var cols: UInt16

    private(set) var hasExited = false

    init(terminal: AgentAuthTerminal, backend: any AgentSignInBackend) {
        self.terminal = terminal
        self.backend = backend
        self.rows = terminal.rows
        self.cols = terminal.cols
    }

    func start() {
        Task { [weak self] in
            guard let self else { return }
            // Subscribe before replaying: a delta that lands in between is
            // buffered, where one that lands before the subscription is lost.
            self.subscription = await self.backend.subscribeToBackendEvents(
                filter: AgentEventFilter(
                    methods: ["terminal.outputDelta", "terminal.exited"],
                    terminalSessionID: self.terminal.sessionID
                )
            ) { [weak self] notification in
                Task { @MainActor in
                    self?.handle(notification)
                }
            }
            await self.replay()
        }
    }

    func detach() {
        guard let token = subscription else { return }
        subscription = nil
        let backend = self.backend
        Task {
            await backend.unsubscribeFromBackendEvents(token)
        }
    }

    // MARK: - TerminalSurfaceBacking

    func attachOutputSink(_ sink: @escaping (Data) -> Void) -> TerminalOutputSinkToken {
        nextOutputSinkToken &+= 1
        let token = TerminalOutputSinkToken(rawValue: nextOutputSinkToken)
        outputSinkToken = token
        outputSink = sink
        for chunk in scrollback {
            sink(chunk)
        }
        return token
    }

    func detachOutputSink(token: TerminalOutputSinkToken) {
        guard outputSinkToken == token else { return }
        outputSinkToken = nil
        outputSink = nil
    }

    func writeInput(_ data: Data) {
        guard !data.isEmpty, !hasExited else { return }
        let sessionID = terminal.sessionID
        let backend = self.backend
        Task {
            do {
                try await backend.writeTerminal(sessionID: sessionID, data: data)
            } catch {
                Self.logger.debug(
                    "sign-in terminal write failed: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
    }

    func resize(rows: UInt16, cols: UInt16) {
        let rows = max(rows, 1)
        let cols = max(cols, 1)
        guard rows != self.rows || cols != self.cols, !hasExited else { return }
        self.rows = rows
        self.cols = cols
        let sessionID = terminal.sessionID
        let backend = self.backend
        Task {
            do {
                try await backend.resizeTerminal(sessionID: sessionID, rows: rows, cols: cols)
            } catch {
                Self.logger.debug(
                    "sign-in terminal resize failed: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
    }

    // MARK: - Output

    private func replay() async {
        defer {
            isReplaying = false
            let buffered = bufferedDeltas.sorted { $0.sequence < $1.sequence }
            bufferedDeltas.removeAll(keepingCapacity: false)
            for delta in buffered {
                apply(delta)
            }
        }
        do {
            // A login terminal belongs to no workspace; its id addresses it.
            let replay = try await backend.replayTerminal(
                workspaceID: nil,
                terminalID: terminal.terminalID,
                maxBytes: nil
            )
            for chunk in replay.chunks.sorted(by: { $0.sequenceStart < $1.sequenceStart }) {
                guard let data = Data(base64Encoded: chunk.dataBase64), !data.isEmpty else {
                    continue
                }
                append(data)
                lastSequence = max(lastSequence, chunk.sequenceEnd)
            }
        } catch {
            Self.logger.debug(
                "sign-in terminal replay failed: \(error.localizedDescription, privacy: .private)"
            )
        }
    }

    private func handle(_ notification: AgentNotification) {
        switch notification.method {
        case "terminal.outputDelta":
            guard let typed = try? decoder.decode(
                ProtocolNotification<TerminalOutputDeltaParams>.self,
                from: notification.payload
            ) else { return }
            guard typed.params.sessionID == terminal.sessionID else { return }
            if isReplaying {
                bufferedDeltas.append(typed.params)
            } else {
                apply(typed.params)
            }
        case "terminal.exited":
            guard let typed = try? decoder.decode(
                ProtocolNotification<TerminalExitedParams>.self,
                from: notification.payload
            ) else { return }
            guard typed.params.sessionID == terminal.sessionID else { return }
            hasExited = true
        default:
            break
        }
    }

    private func apply(_ delta: TerminalOutputDeltaParams) {
        guard delta.sequence > lastSequence else { return }
        lastSequence = delta.sequence
        guard let data = Data(base64Encoded: delta.dataBase64), !data.isEmpty else { return }
        append(data)
    }

    private func append(_ data: Data) {
        // A login writes a URL and a prompt or two — small enough that the cap
        // exists to bound a runaway, not to manage a real scrollback.
        scrollback.append(data)
        if scrollback.count > 512 {
            scrollback.removeFirst(scrollback.count - 512)
        }
        outputSink?(data)
    }
}
