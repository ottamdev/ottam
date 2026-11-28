import Foundation
import OttamBackendClient

/// Presentation model for the composer's inline base-branch popover shown
/// while Worktree is ticked.
///
/// Option A behavior: `selectedBaseRef == nil` means the primary checkout's
/// current `HEAD`. When the repository's detected default branch differs from
/// `HEAD`, it is pinned right below Current so switching to a clean cut off
/// `main` is one click.
public struct WorktreeBaseBranchMenuState: Equatable, Sendable {
    public let result: WorktreeBaseBranchesResult?
    /// Explicit base ref to pass to `conversation.create`, or `nil` to track
    /// the primary checkout's current `HEAD`.
    public let effectiveBaseRef: String?
    /// Detected default branch when it differs from the primary checkout's
    /// current branch; pinned immediately after Current.
    public let defaultBranch: WorktreeBaseBranch?
    /// Active selection when it is neither Current nor the pinned default
    /// branch, so the root popover still shows what is checked.
    public let selectedCustomBranch: WorktreeBaseBranch?
    /// All local branches in Core's order (for the Local branches sub-page).
    public let allLocalBranches: [WorktreeBaseBranch]
    /// All remote-tracking branches in Core's order (for the Remote branches sub-page).
    public let allRemoteBranches: [WorktreeBaseBranch]
    /// Local branches other than Current and the pinned default branch.
    public let otherLocalBranches: [WorktreeBaseBranch]
    /// Remote-tracking branches other than a remote-only pinned default branch.
    public let remoteBranches: [WorktreeBaseBranch]

    private let currentBranchRef: String?

    public init(result: WorktreeBaseBranchesResult?, selectedBaseRef: String?) {
        self.result = result

        let currentRef = result?.currentBranch.map { "refs/heads/\($0)" }
        self.currentBranchRef = currentRef
        let branches = result?.branches ?? []

        // Keep an in-flight selection until the branch list arrives; once
        // loaded, fall back to HEAD if the chosen ref no longer exists or
        // matches the current branch.
        if let selectedBaseRef {
            if let result {
                if selectedBaseRef == currentRef {
                    effectiveBaseRef = nil
                } else if result.branches.contains(where: { $0.refName == selectedBaseRef }) {
                    effectiveBaseRef = selectedBaseRef
                } else {
                    effectiveBaseRef = nil
                }
            } else {
                effectiveBaseRef = selectedBaseRef
            }
        } else {
            effectiveBaseRef = nil
        }

        if let defaultRef = result?.defaultBranchRef,
           defaultRef != currentRef,
           let matched = branches.first(where: { $0.refName == defaultRef })
        {
            defaultBranch = matched
        } else {
            defaultBranch = nil
        }

        let pinnedDefaultRef = defaultBranch?.refName
        if let effectiveBaseRef,
           effectiveBaseRef != pinnedDefaultRef,
           let matched = branches.first(where: { $0.refName == effectiveBaseRef })
        {
            selectedCustomBranch = matched
        } else {
            selectedCustomBranch = nil
        }

        allLocalBranches = branches.filter { $0.kind == .local }
        allRemoteBranches = branches.filter { $0.kind == .remote }
        otherLocalBranches = allLocalBranches.filter { branch in
            branch.refName != currentRef && branch.refName != pinnedDefaultRef
        }
        remoteBranches = allRemoteBranches.filter { branch in
            branch.refName != pinnedDefaultRef
        }
    }

    public var isLoading: Bool {
        result == nil
    }

    public var isCurrentSelected: Bool {
        effectiveBaseRef == nil
    }

    public var showsDirtyNote: Bool {
        result?.primaryHasUncommittedChanges == true
    }

    public var hasLocalBranchesDrilldown: Bool {
        !otherLocalBranches.isEmpty
    }

    public var hasRemoteBranchesDrilldown: Bool {
        !remoteBranches.isEmpty
    }

    /// Whether `branch` is the active choice (including Current HEAD when
    /// `effectiveBaseRef == nil`).
    public func isBranchSelected(_ branch: WorktreeBaseBranch) -> Bool {
        if let effectiveBaseRef {
            return branch.refName == effectiveBaseRef
        }
        return branch.refName == currentBranchRef
    }

    /// The ref to store when picking `branch`: choosing the primary checkout's
    /// current branch normalizes back to `nil` so the draft continues tracking `HEAD`.
    public func selectionRef(for branch: WorktreeBaseBranch) -> String? {
        branch.refName == currentBranchRef ? nil : branch.refName
    }

    /// Bare branch title for the Current HEAD option and closed trigger face.
    public var currentHeadTitle: String {
        if let current = result?.currentBranch {
            return current
        }
        if let detached = result?.detachedHead {
            return "HEAD (\(detached))"
        }
        return "HEAD"
    }

    /// Bare title of the active selection (without `"from"`), used for tooltips,
    /// loading placeholder, and closed trigger display.
    public var selectedTitle: String {
        if let effectiveBaseRef {
            if let matched = result?.branches.first(where: { $0.refName == effectiveBaseRef }) {
                return matched.name
            }
            return Self.shortDisplayName(for: effectiveBaseRef)
        }
        if isLoading {
            return "…"
        }
        return currentHeadTitle
    }

    public var helpText: String {
        let base = isLoading ? "Loading base branches…" : "Base branch: \(selectedTitle)"
        if showsDirtyNote {
            return "\(base). Starts clean from committed state; uncommitted changes stay in the primary checkout."
        }
        return base
    }

    private static func shortDisplayName(for refName: String) -> String {
        for prefix in ["refs/heads/", "refs/remotes/"] {
            if refName.hasPrefix(prefix) {
                return String(refName.dropFirst(prefix.count))
            }
        }
        return refName
    }
}
