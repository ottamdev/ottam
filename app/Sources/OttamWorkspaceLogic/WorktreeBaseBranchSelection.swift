import Foundation
import OttamBackendClient

/// Saved new-chat preference for which base a worktree starts from.
/// Only the abstract role (`current` vs. `default`) persists across sessions;
/// one-off named branch picks never persist.
public enum WorktreeBasePreference: String, Equatable, Sendable {
    case current
    case defaultBranch = "default"
}

/// Active base-branch selection for the current draft worktree.
public enum WorktreeBaseSelection: Equatable, Sendable {
    case current
    case defaultBranch
    /// An explicit branch ref (`refs/heads/…` or `refs/remotes/…`) picked for
    /// this draft only.
    case branch(String)
}

/// Presentation model for the composer's inline base-branch popover shown
/// while Worktree is ticked.
///
/// New worktrees default to the primary checkout's current `HEAD` unless the
/// user selects the detected default branch (`main`/`master`/`origin/HEAD`) or
/// picks another local or remote branch for the current draft.
public struct WorktreeBaseBranchMenuState: Equatable, Sendable {
    public let result: WorktreeBaseBranchesResult?
    public let isLoading: Bool
    public let loadError: String?
    public let selection: WorktreeBaseSelection
    /// Detected default branch when it differs from the primary checkout's
    /// current branch; pinned immediately after Current.
    public let defaultBranch: WorktreeBaseBranch?
    /// Active one-off branch when it is neither Current nor the pinned default
    /// branch, so the root popover shows what is checked.
    public let selectedCustomBranch: WorktreeBaseBranch?
    /// Set when an explicit `.branch(ref)` is not present in the loaded branch
    /// list, so the UI surfaces the missing ref instead of silently reverting.
    public let missingSelectedRef: String?
    /// All local branches in Core's order (for the Local branches sub-page).
    public let allLocalBranches: [WorktreeBaseBranch]
    /// All remote-tracking branches in Core's order (for the Remote branches sub-page).
    public let allRemoteBranches: [WorktreeBaseBranch]
    /// Local branches other than Current and the pinned default branch.
    public let otherLocalBranches: [WorktreeBaseBranch]
    /// Remote-tracking branches other than a remote-only pinned default branch.
    public let remoteBranches: [WorktreeBaseBranch]

    private let currentBranchRef: String?

    public init(
        result: WorktreeBaseBranchesResult?,
        isLoading: Bool = false,
        loadError: String? = nil,
        selection: WorktreeBaseSelection
    ) {
        self.result = result
        self.isLoading = isLoading
        self.loadError = loadError
        self.selection = selection

        let currentRef = result?.currentBranch.map { "refs/heads/\($0)" }
        self.currentBranchRef = currentRef
        let branches = result?.branches ?? []

        if let defaultRef = result?.defaultBranchRef,
           defaultRef != currentRef,
           let matched = branches.first(where: { $0.refName == defaultRef })
        {
            defaultBranch = matched
        } else {
            defaultBranch = nil
        }

        let pinnedDefaultRef = defaultBranch?.refName
        switch selection {
        case .current, .defaultBranch:
            selectedCustomBranch = nil
            missingSelectedRef = nil
        case .branch(let refName):
            if let matched = branches.first(where: { $0.refName == refName }) {
                selectedCustomBranch = refName == pinnedDefaultRef ? nil : matched
                missingSelectedRef = nil
            } else {
                selectedCustomBranch = nil
                missingSelectedRef = result != nil ? refName : nil
            }
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

    /// The `worktreeBaseRef` sent to `conversation.create`.
    ///
    /// Explicit choices (`.branch` and `.defaultBranch` when Core reported a
    /// `defaultBranchRef`) are sent verbatim rather than converted to `nil`, so
    /// a branch switch in another terminal cannot silently redirect them to the
    /// wrong commit.
    public var wireBaseRef: String? {
        switch selection {
        case .current:
            return nil
        case .defaultBranch:
            return result?.defaultBranchRef
        case .branch(let refName):
            return refName
        }
    }

    /// Only disable the trigger during the initial fetch when no result or error
    /// has arrived yet; a failed fetch keeps the trigger open so the user can retry.
    public var isTriggerDisabled: Bool {
        isLoading && result == nil
    }

    /// False on an unborn HEAD (repository with no commits on HEAD yet).
    public var isCurrentHeadAvailable: Bool {
        guard let result else { return true }
        return result.currentBranch != nil || result.detachedHead != nil
    }

    public var isCurrentSelected: Bool {
        switch selection {
        case .current:
            return true
        case .defaultBranch:
            return defaultBranch == nil
        case .branch:
            return false
        }
    }

    public var isDefaultSelected: Bool {
        guard let defaultBranch else { return false }
        switch selection {
        case .defaultBranch:
            return true
        case .branch(let refName):
            return refName == defaultBranch.refName
        case .current:
            return false
        }
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

    public func isBranchSelected(_ branch: WorktreeBaseBranch) -> Bool {
        switch selection {
        case .current:
            return branch.refName == currentBranchRef
        case .defaultBranch:
            if let defaultRef = result?.defaultBranchRef {
                return branch.refName == defaultRef
            }
            return branch.refName == currentBranchRef
        case .branch(let refName):
            return branch.refName == refName
        }
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

    /// Bare title of the active selection (without `"from"`), used for tooltips
    /// and the closed trigger display.
    public var selectedTitle: String {
        switch selection {
        case .branch(let refName):
            if let matched = result?.branches.first(where: { $0.refName == refName }) {
                return matched.name
            }
            return Self.shortDisplayName(for: refName)
        case .defaultBranch:
            if let defaultBranch {
                return defaultBranch.name
            }
            if let defaultRef = result?.defaultBranchRef,
               let matched = result?.branches.first(where: { $0.refName == defaultRef })
            {
                return matched.name
            }
            if result != nil {
                return currentHeadTitle
            }
            return isLoading ? "…" : "HEAD"
        case .current:
            if result != nil {
                return currentHeadTitle
            }
            return isLoading ? "…" : "HEAD"
        }
    }

    public var helpText: String {
        if let loadError, result == nil {
            return "Could not list branches (\(loadError)). Click to retry, or send to cut from HEAD."
        }
        let base = isTriggerDisabled ? "Loading base branches…" : "Base branch: \(selectedTitle)"
        if showsDirtyNote {
            return "\(base). Starts clean from committed state; uncommitted changes stay in the primary checkout."
        }
        return base
    }

    public static func shortDisplayName(for refName: String) -> String {
        for prefix in ["refs/heads/", "refs/remotes/"] {
            if refName.hasPrefix(prefix) {
                return String(refName.dropFirst(prefix.count))
            }
        }
        return refName
    }
}
