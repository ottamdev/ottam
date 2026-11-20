import Foundation
import OttamBackendClient

/// What the sidebar's alerts and notices say about a conversation's worktree.
/// Core removes a worktree with the last conversation working in it, and
/// keeps its branch only when the branch has commits of its own.
public enum WorktreeText {
    /// The Delete Worktree… alert's title.
    public static func removalTitle(_ status: WorktreeStatusResult) -> String {
        "Delete Worktree “\(status.branch)”?"
    }

    /// The Delete Worktree… alert's message: what goes with the folder, and
    /// what becomes of the conversations working in it.
    public static func removalMessage(_ status: WorktreeStatusResult) -> String {
        let conversations = status.conversationCount == 1
            ? "The conversation working in it stays readable but can no longer continue."
            : "The \(status.conversationCount) conversations working in it stay readable but can no longer continue."
        return removesFolder(status) + " " + branchFate(status) + " " + conversations
    }

    /// The paragraph Delete Conversation… adds for a conversation in a
    /// worktree, or nil when it has none.
    ///
    /// - Parameter deletedChildren: the delegated children working in the same
    ///   worktree, which Delete All removes along with the conversation.
    public static func conversationDeletion(
        _ status: WorktreeStatusResult,
        deletedChildren: Int
    ) -> String {
        let others = status.conversationCount - 1 - deletedChildren
        if others > 0 {
            return others == 1
                ? "Its worktree, \(status.branch), stays: another conversation works in it."
                : "Its worktree, \(status.branch), stays: \(others) other conversations work in it."
        }
        let removal = removesFolder(status) + " " + branchFate(status)
        guard deletedChildren > 0 else {
            return "It works in the worktree \(status.branch). " + removal
        }
        return "It works in the worktree \(status.branch), and so do its delegated conversations. "
            + "Delete All removes the worktree with them: " + lowercasedFirst(removal)
            + " Keeping them keeps the worktree."
    }

    /// What a removal Core reported is worth telling: a branch kept for its
    /// commits, or a folder left behind. Nil when it went as the alert said.
    public static func removalNotice(_ removal: WorktreeRemoval) -> String? {
        if !removal.removed {
            let reason = removal.message.map { " (\($0))" } ?? ""
            return "Could not remove the worktree at \(removal.path)\(reason). Ottam tries again on its next launch."
        }
        guard removal.branchKept else { return nil }
        return "Removed the worktree. The branch \(removal.branch) is kept with its commits."
    }

    private static func removesFolder(_ status: WorktreeStatusResult) -> String {
        switch status.uncommittedChanges {
        case 0: "This removes its folder, which has no uncommitted changes."
        case 1: "This removes its folder and 1 uncommitted change."
        default: "This removes its folder and \(status.uncommittedChanges) uncommitted changes."
        }
    }

    private static func branchFate(_ status: WorktreeStatusResult) -> String {
        switch status.commits {
        case 0: "The branch has no commits of its own and is deleted too."
        case 1: "The branch is kept with its 1 commit."
        default: "The branch is kept with its \(status.commits) commits."
        }
    }

    private static func lowercasedFirst(_ text: String) -> String {
        text.prefix(1).lowercased() + text.dropFirst()
    }
}
