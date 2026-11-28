import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// A worktree is a project source no Workspace member carries, so a
/// conversation's root there must not read as detached just for that.
@MainActor
@Suite("Worktree roots")
struct WorktreeRootTests {
    private let member = WorkspaceProjectSummary(
        project: ProjectSummary(
            projectID: ProjectID("project-alpha"),
            displayName: "alpha",
            createdAt: 0,
            updatedAt: 0,
            lastOpenedAt: 0
        ),
        source: ProjectSourceSummary(
            projectSourceID: ProjectSourceID("source-alpha"),
            projectID: ProjectID("project-alpha"),
            kind: .localGit,
            displayName: "alpha",
            canonicalPath: "/tmp/alpha",
            createdAt: 0,
            updatedAt: 0
        ),
        sortOrder: 0,
        addedAt: 0
    )

    private func worktreeRoot(_ availability: WorkingRootAvailability = .available) -> WorkingRootSummary {
        WorkingRootSummary(
            kind: .project,
            displayName: "alpha",
            canonicalPath: "/tmp/worktrees/alpha/fix-3f2a",
            projectID: ProjectID("project-alpha"),
            sourceID: ProjectSourceID("source-worktree"),
            availability: availability,
            worktree: WorktreeSummary(branch: "ottam/fix-3f2a", baseCommit: "abc123")
        )
    }

    @Test("a worktree keeps Core's availability while its project is a member, and detaches when it leaves")
    func availability() {
        #expect(worktreeRoot().updatingAvailability(for: [member]).availability == .available)
        #expect(worktreeRoot(.detached).updatingAvailability(for: [member]).availability == .detached)
        #expect(worktreeRoot().updatingAvailability(for: []).availability == .detached)
    }

    @Test("Review shows the worktree's own source under its project, and nothing once it is gone")
    func reviewProject() throws {
        let project = try #require(worktreeRoot().reviewProject(in: [member]))
        #expect(project.projectSourceID == ProjectSourceID("source-worktree"))
        #expect(project.canonicalRoot == "/tmp/worktrees/alpha/fix-3f2a")
        #expect(project.displayName == "alpha")
        #expect(project.currentBranch == "ottam/fix-3f2a")
        #expect(worktreeRoot(.detached).reviewProject(in: [member]) == nil)
        #expect(worktreeRoot().reviewProject(in: []) == nil)
    }

    @Test("a terminal opens in the worktree, not the project's checkout")
    func terminalRoot() {
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        let terminals = TerminalWorkspaceModel()
        terminals.configure(
            workspaceID: WorkspaceID("worktrees"),
            workspaceFiles: nil,
            projects: [member],
            backend: backend
        )
        #expect(terminals.canCreateTerminal(in: worktreeRoot()))
        #expect(!terminals.canCreateTerminal(in: worktreeRoot(.detached)))
        let record = terminals.createTerminal(in: worktreeRoot(), backend: backend)
        #expect(record?.cwd == "/tmp/worktrees/alpha/fix-3f2a")
        #expect(record?.workingRoot.sourceID == ProjectSourceID("source-worktree"))
        terminals.closeAll(backend: backend)
    }

    @Test("a worktree terminal detached by its project leaving comes back only on Core's word")
    func worktreeTerminalReattachesWithItsProject() throws {
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        let terminals = TerminalWorkspaceModel()
        let workspaceID = WorkspaceID("worktrees")
        terminals.configure(workspaceID: workspaceID, workspaceFiles: nil, projects: [member], backend: backend)
        let record = terminals.createTerminal(in: worktreeRoot(), backend: backend)
        let id = try #require(record?.id)
        terminals.configure(workspaceID: workspaceID, workspaceFiles: nil, projects: [], backend: backend)
        #expect(terminals.record(for: id)?.workingRoot.availability == .detached)
        // Membership alone does not bring it back: Core may have removed it.
        terminals.configure(workspaceID: workspaceID, workspaceFiles: nil, projects: [member], backend: backend)
        #expect(terminals.record(for: id)?.workingRoot.availability == .detached)
        terminals.adoptWorktreeRoots([worktreeRoot(.detached)], backend: backend)
        #expect(terminals.record(for: id)?.workingRoot.availability == .detached)
        terminals.adoptWorktreeRoots([worktreeRoot()], backend: backend)
        #expect(terminals.record(for: id)?.workingRoot.availability == .available)
        terminals.closeAll(backend: backend)
    }

