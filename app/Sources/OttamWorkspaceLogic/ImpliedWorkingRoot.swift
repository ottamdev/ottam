import Foundation
import OttamBackendClient

/// How many projects a Workspace holds, which is the only input that decides
/// whether the user has a working-root choice to make. Derived at render time —
/// a stored shape goes stale the moment a project is added or removed.
public enum WorkspaceShape: Equatable, Sendable {
    /// No projects. One obvious home: the managed Workspace files directory.
    case empty
    /// Exactly one project. One obvious home: that project.
    case single
    /// Two or more projects. A real decision, so the chooser stays.
    case multiple

    public static func of(projectCount: Int) -> WorkspaceShape {
        switch projectCount {
        case ..<1: .empty
        case 1: .single
        default: .multiple
        }
    }

    public static func of(projects: [WorkspaceProjectSummary]) -> WorkspaceShape {
        of(projectCount: projects.count)
    }

    /// True where there is no choice to offer, so the UI hides the chooser and
    /// a restored root that disagrees with the implied one is discarded.
    public var collapsesRootChooser: Bool {
        self != .multiple
    }
}

/// One line saying where a Workspace lives: the lone project's path, or a
/// count, since a single path would be wrong above several projects.
public func workspaceFolderSummary(projectPaths: [String]) -> String {
    switch WorkspaceShape.of(projectCount: projectPaths.count) {
    case .empty:
        return "No projects"
    case .single:
        guard let only = projectPaths.first else { return "No projects" }
        // Component-aware; a raw `hasPrefix` would turn /Users/ajax into ~ax.
        return (only as NSString).abbreviatingWithTildeInPath
    case .multiple:
        return "\(projectPaths.count) projects"
    }
}

/// The single working root that new work is created at, derived from the shape.
///
/// Every path that starts a Conversation or Terminal resolves through this, so
/// hiding the chooser in the collapsed shapes cannot be routed around by a
/// seeded literal, a restored scene value, or an entry point that decides for
/// itself.
public func impliedWorkingRoot(
    projects: [WorkspaceProjectSummary],
    selectedProject: ResolvedProject?
) -> WorkingRootSelection {
    switch WorkspaceShape.of(projects: projects) {
    case .empty:
        return .workspaceFiles
    case .single:
        // The lone project, regardless of what is currently selected — in this
        // shape there is nothing else the user could have meant.
        guard let only = projects.first else { return .workspaceFiles }
        return .project(
            projectID: only.project.projectID,
            sourceID: only.source.projectSourceID
        )
    case .multiple:
        guard let selected = selectedProject else { return .workspaceFiles }
        return .project(
            projectID: selected.projectID,
            sourceID: selected.projectSourceID
        )
    }
}

/// Normalizes a working root against the Workspace's current shape — one
/// restored with the window, seeded before configuration, left behind by a
/// project count change, or picked from the chooser.
///
/// A validity check alone is not enough: `.workspaceFiles` is always valid, so a
/// root stored while the Workspace had two projects would otherwise survive into
/// the collapsed shape and silently create work in a directory the user can no
/// longer see or choose.
///
/// Every path that sets a working root runs through this, including the setters.
/// That is safe only because the chooser is now removed in the collapsed shapes:
/// while it was still on screen, discarding an explicit pick of "Workspace
/// files" would have made a visible control silently do nothing.
/// - Parameter hasWorkspaceFiles: whether the Workspace's managed files
///   directory resolved. `.workspaceFiles` is not unconditionally valid — a
///   Workspace without one cannot honor a root pointing at it.
public func normalizeRestoredWorkingRoot(
    _ restored: WorkingRootSelection,
    projects: [WorkspaceProjectSummary],
    selectedProject: ResolvedProject?,
    hasWorkspaceFiles: Bool
) -> WorkingRootSelection {
    let implied = impliedWorkingRoot(projects: projects, selectedProject: selectedProject)
    let shape = WorkspaceShape.of(projects: projects)

    guard shape == .multiple else {
        // Collapsed shapes: anything that is not the implied root is discarded.
        return restored == implied ? restored : implied
    }

    // Multi-project: honor the restored root while it still resolves, as today.
    let resolvable = isResolvable(
        restored,
        projects: projects,
        hasWorkspaceFiles: hasWorkspaceFiles
    )
    return resolvable ? restored : implied
}

/// Whether a selection still names something this Workspace holds.
public func isResolvable(
    _ selection: WorkingRootSelection,
    projects: [WorkspaceProjectSummary],
    hasWorkspaceFiles: Bool
) -> Bool {
    switch selection {
    case .workspaceFiles:
        return hasWorkspaceFiles
    case .project(let projectID, let sourceID):
        return projects.contains {
            $0.project.projectID == projectID && $0.source.projectSourceID == sourceID
        }
    }
}
