import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("WorktreeText")
struct WorktreeTextTests {
    private func worktree(
        changes: Int = 0,
        commits: Int = 0,
        branchKept: Bool? = nil,
        unbranched: Int = 0,
        conversations: Int = 1
    ) -> WorktreeConfirmation {
        WorktreeConfirmation(WorktreeStatusResult(
            projectSourceID: ProjectSourceID("source-worktree"),
            branch: "ottam/fix-login-3f2a",
            path: "/tmp/worktrees/alpha/fix-login-3f2a",
            uncommittedChanges: changes,
            commits: commits,
            branchKept: branchKept ?? (commits > 0),
            unbranchedCommits: unbranched,
            conversationCount: conversations
        ))
    }

    /// What the alerts know when `worktree.status` fails: the list's
    /// conversations on that worktree, and no counts.
    private func uncounted(conversations: Int) -> WorktreeConfirmation {
        let alpha = Fixture.project("alpha")
        let first = Fixture.worktreeConversation("fix", in: alpha)
        var listed = [first, Fixture.conversation("elsewhere", in: alpha)]
        for index in 1..<conversations {
            var sharing = Fixture.worktreeConversation("fix-\(index)", in: alpha)
            sharing.workingRoot = first.workingRoot
            listed.append(sharing)
        }
        return WorktreeConfirmation(uncounted: first.workingRoot, conversations: listed)!
    }

