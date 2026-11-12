import OttamBackendClient
import Observation

@MainActor
@Observable
final class WorkspaceStore {
    let workspaceID: WorkspaceID
    private(set) var workspaceSummary: WorkspaceSummary?
    private(set) var workspaceFiles: WorkingRootSummary?
    private(set) var projects: [WorkspaceProjectSummary]
    private(set) var selectedProjectID: ProjectID?
    private(set) var projectSummary: ResolvedProject?
    private(set) var isOpeningProject: Bool
    private(set) var projectOpenError: String?
    private(set) var projectOpenPath: String?

    init(
        workspaceID: WorkspaceID,
        workspaceSummary: WorkspaceSummary? = nil,
        projects: [WorkspaceProjectSummary] = [],
        selectedProjectID: ProjectID? = nil,
        projectSummary: ResolvedProject? = nil,
        isOpeningProject: Bool = false,
        projectOpenError: String? = nil,
        projectOpenPath: String? = nil
    ) {
        self.workspaceID = workspaceID
        self.workspaceSummary = workspaceSummary
        self.projects = projects
        self.selectedProjectID = selectedProjectID
        self.projectSummary = projectSummary
        self.isOpeningProject = isOpeningProject
        self.projectOpenError = projectOpenError
        self.projectOpenPath = projectOpenPath
    }

    func apply(detail: WorkspaceDetail, preferredProjectID: ProjectID? = nil) {
        workspaceSummary = detail.workspace
        workspaceFiles = detail.workspaceFiles
        projects = detail.projects.sorted {
            ($0.sortOrder, $0.project.projectID.rawValue)
                < ($1.sortOrder, $1.project.projectID.rawValue)
        }
        let preferred = preferredProjectID ?? selectedProjectID
        let selected = projects.first { $0.project.projectID == preferred } ?? projects.first
        selectedProjectID = selected?.project.projectID
        if let selected {
            apply(projectSummary: selected.resolvedProject)
        } else {
            projectSummary = nil
            projectOpenPath = nil
            isOpeningProject = false
            projectOpenError = nil
        }
    }

    func beginOpeningProject(path: String) {
        projectOpenPath = path
        isOpeningProject = true
        projectOpenError = nil
    }

    func apply(projectSummary: ResolvedProject) {
        self.projectSummary = projectSummary
        projectOpenPath = projectSummary.sourcePathForDisplay.isEmpty
            ? projectSummary.canonicalRoot
            : projectSummary.sourcePathForDisplay
        isOpeningProject = false
        projectOpenError = nil
    }

    func failProjectOpen(_ message: String) {
        projectSummary = nil
        isOpeningProject = false
        projectOpenError = message
    }
}
