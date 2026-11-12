import OttamBackendClient
import Testing

@testable import OttamWorkspaceLogic

@Suite("SidebarSectionBuilder")
struct SidebarSectionBuilderTests {
    private func sections(
        projects: [WorkspaceProjectSummary],
        selectedProject: ResolvedProject? = nil,
        conversations: [ConversationSummary]
    ) -> [SidebarSection] {
        SidebarSectionBuilder.sections(
            projects: projects,
            selectedProject: selectedProject,
            conversations: conversations,
            attentionConversationIDs: [],
            runningConversationIDs: []
        )
    }

    @Test("no projects puts every conversation in one flat list")
    func zeroProjects() {
        let result = sections(
            projects: [],
            conversations: [Fixture.workspaceFilesConversation("scratch")]
        )

        #expect(result.count == 1)
        #expect(result[0].kind == .all)
        #expect(result[0].allItems.count == 1)
    }

    @Test("no conversations produces no sections at all")
    func emptyHistory() {
        #expect(sections(projects: [Fixture.project("alpha")], conversations: []).isEmpty)
    }

    @Test("one project produces a single flat section")
    func singleProject() {
        let alpha = Fixture.project("alpha")
        let result = sections(
            projects: [alpha],
            conversations: [
                Fixture.conversation("spec", in: alpha),
                Fixture.conversation("bug", in: alpha),
            ]
        )

        #expect(result.count == 1)
        #expect(result[0].kind == .all)
        #expect(result[0].allItems.count == 2)
    }

    // The inverse of what this builder used to do. Splitting a one-project
    // history across a project header and a "Chats" bucket is the symptom the
    // single-project-workspace spec opens with; the flat list is the fix, and
    // this test is the regression signal if sections ever come back.
    @Test("one project plus a workspace-files chat stay in one list")
    func singleProjectWithLooseChat() {
        let alpha = Fixture.project("alpha")
        let result = sections(
            projects: [alpha],
            conversations: [
                Fixture.conversation("spec", in: alpha),
                Fixture.workspaceFilesConversation("scratch"),
            ]
        )

        #expect(result.map(\.kind) == [.all])
        #expect(result[0].allItems.map(\.summary.title) == ["spec", "scratch"])
    }

    @Test("multiple projects each get a section, in workspace order")
    func multipleProjects() {
        let alpha = Fixture.project("alpha", sortOrder: 0)
        let beta = Fixture.project("beta", sortOrder: 1)
        let result = sections(
            projects: [alpha, beta],
            conversations: [
                Fixture.conversation("beta-work", in: beta),
                Fixture.conversation("alpha-work", in: alpha),
            ]
        )

        #expect(
            result.map(\.kind) == [
                .project(alpha.project.projectID),
                .project(beta.project.projectID),
            ]
        )
    }

    @Test("a project with no conversations is skipped rather than shown empty")
    func emptyProjectSectionSkipped() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")
        let result = sections(
            projects: [alpha, beta],
            conversations: [Fixture.conversation("alpha-work", in: alpha)]
        )

        #expect(result.map(\.kind) == [.project(alpha.project.projectID)])
    }

    // Nothing disappears in either shape: a conversation whose project left the
    // Workspace stays in the flat list, and falls into the loose bucket once
    // sections return.
    @Test("a conversation whose project left the workspace survives in the flat list")
    func departedProjectSurvivesFlat() {
        let alpha = Fixture.project("alpha")
        let result = sections(
            projects: [alpha],
            conversations: [
                Fixture.conversation("alpha-work", in: alpha),
                Fixture.departedProjectConversation("orphan"),
            ]
        )

        #expect(result.map(\.kind) == [.all])
        #expect(result[0].allItems.map(\.summary.title) == ["alpha-work", "orphan"])
    }

    @Test("a conversation whose project left the workspace falls into the loose bucket")
    func departedProjectFallsBackToLoose() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")
        let result = sections(
            projects: [alpha, beta],
            conversations: [
                Fixture.conversation("alpha-work", in: alpha),
                Fixture.departedProjectConversation("orphan"),
            ]
        )

        #expect(result.count == 2)
        #expect(result[1].kind == .looseChats)
        #expect(result[1].allItems.map(\.summary.title) == ["orphan"])
    }

    @Test("recency order is preserved within a section")
    func recencyOrderPreserved() {
        let alpha = Fixture.project("alpha")
        let result = sections(
            projects: [alpha],
            conversations: [
                Fixture.conversation("newest", in: alpha),
                Fixture.conversation("middle", in: alpha),
                Fixture.conversation("oldest", in: alpha),
            ]
        )

        #expect(result[0].allItems.map(\.summary.title) == ["newest", "middle", "oldest"])
    }

    @Test("attention and running flags are resolved onto the right rows")
    func liveStatusResolved() {
        let alpha = Fixture.project("alpha")
        let busy = Fixture.conversation("busy", in: alpha)
        let idle = Fixture.conversation("idle", in: alpha)

        let result = SidebarSectionBuilder.sections(
            projects: [alpha],
            selectedProject: Fixture.resolved(alpha),
            conversations: [busy, idle],
            attentionConversationIDs: [busy.conversationID],
            runningConversationIDs: [busy.conversationID]
        )

        let rows = result[0].allItems
        #expect(rows[0].needsAttention && rows[0].isRunning)
        #expect(!rows[1].needsAttention && !rows[1].isRunning)
    }

    @Test("collapse-state persistence keys are stable and distinct")
    func persistenceKeys() {
        #expect(SidebarSectionKind.all.persistenceKey == "all")
        #expect(SidebarSectionKind.looseChats.persistenceKey == "chats")
        #expect(SidebarSectionKind.project(ProjectID("abc")).persistenceKey == "project:abc")
    }

    // The flat list replaces what used to be two sections, so reusing the
    // per-section cap would halve the visible history in the shape this layout
    // is meant to privilege.
    @Test("the flat list shows more rows before folding than a per-project section")
    func visibleCapPerKind() {
        #expect(SidebarSectionBuilder.visibleCap(for: .all) == 12)
        #expect(SidebarSectionBuilder.visibleCap(for: .project(ProjectID("abc"))) == 5)
        #expect(SidebarSectionBuilder.visibleCap(for: .looseChats) == 5)
        #expect(
            SidebarSectionBuilder.visibleCap(for: .all)
                > SidebarSectionBuilder.visibleCap(for: .looseChats)
        )
    }
}

