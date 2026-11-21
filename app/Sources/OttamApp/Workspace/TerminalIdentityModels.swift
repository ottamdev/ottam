import OttamBackendClient
import OttamWorkspaceLogic
import Foundation
import Observation
import OSLog

extension TerminalID: Identifiable {
    public var id: String { rawValue }

    init() {
        self.init(UUID().uuidString.lowercased())
    }
}

extension WorkingRootSummary {
    var selection: WorkingRootSelection {
        switch kind {
        case .workspaceFiles:
            return .workspaceFiles
        case .project:
            return .project(
                projectID: projectID ?? ProjectID(""),
                sourceID: sourceID ?? ProjectSourceID("")
            )
        }
    }

    func updatingAvailability(for projects: [WorkspaceProjectSummary]) -> WorkingRootSummary {
        var updated = self
        switch kind {
        case .workspaceFiles:
            updated.availability = .available
        case .project:
            let projectIsMember = projects.contains { $0.project.projectID == projectID }
            if worktree != nil {
                // A worktree is never a member's source. Core's answer stands
                // while its project stays: it knows whether the checkout is gone.
                if !projectIsMember { updated.availability = .detached }
            } else {
                let exactSourceIsAttached = projects.contains { member in
                    member.project.projectID == projectID
                        && member.source.projectSourceID == sourceID
                }
                updated.availability = exactSourceIsAttached ? .available : .detached
            }
        }
        return updated
    }

    /// The project source Review shows for this root: the member's own, or
    /// the conversation's worktree under its member project.
    func reviewProject(in projects: [WorkspaceProjectSummary]) -> ResolvedProject? {
        guard let sourceID else { return nil }
        guard let worktree else {
            return projects.first { $0.source.projectSourceID == sourceID }?.resolvedProject
        }
        guard updatingAvailability(for: projects).availability == .available,
              let member = projects.first(where: { $0.project.projectID == projectID })
        else { return nil }
        return ResolvedProject(
            project: member.project,
            source: ProjectSourceSummary(
                projectSourceID: sourceID,
                projectID: member.project.projectID,
                kind: .localGit,
                displayName: worktree.branch,
                canonicalPath: canonicalPath,
                createdAt: 0,
                updatedAt: 0,
                currentBranch: worktree.branch
            )
        )
    }
}

/// A terminal tab's terminal. `id` names the tab; Core assigns its own
/// `TerminalID` when the terminal opens.
struct TerminalRecord: Identifiable, Equatable {
    var id = TerminalID()
    var workspaceID: WorkspaceID
    var workingRoot: WorkingRootSummary
    var title: String
    var cwd: String
}

/// A window's terminals, each a tab in one conversation's tools pane. Core
/// holds a terminal only for its own lifetime, so nothing here restores: the
/// window closes its terminals as it closes, and a replaced Core starts with
/// none. See `docs/architecture/terminal/README.md`.
@MainActor
@Observable
final class TerminalWorkspaceModel {
    private(set) var workspaceID: WorkspaceID?
    private(set) var workspaceFiles: WorkingRootSummary?
    private(set) var projects: [WorkspaceProjectSummary] = []
    private(set) var records: [TerminalRecord] = []