    @Test("a rejoined project's worktree roots wait for a re-list across reconfiguration")
    func rejoinedWorktreeAwaitsRelist() {
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        let model = ChatWorkspaceModel(defaults: UserDefaults(suiteName: "worktree-relist-\(UUID())")!)
        let workspaceID = WorkspaceID("worktrees")
        let configure = { (projects: [WorkspaceProjectSummary]) in
            model.configure(
                workspaceID: workspaceID,
                workspaceFiles: nil,
                projects: projects,
                projectSummary: nil,
                backend: backend
            )
        }
        configure([member])
        model.apply(conversations: [
            ConversationSummary(
                conversationID: ConversationID("worktree-chat"),
                workspaceID: workspaceID,
                workingRoot: worktreeRoot(),
                title: "Fix",
                createdAt: "2026-11-20T00:00:00Z",
                updatedAt: "2026-11-20T00:00:00Z",
                alwaysAllowToolActions: false
            )
        ])
        configure([])
        #expect(!model.worktreeRootsNeedRelist)
        configure([member])
        #expect(model.worktreeRootsNeedRelist)
        // A second configure before the re-list lands keeps it owed; this
        // Core never answers, so nothing clears it.
        configure([member])
        #expect(model.worktreeRootsNeedRelist)
        #expect(model.conversations.first?.workingRoot.availability == .detached)
    }