/// The mark that tells a row apart when the layout around it does not say where
/// it lives. Its trigger differs by shape, which is why it is resolved in the
/// builder rather than the view.
@Suite("root-mismatch mark")
struct RootMismatchMarkTests {
    @Test("one project marks only the rows not rooted at it")
    func singleShapeMarksNonImplied() {
        let alpha = Fixture.project("alpha")
        let result = SidebarSectionBuilder.sections(
            projects: [alpha],
            selectedProject: Fixture.resolved(alpha),
            conversations: [
                Fixture.conversation("spec", in: alpha),
                Fixture.workspaceFilesConversation("scratch"),
                Fixture.departedProjectConversation("orphan"),
            ],
            attentionConversationIDs: [],
            runningConversationIDs: []
        )

        #expect(
            result[0].allItems.map(\.rootMark)
                == [nil, .workspaceFiles, .detachedProject(name: "departed")]
        )
    }

    // With no projects the implied root *is* workspace files, so the ordinary
    // row carries no mark and only the stranded one does.
    @Test("zero projects mark only what is not rooted at workspace files")
    func emptyShapeMarksNonImplied() {
        let result = SidebarSectionBuilder.sections(
            projects: [],
            selectedProject: nil,
            conversations: [
                Fixture.workspaceFilesConversation("scratch"),
                Fixture.departedProjectConversation("orphan"),
            ],
            attentionConversationIDs: [],
            runningConversationIDs: []
        )

        #expect(
            result[0].allItems.map(\.rootMark)
                == [nil, .detachedProject(name: "departed")]
        )
    }

    // Per-project headers already say where their rows live, so those rows stay
    // unmarked no matter which project is selected; only the loose bucket, which
    // names nothing, carries roots.
    @Test("multiple projects mark only the loose bucket")
    func multipleShapeMarksOnlyLoose() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")
        let result = SidebarSectionBuilder.sections(
            projects: [alpha, beta],
            selectedProject: Fixture.resolved(alpha),
            conversations: [
                Fixture.conversation("alpha-work", in: alpha),
                Fixture.conversation("beta-work", in: beta),
                Fixture.workspaceFilesConversation("scratch"),
            ],
            attentionConversationIDs: [],
            runningConversationIDs: []
        )

        #expect(result[0].allItems.allSatisfy { $0.rootMark == nil })
        #expect(result[1].allItems.allSatisfy { $0.rootMark == nil })
        #expect(result[2].allItems.map(\.rootMark) == [.workspaceFiles])
    }

    // The two marks have to stay distinguishable in the same bucket — that is
    // the whole reason the classification is an enum rather than a flag.
    @Test("the loose bucket tells managed files apart from a departed project")
    func looseBucketDistinguishesMarks() {
        let alpha = Fixture.project("alpha")
        let beta = Fixture.project("beta")
        let result = SidebarSectionBuilder.sections(
            projects: [alpha, beta],
            selectedProject: Fixture.resolved(alpha),
            conversations: [
                Fixture.conversation("alpha-work", in: alpha),
                Fixture.workspaceFilesConversation("scratch"),
                Fixture.departedProjectConversation("orphan"),
            ],
            attentionConversationIDs: [],
            runningConversationIDs: []
        )

        #expect(
            result[1].allItems.map(\.rootMark)
                == [.workspaceFiles, .detachedProject(name: "departed")]
        )
    }
}

