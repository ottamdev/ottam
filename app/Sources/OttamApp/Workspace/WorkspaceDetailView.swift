import OttamAccessibility
import SwiftUI

struct WorkspaceDetailView: View {
    @Bindable var workspace: WorkspaceStore
    @Bindable var diffModel: DiffReviewModel
    @Bindable var chatWorkspace: ChatWorkspaceModel
    let addProject: () -> Void

    var body: some View {
        ChatWorkspaceView(
            workspace: workspace,
            model: chatWorkspace,
            backend: diffModel,
            addProject: addProject
        )
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.workspaceCanvas)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.workspaceDetail)
        .accessibilityLabel("Workspace detail")
    }
}

struct ProjectUnavailableWorkspaceView: View {
    let projectOpenError: String
    let projectOpenPath: String?
    let isOpeningProject: Bool
    let chooseWorkspace: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label("Project Unavailable", systemImage: "folder.badge.questionmark")
        } description: {
            VStack(spacing: 8) {
                Text(projectOpenError)
                if let projectOpenPath {
                    Text(projectOpenPath)
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
            }
        } actions: {
            Button("Choose Project…", action: chooseWorkspace)
                .disabled(isOpeningProject)
        }
    }
}
