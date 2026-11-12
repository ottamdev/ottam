import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("WorkspaceContextSubtitle")
struct WorkspaceContextSubtitleTests {
    private func project(_ name: String, branch: String?) -> WorkspaceProjectSummary {
        var member = Fixture.project(name)
        member.source.currentBranch = branch
        return member
    }

    @Test("a project root among several shows its name and last-seen branch")
    func projectBranch() {
        let alpha = project("alpha", branch: "main")
        let beta = project("beta", branch: "develop")
        let subtitle = WorkspaceContextSubtitle.resolve(
            rootName: "alpha",
            sourceID: alpha.source.projectSourceID,
            projects: [alpha, beta]
        )
        #expect(subtitle == "alpha · main")
    }

    @Test("a single-project Workspace's own root shows just the branch")
    func singleProjectBranch() {
        let alpha = project("alpha", branch: "main")
        let subtitle = WorkspaceContextSubtitle.resolve(
            rootName: "alpha",
            sourceID: alpha.source.projectSourceID,
            projects: [alpha]
        )
        #expect(subtitle == "main")
    }

    @Test("a loaded diff's branch wins over the project summary")
    func loadedBranchWins() {
        let alpha = project("alpha", branch: "main")
        let subtitle = WorkspaceContextSubtitle.resolve(
            rootName: "alpha",
            sourceID: alpha.source.projectSourceID,
            projects: [alpha],
            loadedBranches: [alpha.source.projectSourceID: "feature ↑2"]
        )
        #expect(subtitle == "feature ↑2")
    }

    @Test("a loaded diff for another source does not leak in")
    func loadedBranchScopedBySource() {
        let alpha = project("alpha", branch: "main")
        let beta = project("beta", branch: "develop")
        let subtitle = WorkspaceContextSubtitle.resolve(
            rootName: "alpha",
            sourceID: alpha.source.projectSourceID,
            projects: [alpha, beta],
            loadedBranches: [beta.source.projectSourceID: "other"]
        )
        #expect(subtitle == "alpha · main")
    }

    @Test("an unknown or blank branch leaves only what names the root")
    func missingBranch() {
        let alpha = project("alpha", branch: "  ")
        #expect(
            WorkspaceContextSubtitle.resolve(
                rootName: "alpha",
                sourceID: alpha.source.projectSourceID,
                projects: [alpha]
            ).isEmpty
        )
        #expect(
            WorkspaceContextSubtitle.resolve(
                rootName: "alpha",
                sourceID: ProjectSourceID("source-gone"),
                projects: [alpha]
            ) == "alpha"
        )
    }

    @Test("Workspace files have no git, so no subtitle, whatever the shape")
    func workspaceFiles() {
        let alpha = project("alpha", branch: "main")
        let beta = project("beta", branch: "develop")
        for projects in [[], [alpha], [alpha, beta]] {
            #expect(
                WorkspaceContextSubtitle.resolve(
                    rootName: "Workspace files",
                    sourceID: nil,
                    projects: projects
                ).isEmpty
            )
        }
    }

    @Test("no root means no subtitle")
    func noRoot() {
        #expect(WorkspaceContextSubtitle.resolve(rootName: nil, sourceID: nil, projects: []).isEmpty)
        #expect(WorkspaceContextSubtitle.resolve(rootName: "", sourceID: nil, projects: []).isEmpty)
    }
}
