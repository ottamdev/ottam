import Foundation
import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("WorkspaceShape")
struct WorkspaceShapeTests {
    @Test("shape derives from project count")
    func shapeFromCount() {
        #expect(WorkspaceShape.of(projectCount: 0) == .empty)
        #expect(WorkspaceShape.of(projectCount: 1) == .single)
        #expect(WorkspaceShape.of(projectCount: 2) == .multiple)
        #expect(WorkspaceShape.of(projectCount: 9) == .multiple)
    }

    @Test("only the multi-project shape offers a choice")
    func chooserCollapses() {
        #expect(WorkspaceShape.empty.collapsesRootChooser)
        #expect(WorkspaceShape.single.collapsesRootChooser)
        #expect(!WorkspaceShape.multiple.collapsesRootChooser)
    }
}

@Suite("workspaceFolderSummary")
struct WorkspaceFolderSummaryTests {
    @Test("no projects says so")
    func emptyShape() {
        #expect(workspaceFolderSummary(projectPaths: []) == "No projects")
    }

    @Test("one project shows its path")
    func singleShape() {
        #expect(workspaceFolderSummary(projectPaths: ["/tmp/alpha"]) == "/tmp/alpha")
    }

    @Test("a path under the home folder is abbreviated")
    func singleShapeUnderHome() {
        let underHome = NSHomeDirectory() + "/Developer/alpha"

        #expect(workspaceFolderSummary(projectPaths: [underHome]) == "~/Developer/alpha")
    }

    // The home folder's path is a string prefix of its sibling's, and the
    // sibling is not under it.
    @Test("a sibling of the home folder keeps its full path")
    func singleShapeBesideHome() {
        let beside = NSHomeDirectory() + "-old/alpha"

        #expect(workspaceFolderSummary(projectPaths: [beside]) == beside)
    }

    // One path would be wrong above several projects.
    @Test("several projects show a count")
    func multipleShape() {
        #expect(
            workspaceFolderSummary(projectPaths: ["/tmp/alpha", "/tmp/beta", "/tmp/gamma"]) == "3 projects"
        )
    }
}

@Suite("impliedWorkingRoot")
struct ImpliedWorkingRootTests {
    @Test("zero projects implies workspace files")
    func emptyShape() {
        #expect(
            impliedWorkingRoot(projects: [], selectedProject: nil) == .workspaceFiles
        )
    }

    @Test("one project implies that project")
    func singleShape() {
        let alpha = Fixture.project("alpha")

        #expect(
            impliedWorkingRoot(projects: [alpha], selectedProject: nil)
                == Fixture.selection(alpha)
        )
    }

    // In the collapsed shape there is nothing else the user could have meant, so
    // a stale selection must not win over the lone project.
    @Test("one project ignores a selection naming something else")
    func singleShapeIgnoresStaleSelection() {
        let alpha = Fixture.project("alpha")
        let departed = Fixture.project("departed")

        #expect(
            impliedWorkingRoot(
                projects: [alpha],
                selectedProject: Fixture.resolved(departed)
            ) == Fixture.selection(alpha)
        )
    }

    @Test("multiple projects imply the selected project")
    func multipleShape() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")

        #expect(
            impliedWorkingRoot(
                projects: [alpha, beta],
                selectedProject: Fixture.resolved(beta)
            ) == Fixture.selection(beta)
        )
    }

    @Test("multiple projects with nothing selected fall back to workspace files")
    func multipleShapeWithoutSelection() {
        #expect(
            impliedWorkingRoot(
                projects: [Fixture.project("alpha"), Fixture.project("beta")],
                selectedProject: nil
            ) == .workspaceFiles
        )
    }
}

@Suite("normalizeRestoredWorkingRoot")
struct NormalizeRestoredWorkingRootTests {
    // The reported bug: `.workspaceFiles` is always valid, so a validity check
    // alone lets a root stored under two projects survive into the collapsed
    // shape and create work in a directory the user can no longer see.
    @Test("one project discards a restored workspace-files root")
    func singleShapeDiscardsWorkspaceFiles() {
        let alpha = Fixture.project("alpha")

        #expect(
            normalizeRestoredWorkingRoot(
                .workspaceFiles,
                projects: [alpha],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }

    @Test("one project discards a restored root naming a departed project")
    func singleShapeDiscardsDepartedProject() {
        let alpha = Fixture.project("alpha")
        let departed = Fixture.project("departed")

        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(departed),
                projects: [alpha],
                selectedProject: nil,
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }

    @Test("one project keeps a restored root that already is the implied root")
    func singleShapeKeepsImplied() {
        let alpha = Fixture.project("alpha")

        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(alpha),
                projects: [alpha],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }

    @Test("zero projects normalizes any restored project root to workspace files")
    func emptyShapeNormalizes() {
        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(Fixture.project("departed")),
                projects: [],
                selectedProject: nil,
                hasWorkspaceFiles: true
            ) == .workspaceFiles
        )
    }

    @Test("multiple projects honor a restored workspace-files root")
    func multipleShapeHonorsWorkspaceFiles() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")

        #expect(
            normalizeRestoredWorkingRoot(
                .workspaceFiles,
                projects: [alpha, beta],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == .workspaceFiles
        )
    }

    @Test("multiple projects honor a restored root naming a non-selected member")
    func multipleShapeHonorsOtherMember() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")

        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(beta),
                projects: [alpha, beta],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(beta)
        )
    }

    @Test("multiple projects replace a restored root that no longer resolves")
    func multipleShapeReplacesUnresolvable() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")

        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(Fixture.project("departed")),
                projects: [alpha, beta],
                selectedProject: Fixture.resolved(beta),
                hasWorkspaceFiles: true
            ) == Fixture.selection(beta)
        )
    }

    // A Workspace whose managed files directory did not resolve cannot honor a
    // root pointing at it, even in the shape that otherwise would.
    @Test("multiple projects replace a workspace-files root when there is no files directory")
    func multipleShapeWithoutWorkspaceFiles() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")

        #expect(
            normalizeRestoredWorkingRoot(
                .workspaceFiles,
                projects: [alpha, beta],
                selectedProject: Fixture.resolved(beta),
                hasWorkspaceFiles: false
            ) == Fixture.selection(beta)
        )
    }
}

