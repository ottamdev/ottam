import OttamBackendClient

/// The window subtitle under the route title: where the visible work runs and
/// on which branch — `ottam · main`. Every route resolves through this so
/// the toolbar reads the same way whichever route is showing.
///
/// Where the Workspace offers no root choice, its implied root is the only
/// home, so the subtitle drops the name and shows just the branch. The
/// sidebar footer already names the Workspace. Workspace files have no git,
/// so they get no subtitle and the title centers alone.
public enum WorkspaceContextSubtitle {
    /// - Parameters:
    ///   - rootName: the working root the route is showing; nil when it shows
    ///     none (no terminal selected, no project open).
    ///   - sourceID: that root's project source, for the branch lookup. Nil for
    ///     Workspace files.
    ///   - projects: the Workspace's projects; their summaries carry the branch
    ///     each source was on when last seen.
    ///   - loadedBranches: branches read from loaded diffs, keyed by source.
    ///     Fresher than the project summary, so they win.
    public static func resolve(
        rootName: String?,
        sourceID: ProjectSourceID?,
        projects: [WorkspaceProjectSummary],
        loadedBranches: [ProjectSourceID: String] = [:]
    ) -> String {
        guard let rootName = nonEmpty(rootName), let sourceID else { return "" }
        let branch = nonEmpty(loadedBranches[sourceID])
            ?? nonEmpty(projects.first { $0.source.projectSourceID == sourceID }?.source.currentBranch)
        let isImpliedRoot = WorkspaceShape.of(projects: projects) == .single
            && sourceID == projects.first?.source.projectSourceID
        return [isImpliedRoot ? nil : rootName, branch]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty
        else { return nil }
        return value
    }
}