    @Test("a send error under the composer clears once the draft's root or Worktree checkbox changes")
    func draftChangeClearsSendError() async throws {
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        var beta = member
        beta.project.projectID = ProjectID("project-beta")
        beta.source.projectSourceID = ProjectSourceID("source-beta")
        beta.source.projectID = beta.project.projectID
        let model = ChatWorkspaceModel(defaults: UserDefaults(suiteName: "worktree-draft-\(UUID())")!)
        model.configure(
            workspaceID: WorkspaceID("worktrees"),
            workspaceFiles: WorkingRootSummary(
                kind: .workspaceFiles,
                displayName: "Files",
                canonicalPath: "/tmp",
                projectID: nil,
                sourceID: nil,
                availability: .available
            ),
            projects: [member, beta],
            projectSummary: nil,
            backend: backend
        )
        let alpha = WorkingRootSelection.project(
            projectID: ProjectID("project-alpha"),
            sourceID: ProjectSourceID("source-alpha")
        )
        model.selectDraftWorkingRoot(alpha)
        #expect(model.draftWorkingRoot == alpha)

        func failToSend() async throws {
            _ = await model.submit("add a line", backend: backend)
            try #require(model.errorText != nil)
        }

        // Let configure's own load against the missing core finish first.
        let deadline = ContinuousClock.now + .seconds(3)
        while model.errorText == nil, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(10))
        }
        try? await Task.sleep(for: .milliseconds(100))

        try await failToSend()
        model.selectDraftWorkingRoot(alpha)
        model.draftCreatesWorktree = false
        #expect(model.errorText != nil, "nothing changed, so the error still applies")
        model.selectDraftWorkingRoot(.workspaceFiles)
        #expect(model.errorText == nil)

        model.selectDraftWorkingRoot(alpha)
        try await failToSend()
        model.draftCreatesWorktree = true
        #expect(model.errorText == nil)
    }

    @Test("only Current/Default preference persists; Worktree toggle and one-off branches reset on new chat")
    func draftWorktreePreferencesPersist() {
        let suite = "worktree-defaults-\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        let alphaRoot = WorkingRootSelection.project(
            projectID: member.project.projectID,
            sourceID: member.source.projectSourceID
        )

        func makeConfiguredModel() -> ChatWorkspaceModel {
            let model = ChatWorkspaceModel(defaults: defaults)
            model.configure(
                workspaceID: WorkspaceID("worktrees"),
                workspaceFiles: nil,
                projects: [member],
                projectSummary: nil,
                backend: backend
            )
            return model
        }

        let first = makeConfiguredModel()
        #expect(!first.draftCreatesWorktree)
        #expect(first.draftWorktreeBaseSelection == .current)

        first.selectDraftWorkingRoot(alphaRoot)
        first.draftCreatesWorktree = true
        first.selectDraftWorktreeBase(.defaultBranch)

        // Picking a one-off branch applies only to the current draft without
        // overwriting the saved `.defaultBranch` preference.
        first.selectDraftWorktreeBase(.branch("refs/heads/feat/temp"))
        #expect(first.draftWorktreeBaseSelection == .branch("refs/heads/feat/temp"))

        // Starting a new chat resets Worktree to unticked and restores the saved
        // Current/Default preference (not the one-off branch).
        first.beginNewConversation(backend: backend)
        #expect(!first.draftCreatesWorktree)
        #expect(first.draftWorktreeBaseSelection == .defaultBranch)

        // A fresh model on the same UserDefaults starts with Worktree unticked
        // and restores only the `.defaultBranch` preference.
        first.draftCreatesWorktree = true
        let restored = makeConfiguredModel()
        #expect(!restored.draftCreatesWorktree)
        #expect(restored.draftWorktreeBaseSelection == .defaultBranch)

        // Switching back to Current persists `.current`.
        restored.selectDraftWorktreeBase(.current)
        let backToCurrent = makeConfiguredModel()
        #expect(!backToCurrent.draftCreatesWorktree)
        #expect(backToCurrent.draftWorktreeBaseSelection == .current)
    }

    @Test("loadDraftWorktreeBranches handles errors without locking the trigger and ignores stale responses")
    func draftWorktreeBranchLoadErrorsAndOutOfOrderResponses() async {
        enum FakeError: LocalizedError {
            case failed
            var errorDescription: String? { "git unavailable" }
        }

        let suite = "worktree-load-\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        let model = ChatWorkspaceModel(defaults: defaults)
        model.configure(
            workspaceID: WorkspaceID("worktrees"),
            workspaceFiles: nil,
            projects: [member],
            projectSummary: nil,
            backend: backend
        )
        model.draftCreatesWorktree = true

        // 1. A failed initial fetch records loadError and keeps the trigger clickable for Retry.
        await model.loadDraftWorktreeBranches { _, _ in
            throw FakeError.failed
        }
        #expect(model.draftWorktreeBranches == nil)
        #expect(model.draftWorktreeBranchesError == "git unavailable")
        #expect(!model.draftWorktreeBaseBranchMenu.isTriggerDisabled)

        // 2. An older slow fetch finishing after a newer fetch does not overwrite the newer result.
        let olderResult = WorktreeBaseBranchesResult(
            currentBranch: "old-branch",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [
                WorktreeBaseBranch(name: "old-branch", refName: "refs/heads/old-branch", kind: .local),
                WorktreeBaseBranch(name: "main", refName: "refs/heads/main", kind: .local),
            ]
        )
        let newerResult = WorktreeBaseBranchesResult(
            currentBranch: "new-branch",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [
                WorktreeBaseBranch(name: "new-branch", refName: "refs/heads/new-branch", kind: .local),
                WorktreeBaseBranch(name: "main", refName: "refs/heads/main", kind: .local),
            ]
        )

        var releaseOlder: CheckedContinuation<WorktreeBaseBranchesResult, Error>?
        let olderTask = Task { @MainActor in
            await model.loadDraftWorktreeBranches { _, _ in
                try await withCheckedThrowingContinuation { continuation in
                    releaseOlder = continuation
                }
            }
        }
        while releaseOlder == nil {
            await Task.yield()
        }

        // Newer fetch starts and finishes first.
        await model.loadDraftWorktreeBranches { _, _ in newerResult }
        #expect(model.draftWorktreeBranches?.currentBranch == "new-branch")
        #expect(model.draftWorktreeBranchesError == nil)

        // Older fetch now resolves; it must be discarded.
        releaseOlder?.resume(returning: olderResult)
        await olderTask.value
        #expect(model.draftWorktreeBranches?.currentBranch == "new-branch")
    }
}