/// Every 0↔1↔2+ transition, since the rule has to hold when a Workspace changes
/// shape and not only when state is first restored.
@Suite("working root across shape transitions")
struct ShapeTransitionTests {
    private let alpha = Fixture.project("alpha")
    private let beta = Fixture.project("beta")

    @Test("0 → 1: adding the first project re-roots the draft at it")
    func emptyToSingle() {
        let before = normalizeRestoredWorkingRoot(
            .workspaceFiles,
            projects: [],
            selectedProject: nil,
            hasWorkspaceFiles: true
        )
        #expect(before == .workspaceFiles)

        #expect(
            normalizeRestoredWorkingRoot(
                before,
                projects: [alpha],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }

    @Test("1 → 0: removing the last project re-roots the draft at workspace files")
    func singleToEmpty() {
        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(alpha),
                projects: [],
                selectedProject: nil,
                hasWorkspaceFiles: true
            ) == .workspaceFiles
        )
    }

    @Test("1 → 2: the existing project root is honored once a choice exists")
    func singleToMultiple() {
        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(alpha),
                projects: [alpha, beta],
                selectedProject: Fixture.resolved(beta),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }

    // A Workspace that drops from two projects to one must not keep a draft root
    // the collapsed shape can no longer offer.
    @Test("2 → 1: a now-unofferable draft root is discarded")
    func multipleToSingle() {
        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(beta),
                projects: [alpha],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )

        #expect(
            normalizeRestoredWorkingRoot(
                .workspaceFiles,
                projects: [alpha],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }

    @Test("2 → 0: everything collapses to workspace files")
    func multipleToEmpty() {
        #expect(
            normalizeRestoredWorkingRoot(
                Fixture.selection(beta),
                projects: [],
                selectedProject: nil,
                hasWorkspaceFiles: true
            ) == .workspaceFiles
        )
    }
}

/// The setter (`selectDraftWorkingRoot`) now runs through the same
/// normalization as restored state. That is only safe because
/// the chooser is gone in the collapsed shapes: while it was still on screen,
/// discarding an explicit pick would have made a visible control do nothing.
@Suite("normalization on an explicit pick")
struct SetterNormalizationTests {
    @Test("an explicit pick is honored wherever the chooser still exists")
    func multipleShapeHonorsExplicitPick() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")

        for pick in [WorkingRootSelection.workspaceFiles, Fixture.selection(beta)] {
            #expect(
                normalizeRestoredWorkingRoot(
                    pick,
                    projects: [alpha, beta],
                    selectedProject: Fixture.resolved(alpha),
                    hasWorkspaceFiles: true
                ) == pick
            )
        }
    }

    // No control offers this, so a pick arriving here is stale state routing
    // around the implied root rather than an intent to honor.
    @Test("a pick that reaches a collapsed shape is discarded")
    func collapsedShapeDiscardsPick() {
        let alpha = Fixture.project("alpha")

        #expect(
            normalizeRestoredWorkingRoot(
                .workspaceFiles,
                projects: [alpha],
                selectedProject: Fixture.resolved(alpha),
                hasWorkspaceFiles: true
            ) == Fixture.selection(alpha)
        )
    }
}

@Suite("isResolvable")
struct IsResolvableTests {
    @Test("workspace files always resolves")
    func workspaceFilesAlwaysResolves() {
        #expect(isResolvable(.workspaceFiles, projects: [], hasWorkspaceFiles: true))
    }

    @Test("a project root resolves only while the workspace still holds it")
    func projectResolution() {
        let alpha = Fixture.project("alpha")

        #expect(isResolvable(Fixture.selection(alpha), projects: [alpha], hasWorkspaceFiles: true))
        #expect(!isResolvable(Fixture.selection(alpha), projects: [], hasWorkspaceFiles: true))
    }
}