    @ObservationIgnored
    private var sessionModels: [TerminalID: TerminalSessionModel] = [:]
    @ObservationIgnored
    private var surfaceControllers: [TerminalID: TerminalSurfaceController] = [:]
    /// One subscription carries every terminal's output in the window, decoded
    /// once and routed by session. A per-terminal subscription decoded every
    /// terminal's output in each of them.
    @ObservationIgnored
    private var eventSubscription: AgentEventSubscriptionToken?
    @ObservationIgnored
    private var eventSubscriptionTask: Task<Void, Never>?
    @ObservationIgnored
    private var eventSubscriptionWorkspaceID: WorkspaceID?
    @ObservationIgnored
    private var eventSubscriptionGeneration = 0
    @ObservationIgnored
    private let decoder = JSONDecoder()
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "Terminal"
    )

    func configure(
        workspaceID: WorkspaceID,
        workspaceFiles: WorkingRootSummary?,
        projects: [WorkspaceProjectSummary],
        backend: DiffReviewModel
    ) {
        if self.workspaceID != workspaceID {
            closeAll(backend: backend)
        }
        self.workspaceID = workspaceID
        self.workspaceFiles = workspaceFiles
        self.projects = projects
        records = records.map { record in
            var updated = record
            updated.workingRoot = record.workingRoot.updatingAvailability(for: projects)
            // Core closes a removed worktree's terminals, so a worktree
            // terminal still here was detached only by its project leaving.
            if updated.workingRoot.worktree != nil,
               projects.contains(where: { $0.project.projectID == updated.workingRoot.projectID }) {
                updated.workingRoot.availability = .available
            }
            return updated
        }
        for record in records {
            sessionModels[record.id]?.configure(
                workspaceID: record.workspaceID,
                workingRoot: record.workingRoot,
                backend: backend
            )
        }
    }

    /// Whether `selection` is still in the workspace to start a terminal in.
    func canCreateTerminal(at selection: WorkingRootSelection) -> Bool {
        workspaceID != nil && rootSummary(for: selection) != nil
    }

    /// Whether a conversation's own root is still there to start a terminal in.
    func canCreateTerminal(in root: WorkingRootSummary) -> Bool {
        workspaceID != nil && attachedRoot(root) != nil
    }

    /// A terminal at `selection`. Core opens it when its tab first shows.
    func createTerminal(at selection: WorkingRootSelection, backend: DiffReviewModel) -> TerminalRecord? {
        rootSummary(for: selection).flatMap { createTerminal(with: $0, backend: backend) }
    }

    /// A terminal in a conversation's own root, which may be its worktree.
    func createTerminal(in root: WorkingRootSummary, backend: DiffReviewModel) -> TerminalRecord? {
        attachedRoot(root).flatMap { createTerminal(with: $0, backend: backend) }
    }

    private func createTerminal(with workingRoot: WorkingRootSummary, backend: DiffReviewModel) -> TerminalRecord? {
        guard let workspaceID else { return nil }
        let record = TerminalRecord(
            workspaceID: workspaceID,
            workingRoot: workingRoot,
            title: "Terminal",
            cwd: workingRoot.canonicalPath
        )
        let model = TerminalSessionModel(events: self)
        model.configure(workspaceID: workspaceID, workingRoot: workingRoot, backend: backend)
        records.append(record)
        sessionModels[record.id] = model
        return record
    }

    func record(for terminalID: TerminalID) -> TerminalRecord? {
        records.first { $0.id == terminalID }
    }

    func sessionModel(for terminalID: TerminalID) -> TerminalSessionModel? {
        sessionModels[terminalID]
    }

    func surfaceController(for record: TerminalRecord) -> TerminalSurfaceController? {
        if let controller = surfaceControllers[record.id] {
            return controller
        }
        guard let model = sessionModels[record.id] else {
            return nil
        }
        let controller = TerminalSurfaceController(
            model: model,
            workingDirectory: record.cwd
        )
        surfaceControllers[record.id] = controller
        return controller
    }

    /// Closes the terminal whose tab closed.
    func closeTerminal(_ terminalID: TerminalID, backend: DiffReviewModel) {
        guard let index = records.firstIndex(where: { $0.id == terminalID }) else {
            return
        }
        end(records.remove(at: index), backend: backend)
    }

    /// Closes every terminal: the window is closing, or a replaced Core holds
    /// none of them.
    func closeAll(backend: DiffReviewModel) {
        let ended = records
        records.removeAll()
        for record in ended {
            end(record, backend: backend)
        }
        releaseEventSubscription(backend: backend)
    }

    /// Subscribes once for the window's workspace; a terminal awaits this before
    /// it opens so none of its output is missed.
    func subscribeIfNeeded(backend: DiffReviewModel) async {
        guard let workspaceID else { return }
        if eventSubscriptionWorkspaceID == workspaceID, let pending = eventSubscriptionTask {
            await pending.value
            return
        }
        releaseEventSubscription(backend: backend)
        let generation = eventSubscriptionGeneration
        let task = Task { [weak self, weak backend] in
            guard let backend else { return }
            let token = await backend.subscribeToBackendEvents(
                filter: AgentEventFilter(
                    methods: ["terminal.outputDelta", "terminal.exited"],
                    workspaceID: workspaceID
                )
            ) { [weak self] notification in
                // Awaited, so output stays in order and core's delivery waits
                // for the main actor instead of queueing unbounded tasks.
                guard let self else { return }
                await self.route(notification)
            }
            guard let self, self.eventSubscriptionGeneration == generation else {
                await backend.unsubscribeFromBackendEvents(token)
                return
            }
            self.eventSubscription = token
        }
        eventSubscriptionWorkspaceID = workspaceID
        eventSubscriptionTask = task
        await task.value
    }

    private func releaseEventSubscription(backend: DiffReviewModel) {
        eventSubscriptionGeneration &+= 1
        eventSubscriptionTask = nil
        eventSubscriptionWorkspaceID = nil
        guard let token = eventSubscription else { return }
        eventSubscription = nil
        Task { await backend.unsubscribeFromBackendEvents(token) }
    }

    private func route(_ notification: AgentNotification) {
        switch notification.method {
        case "terminal.outputDelta":
            do {
                let delta = try decoder.decode(
                    ProtocolNotification<TerminalOutputDeltaParams>.self,
                    from: notification.payload
                ).params
                for model in recipients(of: delta.sessionID) { model.receiveOutputDelta(delta) }
            } catch {
                Self.logger.error("failed to decode terminal output notification: \(error.localizedDescription, privacy: .private)")
            }
        case "terminal.exited":
            do {
                let exited = try decoder.decode(
                    ProtocolNotification<TerminalExitedParams>.self,
                    from: notification.payload
                ).params
                for model in recipients(of: exited.sessionID) { model.receiveExited(exited) }
            } catch {
                Self.logger.error("failed to decode terminal exit notification: \(error.localizedDescription, privacy: .private)")
            }
        default:
            break
        }
    }

    /// The terminal that owns `sessionID`, or, before any does, every terminal
    /// still opening: output can arrive ahead of the open's response, and the
    /// one whose open returns that session keeps it.
    private func recipients(of sessionID: TerminalSessionID) -> [TerminalSessionModel] {
        let models = Array(sessionModels.values)
        if let owner = models.first(where: { $0.sessionSummary?.sessionID == sessionID }) {
            return [owner]
        }
        return models.filter(\.isOpening)
    }

    /// Closes the terminal's session and deletes it from Core. A terminal
    /// still opening deletes itself when the open lands.
    private func end(_ record: TerminalRecord, backend: DiffReviewModel) {
        let model = sessionModels.removeValue(forKey: record.id)
        let terminalID = model?.coreTerminalID
        surfaceControllers.removeValue(forKey: record.id)?.destroy()
        Task {
            await model?.closeSession()
            if let terminalID {
                try? await backend.deleteTerminal(
                    workspaceID: record.workspaceID,
                    terminalID: terminalID
                )
            }
        }
    }

    /// A worktree is no member's source, so it stands on its project and on
    /// Core's availability; any other root resolves through the members.
    private func attachedRoot(_ root: WorkingRootSummary) -> WorkingRootSummary? {
        guard root.worktree != nil else { return rootSummary(for: root.selection) }
        let attached = root.updatingAvailability(for: projects)
        return attached.availability == .available ? attached : nil
    }

    private func rootSummary(for selection: WorkingRootSelection) -> WorkingRootSummary? {
        switch selection {
        case .workspaceFiles:
            return workspaceFiles
        case .project(let projectID, let sourceID):
            guard let member = projects.first(where: {
                $0.project.projectID == projectID
                    && $0.source.projectSourceID == sourceID
            }) else { return nil }
            return WorkingRootSummary(
                kind: .project,
                displayName: member.project.displayName,
                canonicalPath: member.source.canonicalPath,
                projectID: member.project.projectID,
                sourceID: member.source.projectSourceID,
                availability: .available
            )
        }
    }
}
