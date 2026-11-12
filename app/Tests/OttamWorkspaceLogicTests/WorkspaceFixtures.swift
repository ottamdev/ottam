import OttamBackendClient

/// Minimal builders for the protocol values these rules operate on, so the tests
/// read as assertions rather than as construction. Only the fields the rules
/// actually branch on carry meaning; the rest are fixed filler.
enum Fixture {
    static func project(
        _ name: String,
        sortOrder: Int = 0
    ) -> WorkspaceProjectSummary {
        let projectID = ProjectID("project-\(name)")
        let sourceID = ProjectSourceID("source-\(name)")
        return WorkspaceProjectSummary(
            project: ProjectSummary(
                projectID: projectID,
                displayName: name,
                createdAt: 0,
                updatedAt: 0,
                lastOpenedAt: 0
            ),
            source: ProjectSourceSummary(
                projectSourceID: sourceID,
                projectID: projectID,
                kind: .localGit,
                displayName: name,
                canonicalPath: "/tmp/\(name)",
                createdAt: 0,
                updatedAt: 0
            ),
            sortOrder: sortOrder,
            addedAt: 0
        )
    }

    static func resolved(_ member: WorkspaceProjectSummary) -> ResolvedProject {
        ResolvedProject(project: member.project, source: member.source)
    }

    static func selection(_ member: WorkspaceProjectSummary) -> WorkingRootSelection {
        .project(
            projectID: member.project.projectID,
            sourceID: member.source.projectSourceID
        )
    }

    /// A conversation rooted at `member`'s project.
    static func conversation(
        _ title: String,
        in member: WorkspaceProjectSummary
    ) -> ConversationSummary {
        conversation(
            title,
            root: WorkingRootSummary(
                kind: .project,
                displayName: member.project.displayName,
                canonicalPath: member.source.canonicalPath,
                projectID: member.project.projectID,
                sourceID: member.source.projectSourceID,
                availability: .available
            )
        )
    }

    /// A conversation rooted at the managed Workspace files directory.
    static func workspaceFilesConversation(_ title: String) -> ConversationSummary {
        conversation(
            title,
            root: WorkingRootSummary(
                kind: .workspaceFiles,
                displayName: "Workspace files",
                canonicalPath: "/tmp/workspace/files",
                availability: .available
            )
        )
    }

    /// A conversation rooted at a project that is no longer a Workspace member.
    static func departedProjectConversation(_ title: String) -> ConversationSummary {
        conversation(
            title,
            root: WorkingRootSummary(
                kind: .project,
                displayName: "departed",
                canonicalPath: "/tmp/departed",
                projectID: ProjectID("project-departed"),
                sourceID: ProjectSourceID("source-departed"),
                availability: .detached
            )
        )
    }

    private static func conversation(
        _ title: String,
        root: WorkingRootSummary
    ) -> ConversationSummary {
        ConversationSummary(
            conversationID: ConversationID("conversation-\(title)"),
            workspaceID: WorkspaceID("workspace-1"),
            workingRoot: root,
            title: title,
            createdAt: "2026-07-27T00:00:00Z",
            updatedAt: "2026-07-27T00:00:00Z",
            alwaysAllowToolActions: false
        )
    }
}
