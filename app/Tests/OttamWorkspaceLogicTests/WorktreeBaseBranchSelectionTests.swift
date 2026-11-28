import OttamBackendClient
import OttamWorkspaceLogic
import Testing

@Suite("WorktreeBaseBranchSelection")
struct WorktreeBaseBranchSelectionTests {
    private func branch(_ name: String, kind: WorktreeBaseBranchKind = .local) -> WorktreeBaseBranch {
        let prefix = kind == .local ? "refs/heads/" : "refs/remotes/"
        return WorktreeBaseBranch(name: name, refName: prefix + name, kind: kind)
    }

    @Test("current selection sends nil on the wire and pins the default branch below Current")
    func featureBranchDefaultAndPinnedMain() {
        let result = WorktreeBaseBranchesResult(
            currentBranch: "feat/auth",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: true,
            branches: [
                branch("feat/auth"),
                branch("main"),
                branch("chore/lint"),
                branch("origin/main", kind: .remote),
            ]
        )

        let state = WorktreeBaseBranchMenuState(result: result, selection: .current)
        #expect(!state.isTriggerDisabled)
        #expect(state.isCurrentHeadAvailable)
        #expect(state.isCurrentSelected)
        #expect(!state.isDefaultSelected)
        #expect(state.wireBaseRef == nil)
        #expect(state.selectedTitle == "feat/auth")
        #expect(state.currentHeadTitle == "feat/auth")
        #expect(state.defaultBranch == branch("main"))
        #expect(state.selectedCustomBranch == nil)
        #expect(state.allLocalBranches == [branch("feat/auth"), branch("main"), branch("chore/lint")])
        #expect(state.otherLocalBranches == [branch("chore/lint")])
        #expect(state.remoteBranches == [branch("origin/main", kind: .remote)])
        #expect(state.hasLocalBranchesDrilldown)
        #expect(state.hasRemoteBranchesDrilldown)
        #expect(state.isBranchSelected(branch("feat/auth")))
        #expect(!state.isBranchSelected(branch("main")))
        #expect(state.showsDirtyNote)
        #expect(state.helpText.contains("feat/auth"))
        #expect(state.helpText.contains("uncommitted changes"))
    }

    @Test("selecting Default sends the explicit default ref even when HEAD happens to be on it")
    func selectingDefaultBranchSendsExplicitRef() {
        let onFeature = WorktreeBaseBranchesResult(
            currentBranch: "feat/auth",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("feat/auth"), branch("main")]
        )
        let featureState = WorktreeBaseBranchMenuState(result: onFeature, selection: .defaultBranch)
        #expect(!featureState.isCurrentSelected)
        #expect(featureState.isDefaultSelected)
        #expect(featureState.wireBaseRef == "refs/heads/main")
        #expect(featureState.selectedTitle == "main")

        // Even when primary checkout is currently on `main`, `.defaultBranch`
        // keeps sending `"refs/heads/main"` so a branch switch before submit
        // never redirects the cut to another branch.
        let onMain = WorktreeBaseBranchesResult(
            currentBranch: "main",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("main"), branch("feat/other")]
        )
        let mainState = WorktreeBaseBranchMenuState(result: onMain, selection: .defaultBranch)
        #expect(mainState.defaultBranch == nil)
        #expect(mainState.isCurrentSelected)
        #expect(mainState.wireBaseRef == "refs/heads/main")
        #expect(mainState.selectedTitle == "main")
    }

    @Test("explicit named branch is sent verbatim and surfaced if missing rather than silently reverting")
    func explicitBranchSelectionAndMissingRef() {
        let result = WorktreeBaseBranchesResult(
            currentBranch: "feat/auth",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [
                branch("feat/auth"),
                branch("main"),
                branch("origin/release", kind: .remote),
            ]
        )

        let remoteState = WorktreeBaseBranchMenuState(
            result: result,
            selection: .branch("refs/remotes/origin/release")
        )
        #expect(!remoteState.isCurrentSelected)
        #expect(remoteState.wireBaseRef == "refs/remotes/origin/release")
        #expect(remoteState.selectedTitle == "origin/release")
        #expect(remoteState.selectedCustomBranch == branch("origin/release", kind: .remote))
        #expect(remoteState.missingSelectedRef == nil)
        #expect(remoteState.isBranchSelected(branch("origin/release", kind: .remote)))

        // Picking the current branch explicitly by name stays verbatim on the wire.
        let explicitCurrent = WorktreeBaseBranchMenuState(
            result: result,
            selection: .branch("refs/heads/feat/auth")
        )
        #expect(explicitCurrent.wireBaseRef == "refs/heads/feat/auth")

        // A missing ref is kept on the wire (for Core's recoverable error) and surfaced.
        let missing = WorktreeBaseBranchMenuState(
            result: result,
            selection: .branch("refs/heads/deleted")
        )
        #expect(missing.wireBaseRef == "refs/heads/deleted")
        #expect(missing.missingSelectedRef == "refs/heads/deleted")
        #expect(missing.selectedTitle == "deleted")
    }

    @Test("detached HEAD and unborn HEAD with fetched branches")
    func detachedAndUnbornHead() {
        let detached = WorktreeBaseBranchesResult(
            currentBranch: nil,
            detachedHead: "a1b2c3d",
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("main")]
        )
        let detachedState = WorktreeBaseBranchMenuState(result: detached, selection: .current)
        #expect(detachedState.isCurrentHeadAvailable)
        #expect(detachedState.selectedTitle == "HEAD (a1b2c3d)")
        #expect(detachedState.defaultBranch == branch("main"))

        let unborn = WorktreeBaseBranchesResult(
            currentBranch: nil,
            detachedHead: nil,
            defaultBranchRef: "refs/remotes/origin/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("origin/main", kind: .remote)]
        )
        let unbornState = WorktreeBaseBranchMenuState(result: unborn, selection: .current)
        #expect(!unbornState.isCurrentHeadAvailable)
        #expect(unbornState.defaultBranch == branch("origin/main", kind: .remote))
    }

    @Test("loading disables the trigger only while in flight; failures keep it clickable to retry")
    func loadingAndFailureTriggerState() {
        let loading = WorktreeBaseBranchMenuState(
            result: nil,
            isLoading: true,
            loadError: nil,
            selection: .current
        )
        #expect(loading.isTriggerDisabled)
        #expect(loading.selectedTitle == "…")

        let failed = WorktreeBaseBranchMenuState(
            result: nil,
            isLoading: false,
            loadError: "git timed out",
            selection: .current
        )
        #expect(!failed.isTriggerDisabled)
        #expect(failed.selectedTitle == "HEAD")
        #expect(failed.helpText.contains("retry"))
    }
}
