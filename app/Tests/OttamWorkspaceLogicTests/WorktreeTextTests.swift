import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("WorktreeText")
struct WorktreeTextTests {
    private func status(changes: Int = 0, commits: Int = 0, conversations: Int = 1) -> WorktreeStatusResult {
        WorktreeStatusResult(
            projectSourceID: ProjectSourceID("source-worktree"),
            branch: "ottam/fix-login-3f2a",
            path: "/tmp/worktrees/alpha/fix-login-3f2a",
            uncommittedChanges: changes,
            commits: commits,
            conversationCount: conversations
        )
    }

    @Test("Delete Worktree counts uncommitted changes and says what becomes of the branch")
    func removal() {
        #expect(WorktreeText.removalTitle(status()) == "Delete Worktree “ottam/fix-login-3f2a”?")
        #expect(
            WorktreeText.removalMessage(status(changes: 3)) == "This removes its folder and 3 uncommitted changes. "
                + "The branch has no commits of its own and is deleted too. "
                + "The conversation working in it stays readable but can no longer continue."
        )
        #expect(
            WorktreeText.removalMessage(status(changes: 1, commits: 2, conversations: 2))
                == "This removes its folder and 1 uncommitted change. The branch is kept with its 2 commits. "
                + "The 2 conversations working in it stay readable but can no longer continue."
        )
        #expect(WorktreeText.removalMessage(status()).hasPrefix("This removes its folder, which has no uncommitted changes."))
    }

    @Test("deleting the last conversation in a worktree names the worktree and what goes")
    func lastConversation() {
        #expect(
            WorktreeText.conversationDeletion(status(changes: 2, commits: 1), deletedChildren: 0)
                == "It works in the worktree ottam/fix-login-3f2a. This removes its folder and 2 uncommitted changes. "
                + "The branch is kept with its 1 commit."
        )
    }

    @Test("a worktree other conversations still work in stays")
    func sharedWorktree() {
        #expect(
            WorktreeText.conversationDeletion(status(conversations: 2), deletedChildren: 0)
                == "Its worktree, ottam/fix-login-3f2a, stays: another conversation works in it."
        )
        #expect(
            WorktreeText.conversationDeletion(status(conversations: 4), deletedChildren: 1)
                == "Its worktree, ottam/fix-login-3f2a, stays: 2 other conversations work in it."
        )
    }

    @Test("a parent whose children share its worktree: Delete All removes it, keeping them keeps it")
    func parentWithChildren() {
        let text = WorktreeText.conversationDeletion(status(conversations: 2), deletedChildren: 1)
        #expect(text.hasPrefix("It works in the worktree ottam/fix-login-3f2a, and so do its delegated conversations. "))
        #expect(text.contains("Delete All removes the worktree with them: this removes its folder"))
        #expect(text.hasSuffix("Keeping them keeps the worktree."))
    }

    @Test("a removal is worth a notice only for a kept branch or a folder left behind")
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
                == "Removed the worktree. The branch ottam/fix-login-3f2a is kept with its commits."
        )
        #expect(
            WorktreeText.removalNotice(removal(removed: false, branchKept: true, message: "Permission denied"))
                == "Could not remove the worktree at /tmp/worktrees/alpha/fix-login-3f2a (Permission denied). "
                + "Ottam tries again on its next launch."
        )
    }
}
