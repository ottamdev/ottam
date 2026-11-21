import Foundation
import OttamBackendClient

/// The worktree a confirmation is about. Core removes the worktree whether or
/// not its status call answered, so a failed call still yields one, from the
/// conversation list, with what removal discards left uncounted.
public struct WorktreeConfirmation: Equatable, Sendable {
    public let projectSourceID: ProjectSourceID
    public let branch: String
    public let conversationCount: Int
    /// Core's count of what removal discards; nil when Core could not say.
    public let status: WorktreeStatusResult?

    public init(_ status: WorktreeStatusResult) {
        projectSourceID = status.projectSourceID
        branch = status.branch
        conversationCount = status.conversationCount
        self.status = status
    }

    /// Nil for a root with no worktree to remove.
    public init?(uncounted root: WorkingRootSummary, conversations: [ConversationSummary]) {
        guard let sourceID = root.sourceID, let worktree = root.worktree else { return nil }
        projectSourceID = sourceID
        branch = worktree.branch
        conversationCount = max(1, conversations.filter { $0.workingRoot.sourceID == sourceID }.count)
        status = nil
    }
}

/// What the sidebar's alerts and notices say about a conversation's worktree.
/// Core removes a worktree with the last conversation working in it, and
/// keeps its branch only when the branch has moved off the commit it started
/// from.
public enum WorktreeText {
    /// The Delete Worktree… alert's title.
    public static func removalTitle(_ worktree: WorktreeConfirmation) -> String {
        "Delete Worktree “\(worktree.branch)”?"
    }

    /// The Delete Worktree… alert's message: what goes with the folder, and
    /// what becomes of the conversations working in it.
    public static func removalMessage(_ worktree: WorktreeConfirmation) -> String {
        let conversations = worktree.conversationCount == 1
            ? "The conversation working in it stays readable but can no longer continue."
            : "The \(worktree.conversationCount) conversations working in it stay readable but can no longer continue."
        return removal(worktree) + " " + conversations
    }

    /// The paragraph Delete Conversation… adds for a conversation in a
    /// worktree.
    ///
    /// - Parameter deletedChildren: the delegated children working in the same
    ///   worktree, which Delete All removes along with the conversation.
    public static func conversationDeletion(
        _ worktree: WorktreeConfirmation,
        deletedChildren: Int
    ) -> String {
        let others = worktree.conversationCount - 1 - deletedChildren
        if others > 0 {
            return others == 1
                ? "Its worktree, \(worktree.branch), stays: another conversation works in it."
                : "Its worktree, \(worktree.branch), stays: \(others) other conversations work in it."
        }
        let removal = removal(worktree)
        guard deletedChildren > 0 else {
            return "It works in the worktree \(worktree.branch). " + removal
        }
        return "It works in the worktree \(worktree.branch), and so do its delegated conversations. "
            + "Delete All removes the worktree with them: " + lowercasedFirst(removal)
            + " Keeping them keeps the worktree."
    }

    /// What a removal Core reported is worth telling: a folder left behind, a
    /// kept branch, or Core's word on either. Nil when it went as the alert
    /// said. Core's message says whether it tries again.
    public static func removalNotice(_ removal: WorktreeRemoval) -> String? {
        if !removal.removed {
            let reason = removal.message.map { ": \($0)" } ?? "."
            return "Could not remove the worktree at \(removal.path)\(reason)"
        }
        if let message = removal.message {
            return "Removed the worktree. \(message)"
        }
        guard removal.branchKept else { return nil }
        return "Removed the worktree. Its branch \(removal.branch) is kept."
    }

    /// The notice when a worktree was gone before its alert could open.
    public static func alreadyRemoved(_ root: WorkingRootSummary) -> String {
        "The worktree \(root.worktree?.branch ?? root.displayName) was already removed."
    }

    private static func removal(_ worktree: WorktreeConfirmation) -> String {
        guard let status = worktree.status else {
            return "This removes its folder and any uncommitted changes in it; Ottam could not count them. "
                + "The branch is deleted too unless it has new commits, and commits no branch holds are lost."
        }
        let lost = switch status.unbranchedCommits {
        case 0: ""
        case 1: " This also loses 1 commit that no branch holds."
        default: " This also loses \(status.unbranchedCommits) commits that no branch holds."
        }
        return removesFolder(status) + " " + branchFate(status) + lost
    }

    private static func removesFolder(_ status: WorktreeStatusResult) -> String {
        switch status.uncommittedChanges {
        case 0: "This removes its folder, which has no uncommitted changes."
        case 1: "This removes its folder and 1 uncommitted change."
        default: "This removes its folder and \(status.uncommittedChanges) uncommitted changes."
        }
    }

    /// Keyed on Core's `branchKept`, the rule removal applies; the count is
    /// the branch's own.
    private static func branchFate(_ status: WorktreeStatusResult) -> String {
        guard status.branchKept else { return "The branch has no commits of its own and is deleted too." }
        switch status.commits {
        case 0: return "The branch is kept."
        case 1: return "The branch is kept with its 1 commit."
        default: return "The branch is kept with its \(status.commits) commits."
        }
    }

    private static func lowercasedFirst(_ text: String) -> String {
        text.prefix(1).lowercased() + text.dropFirst()
    }
}