@Suite("WorkingRootSummary.matches")
struct WorkingRootMatchesTests {
    @Test("a stored root matches the selection naming the same place")
    func identityMatch() {
        let alpha = Fixture.project("alpha")
        let root = Fixture.conversation("spec", in: alpha).workingRoot

        #expect(root.matches(Fixture.selection(alpha)))
        #expect(!root.matches(.workspaceFiles))
    }

    // Compared by identity, not availability — that is what lets the sidebar
    // keep a departed project's row and mark it rather than drop it.
    @Test("a detached root still matches the selection naming it")
    func detachedStillMatches() {
        let root = Fixture.departedProjectConversation("orphan").workingRoot

        #expect(root.availability == .detached)
        #expect(
            root.matches(
                .project(
                    projectID: ProjectID("project-departed"),
                    sourceID: ProjectSourceID("source-departed")
                )
            )
        )
    }

    @Test("a workspace-files root matches only workspace files")
    func workspaceFilesMatch() {
        let root = Fixture.workspaceFilesConversation("scratch").workingRoot

        #expect(root.matches(.workspaceFiles))
        #expect(!root.matches(Fixture.selection(Fixture.project("alpha"))))
    }

    @Test("a delegated child nests under its listed parent and nowhere else")
    func delegatedChildrenNest() {
        let parent = Fixture.workspaceFilesConversation("parent")
        var child = Fixture.workspaceFilesConversation("review-bot: review")
        child.parentConversationID = parent.conversationID
        var orphan = Fixture.workspaceFilesConversation("orphan")
        orphan.parentConversationID = ConversationID("conversation-deleted")
        let result = SidebarSectionBuilder.sections(
            projects: [],
            selectedProject: nil,
            conversations: [child, parent, orphan],
            attentionConversationIDs: [],
            runningConversationIDs: []
        )

        #expect(result[0].allItems.map(\.summary.title) == ["parent", "orphan"])
        #expect(result[0].allItems[0].children.map(\.summary.title) == ["review-bot: review"])
        #expect(result[0].allItems[1].children.isEmpty)
    }

    private func family(
        attention: Bool = false,
        running: Bool = false
    ) -> (parent: SidebarConversationItem, child: ConversationID) {
        let parent = Fixture.workspaceFilesConversation("parent")
        var child = Fixture.workspaceFilesConversation("review-bot: review")
        child.parentConversationID = parent.conversationID
        let result = SidebarSectionBuilder.sections(
            projects: [],
            selectedProject: nil,
            conversations: [child, parent],
            attentionConversationIDs: attention ? [child.conversationID] : [],
            runningConversationIDs: running ? [child.conversationID] : []
        )
        return (result[0].allItems[0], child.conversationID)
    }

    @Test("a parent carries its children's approvals and work")
    func parentCarriesChildStatus() {
        let waiting = family(attention: true).parent
        #expect(!waiting.needsAttention)
        #expect(waiting.childNeedsAttention)
        #expect(waiting.attentionCount == 1)
        #expect(family(running: true).parent.childIsRunning)

        let quiet = family().parent
        #expect(!quiet.childNeedsAttention && !quiet.childIsRunning)
        #expect(quiet.attentionCount == 0)
    }

    @Test("a quiet parent stays past the cap when its child is selected, waiting or working")
    func parentStaysPastCapForChild() {
        let quiet = family()
        #expect(!quiet.parent.staysPastCap(selected: nil))
        #expect(quiet.parent.staysPastCap(selected: quiet.child))
        #expect(quiet.parent.staysPastCap(selected: quiet.parent.id))
        #expect(family(attention: true).parent.staysPastCap(selected: nil))
        #expect(family(running: true).parent.staysPastCap(selected: nil))
    }
}
