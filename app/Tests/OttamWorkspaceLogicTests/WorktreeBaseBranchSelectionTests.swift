import OttamBackendClient
import OttamWorkspaceLogic
import Testing

@Suite("WorktreeBaseBranchSelection")
struct WorktreeBaseBranchSelectionTests {
    private func branch(_ name: String, kind: WorktreeBaseBranchKind = .local) -> WorktreeBaseBranch {
        let prefix = kind == .local ? "refs/heads/" : "refs/remotes/"
        return WorktreeBaseBranch(name: name, refName: prefix + name, kind: kind)
    }

    @Test("Option A defaults to HEAD on a feature branch and pins the default branch below it")
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

        let state = WorktreeBaseBranchMenuState(result: result, selectedBaseRef: nil)
        #expect(!state.isLoading)
        #expect(state.isCurrentSelected)
        #expect(state.effectiveBaseRef == nil)
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
        #expect(state.selectionRef(for: branch("feat/auth")) == nil)
        #expect(state.selectionRef(for: branch("main")) == "refs/heads/main")
        #expect(state.showsDirtyNote)
        #expect(state.helpText.contains("feat/auth"))
        #expect(state.helpText.contains("uncommitted changes"))
    }

    @Test("selecting a non-default branch exposes selectedCustomBranch for the root popover")
    func selectingCustomBranch() {
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

        let state = WorktreeBaseBranchMenuState(
            result: result,
            selectedBaseRef: "refs/remotes/origin/release"
        )
        #expect(!state.isCurrentSelected)
        #expect(state.selectedTitle == "origin/release")
        #expect(state.selectedCustomBranch == branch("origin/release", kind: .remote))
        #expect(state.isBranchSelected(branch("origin/release", kind: .remote)))
        #expect(!state.isBranchSelected(branch("feat/auth")))
        #expect(!state.hasLocalBranchesDrilldown)
        #expect(state.hasRemoteBranchesDrilldown)
    }

    @Test("selecting the default branch sends its ref and updates the label")
    func selectingDefaultBranch() {
        let result = WorktreeBaseBranchesResult(
            currentBranch: "feat/auth",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("feat/auth"), branch("main")]
        )

        let state = WorktreeBaseBranchMenuState(result: result, selectedBaseRef: "refs/heads/main")
        #expect(!state.isCurrentSelected)
        #expect(state.effectiveBaseRef == "refs/heads/main")
        #expect(state.selectedTitle == "main")
        #expect(!state.showsDirtyNote)
    }

    @Test("on the default branch itself, default is not duplicated below Current")
    func onDefaultBranchNoDuplicate() {
        let result = WorktreeBaseBranchesResult(
            currentBranch: "main",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("main"), branch("feat/other")]
        )

        let state = WorktreeBaseBranchMenuState(result: result, selectedBaseRef: nil)
        #expect(state.selectedTitle == "main")
        #expect(state.defaultBranch == nil)
        #expect(state.otherLocalBranches == [branch("feat/other")])

        // Picking the current branch's ref normalizes back to nil (tracks HEAD).
        let sameRef = WorktreeBaseBranchMenuState(result: result, selectedBaseRef: "refs/heads/main")
        #expect(sameRef.effectiveBaseRef == nil)
        #expect(sameRef.isCurrentSelected)
    }

    @Test("detached HEAD shows short SHA and still pins default branch")
    func detachedHead() {
        let result = WorktreeBaseBranchesResult(
            currentBranch: nil,
            detachedHead: "a1b2c3d",
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("main")]
        )

        let state = WorktreeBaseBranchMenuState(result: result, selectedBaseRef: nil)
        #expect(state.selectedTitle == "HEAD (a1b2c3d)")
        #expect(state.currentHeadTitle == "HEAD (a1b2c3d)")
        #expect(state.defaultBranch == branch("main"))
    }

    @Test("loading state shows ellipsis until branches arrive, and stale refs fall back to HEAD")
    func loadingAndStaleRefHandling() {
        let loadingDefault = WorktreeBaseBranchMenuState(result: nil, selectedBaseRef: nil)
        #expect(loadingDefault.isLoading)
        #expect(loadingDefault.selectedTitle == "…")

        let loadingSelected = WorktreeBaseBranchMenuState(result: nil, selectedBaseRef: "refs/heads/main")
        #expect(loadingSelected.isLoading)
        #expect(loadingSelected.effectiveBaseRef == "refs/heads/main")
        #expect(loadingSelected.selectedTitle == "main")

        let loaded = WorktreeBaseBranchesResult(
            currentBranch: "main",
            detachedHead: nil,
            defaultBranchRef: "refs/heads/main",
            primaryHasUncommittedChanges: false,
            branches: [branch("main")]
        )
        let stale = WorktreeBaseBranchMenuState(result: loaded, selectedBaseRef: "refs/heads/deleted")
        #expect(stale.effectiveBaseRef == nil)
        #expect(stale.isCurrentSelected)
        #expect(stale.selectedTitle == "main")
    }
}