    @Test("Delete Worktree counts uncommitted changes and says what becomes of the branch")
    func removal() {
        #expect(WorktreeText.removalTitle(worktree()) == "Delete Worktree “ottam/fix-login-3f2a”?")
        #expect(
            WorktreeText.removalMessage(worktree(changes: 3)) == "This removes its folder and 3 uncommitted changes. "
                + "The branch has no commits of its own and is deleted too. "
                + "The conversation working in it stays readable but can no longer continue."
        )
        #expect(
            WorktreeText.removalMessage(worktree(changes: 1, commits: 2, conversations: 2))
                == "This removes its folder and 1 uncommitted change. The branch is kept with its 2 commits. "
                + "The 2 conversations working in it stay readable but can no longer continue."
        )
        #expect(WorktreeText.removalMessage(worktree()).hasPrefix("This removes its folder, which has no uncommitted changes."))
    }

    @Test("the branch's fate follows Core's branchKept, and commits on no branch are called lost")
    func branchFate() {
        // Moved off its base without commits of its own, as after a reset.
        #expect(WorktreeText.removalMessage(worktree(branchKept: true)).contains(" The branch is kept. "))
        // A detached HEAD's commits: the branch goes, and so do they.
        #expect(
            WorktreeText.removalMessage(worktree(unbranched: 2)).hasPrefix(
                "This removes its folder, which has no uncommitted changes. "
                    + "The branch has no commits of its own and is deleted too. "
                    + "This also loses 2 commits that no branch holds."
            )
        )
        #expect(WorktreeText.removalMessage(worktree(unbranched: 1)).contains("This also loses 1 commit that no branch holds."))
    }

    @Test("deleting the last conversation in a worktree names the worktree and what goes")
    func lastConversation() {
        #expect(
            WorktreeText.conversationDeletion(worktree(changes: 2, commits: 1), deletedChildren: 0)
                == "It works in the worktree ottam/fix-login-3f2a. This removes its folder and 2 uncommitted changes. "
                + "The branch is kept with its 1 commit."
        )
    }

    @Test("a worktree other conversations still work in stays")
    func sharedWorktree() {
        #expect(
            WorktreeText.conversationDeletion(worktree(conversations: 2), deletedChildren: 0)
                == "Its worktree, ottam/fix-login-3f2a, stays: another conversation works in it."
        )
        #expect(
            WorktreeText.conversationDeletion(worktree(conversations: 4), deletedChildren: 1)
                == "Its worktree, ottam/fix-login-3f2a, stays: 2 other conversations work in it."
        )
    }

    @Test("a parent whose children share its worktree: Delete All removes it, keeping them keeps it")
    func parentWithChildren() {
        let text = WorktreeText.conversationDeletion(worktree(conversations: 2), deletedChildren: 1)
        #expect(text.hasPrefix("It works in the worktree ottam/fix-login-3f2a, and so do its delegated conversations. "))
        #expect(text.contains("Delete All removes the worktree with them: this removes its folder"))
        #expect(text.hasSuffix("Keeping them keeps the worktree."))
    }

    @Test("when Core cannot count, both alerts still say the folder and its changes go")
    func uncountedFallback() {
        let unknown = "This removes its folder and any uncommitted changes in it; Ottam could not count them. "
            + "The branch is deleted too unless it has new commits."
        let alone = uncounted(conversations: 1)
        #expect(alone.projectSourceID == ProjectSourceID("source-worktree-fix"))
        #expect(WorktreeText.removalTitle(alone) == "Delete Worktree “ottam/fix-login-3f2a”?")
        #expect(
            WorktreeText.removalMessage(alone)
                == unknown + " The conversation working in it stays readable but can no longer continue."
        )
        #expect(
            WorktreeText.conversationDeletion(alone, deletedChildren: 0)
                == "It works in the worktree ottam/fix-login-3f2a. " + unknown
        )

        // The list's count decides whether it stays, as Core's would.
        let shared = uncounted(conversations: 2)
        #expect(
            WorktreeText.conversationDeletion(shared, deletedChildren: 0)
                == "Its worktree, ottam/fix-login-3f2a, stays: another conversation works in it."
        )
        #expect(
            WorktreeText.conversationDeletion(shared, deletedChildren: 1)
                == "It works in the worktree ottam/fix-login-3f2a, and so do its delegated conversations. "
                + "Delete All removes the worktree with them: this removes its folder and any uncommitted "
                + "changes in it; Ottam could not count them. The branch is deleted too unless it has new commits. "
                + "Keeping them keeps the worktree."
        )

        // A root with no worktree has nothing to confirm.
        let plain = Fixture.conversation("plain", in: Fixture.project("alpha"))
        #expect(WorktreeConfirmation(uncounted: plain.workingRoot, conversations: [plain]) == nil)
    }

    @Test("a removal notice quotes Core, which alone says whether it tries again")
    func removalNotice() {
        func removal(removed: Bool, branchKept: Bool, message: String? = nil) -> WorktreeRemoval {
            WorktreeRemoval(
                projectSourceID: ProjectSourceID("source-worktree"),
                branch: "ottam/fix-login-3f2a",
                path: "/tmp/worktrees/alpha/fix-login-3f2a",
                removed: removed,
                branchKept: branchKept,
                message: message
            )
        }
        #expect(WorktreeText.removalNotice(removal(removed: true, branchKept: false)) == nil)
        #expect(
            WorktreeText.removalNotice(removal(removed: true, branchKept: true))
                == "Removed the worktree. Its branch ottam/fix-login-3f2a is kept."
        )
        let retried = "Permission denied (os error 13). Ottam tries again on its next launch."
        #expect(
            WorktreeText.removalNotice(removal(removed: false, branchKept: true, message: retried))
                == "Could not remove the worktree at /tmp/worktrees/alpha/fix-login-3f2a: " + retried
        )
        // The refusal outside Ottam's folder promises nothing.
        let refused = WorktreeText.removalNotice(removal(
            removed: false,
            branchKept: true,
            message: "Ottam removes only worktrees it made in its own folder."
        ))
        #expect(refused == "Could not remove the worktree at /tmp/worktrees/alpha/fix-login-3f2a: "
            + "Ottam removes only worktrees it made in its own folder.")
        #expect(refused?.contains("next launch") == false)
        #expect(
            WorktreeText.removalNotice(removal(
                removed: true,
                branchKept: true,
                message: "The project's checkout is gone, so its branch was left alone."
            )) == "Removed the worktree. The project's checkout is gone, so its branch was left alone."
        )
    }
}
