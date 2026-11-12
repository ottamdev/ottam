import OttamAccessibility
import OttamBackendClient
import OttamWorkspaceLogic
import Darwin
import SwiftUI

struct WorkspaceChooserView: View {
    /// One fixed column, like Xcode's welcome window. The size holds through
    /// startup, failure, recovery and an empty workspace list.
    static let windowSize = CGSize(width: 480, height: 640)

    @Environment(\.dismissWindow) private var dismissWindow
    let runtime: OttamAppRuntime
    @State private var selection: WorkspaceID?
    @State private var archivedExpanded = false
    @State private var showingNewWorkspace = false
    @State private var newWorkspaceName = ""
    @State private var folderWorkspaceChoices: [WorkspaceSummary] = []
    @State private var pendingFolderPath: String?
    @State private var showingFolderWorkspaceChoices = false
    @State private var errorText: String?

    private var stateContent: some View {
        Group {
            if case .ready = runtime.connectionState {
                chooserContent
            } else {
                WorkspaceChooserRecoveryView(
                    state: runtime.connectionState,
                    exitError: runtime.coreExitError,
                    retry: runtime.retryConnection
                )
                .appTypographyRoot(TypographySettings.shared.resolved)
            }
        }
        .frame(width: Self.windowSize.width, height: Self.windowSize.height)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.workspaceChooser)
    }

    private var chooserContent: some View {
        VStack(spacing: 0) {
            WorkspaceWelcomeHeader()
            HStack(spacing: 8) {
                WorkspaceWelcomeAction("New Workspace…") {
                    newWorkspaceName = ""
                    showingNewWorkspace = true
                }
                .accessibilityIdentifier(AccessibilityID.chooserNewWorkspace)
                WorkspaceWelcomeAction("Open Git Folder…", action: openFolder)
            }
            .padding(.top, 22)
            WorkspaceListPane(
                connectionState: runtime.connectionState,
                activeWorkspaces: runtime.activeWorkspaces,
                archivedWorkspaces: runtime.archivedWorkspaces,
                openWorkspace: openWorkspace,
                restoreAndOpen: restoreAndOpen,
                selection: $selection,
                archivedExpanded: $archivedExpanded
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.top, 16)
        }
        .padding(.horizontal, 60)
        .padding(.top, 36)
        .padding(.bottom, 48)
        .background {
            Color.clear
                .contentShape(Rectangle())
                .gesture(WindowDragGesture())
                .allowsWindowActivationEvents(true)
        }
    }

    var body: some View {
        stateContent
        .task {
            await runtime.start()
#if DEBUG
            await UITestPreparation.prepareChooserIfRequested()
            errorText = await UITestWorkspaceLauncher.launchIfRequested(
                runtime: runtime,
                openWorkspace: openWorkspace,
                openWorkspaceWindow: { workspaceID in
                    runtime.windows.open(workspaceID, runtime: runtime)
                },
                closeChooser: {
                    runtime.windows.closeChooser()
                    dismissWindow(id: "workspaceChooser")
                }
            )
#endif
        }
        .sheet(isPresented: $showingNewWorkspace) {
            newWorkspaceSheet
        }
        .sheet(isPresented: $showingFolderWorkspaceChoices) {
            folderWorkspaceSheet
        }
        .alert("Workspace Error", isPresented: errorPresented) {
            Button("OK", role: .cancel) { errorText = nil }
        } message: {
            Text(errorText ?? "")
        }
        .background {
            WelcomeWindowProbe(registry: runtime.windows)
                .frame(width: 0, height: 0)
        }
    }

    private var newWorkspaceSheet: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("New Workspace")
                .font(.title2.weight(.semibold))
            TextField("Workspace Name", text: $newWorkspaceName)
                .textFieldStyle(.roundedBorder)
                .onSubmit(createWorkspace)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { showingNewWorkspace = false }
                Button("Create", action: createWorkspace)
                    .buttonStyle(.borderedProminent)
                    .disabled(newWorkspaceName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(24)
        .frame(width: 390)
    }

    private var folderWorkspaceSheet: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Choose a Workspace")
                .font(.title2.weight(.semibold))
            Text("This Project already belongs to more than one Workspace.")
                .foregroundStyle(.secondary)
            ForEach(folderWorkspaceChoices) { workspace in
                Button {
                    if workspace.archivedAt != nil {
                        restoreAndOpen(workspace.workspaceID)
                    } else {
                        openWorkspace(workspace.workspaceID)
                    }
                    showingFolderWorkspaceChoices = false
                } label: {
                    HStack {
                        WorkspaceChooserRow(
                            workspace: workspace,
                            projectPaths: listedProjectPaths(of: workspace.workspaceID)
                        )
                        Spacer()
                        if workspace.archivedAt != nil {
                            Text("Restore")
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
            if let pendingFolderPath {
                Button {
                    createWorkspace(forFolder: pendingFolderPath)
                } label: {
                    Label("Add to a New Workspace…", systemImage: "plus.square")
                }
                .buttonStyle(.bordered)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) {
                    showingFolderWorkspaceChoices = false
                }
            }
        }
        .padding(24)
        .frame(width: 440)
    }

    private var errorPresented: Binding<Bool> {
        Binding(
            get: { errorText != nil },
            set: { if !$0 { errorText = nil } }
        )
    }

    private func createWorkspace() {
        let name = newWorkspaceName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        Task {
            do {
                let detail = try await runtime.createWorkspace(displayName: name)
                showingNewWorkspace = false
                openWorkspace(detail.workspace.workspaceID)
            } catch {
                errorText = error.localizedDescription
            }
        }
    }

    /// Opening a folder answers with summaries, which carry no paths; the
    /// list refreshed by that same call does.
    private func listedProjectPaths(of workspaceID: WorkspaceID) -> [String]? {
        runtime.workspaces.first { $0.workspace.workspaceID == workspaceID }?.projectPaths
    }

    private func openFolder() {
        guard let path = ProjectFolderPicker.chooseGitBackedWorkspace() else { return }
        Task {
            do {
                let result = try await runtime.openProject(path: path)
                let active = result.workspaces.filter { $0.archivedAt == nil }
                if let created = result.createdWorkspace {
                    openWorkspace(created.workspaceID)
                } else if active.count == 1, let workspace = active.first {
                    openWorkspace(workspace.workspaceID)
                } else {
                    pendingFolderPath = path
                    folderWorkspaceChoices = result.workspaces
                    showingFolderWorkspaceChoices = true
                }
            } catch {
                errorText = error.localizedDescription
            }
        }
    }

    private func createWorkspace(forFolder path: String) {
        Task {
            do {
                let suggestedName = URL(fileURLWithPath: path).lastPathComponent
                let detail = try await runtime.createWorkspace(displayName: suggestedName)
                _ = try await runtime.addProject(
                    workspaceID: detail.workspace.workspaceID,
                    path: path
                )
                pendingFolderPath = nil
                showingFolderWorkspaceChoices = false
                openWorkspace(detail.workspace.workspaceID)
            } catch {
                errorText = error.localizedDescription
            }
        }
    }

    private func restoreAndOpen(_ workspaceID: WorkspaceID) {
        Task {
            do {
                try await runtime.restoreWorkspace(workspaceID)
                openWorkspace(workspaceID)
            } catch {
                errorText = error.localizedDescription
            }
        }
    }

    private func openWorkspace(_ workspaceID: WorkspaceID) {
        runtime.windows.open(workspaceID, runtime: runtime)
        runtime.windows.closeChooser()
        dismissWindow(id: "workspaceChooser")
    }

}

private struct WorkspaceWelcomeHeader: View {
    /// Releases are stamped `YY.M.N` by `build-app.sh --version`. Every other
    /// build keeps the plist's `0.x` placeholder, which says nothing about
    /// what it was built from.
    private var versionLine: String {
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String
        guard let version,
              let year = version.split(separator: ".").first.flatMap({ Int($0) }),
              year > 0
        else { return "Development" }
        return "Version \(version)"
    }

    private var markMotion: OttamThreadMark.Motion {
        #if DEBUG
        // Captures must not land mid-draw.
        if UITestPreparation.isActive { return .still }
        #endif
        return .flow(hold: 10)
    }

    var body: some View {
        VStack(spacing: 0) {
            OttamThreadMark(motion: markMotion)
                .frame(width: 128, height: 128)
                // The mark's box is clear above and below the thread; tuck
                // the neighbours in so spacing is measured from the thread.
                .padding(.vertical, -20)
            // The stacked lockup's proportions: the wordmark is a caption at
            // about a quarter of the mark's scale.
            OttamWordmark()
                .frame(height: 25)
                .padding(.top, 4)
            Text(versionLine)
                .font(.body)
                .foregroundStyle(.secondary)
                .padding(.top, 8)
        }
    }
}

private struct WorkspaceListPane: View {
    let connectionState: OttamAppRuntime.ConnectionState
    let activeWorkspaces: [WorkspaceListItem]
    let archivedWorkspaces: [WorkspaceListItem]
    let openWorkspace: (WorkspaceID) -> Void
    let restoreAndOpen: (WorkspaceID) -> Void
    @Binding var selection: WorkspaceID?
    @Binding var archivedExpanded: Bool

    var body: some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(.fill.quaternary, in: .rect(cornerRadius: 16))
            .clipShape(.rect(cornerRadius: 16))
    }

    @ViewBuilder
    private var content: some View {
        if activeWorkspaces.isEmpty && archivedWorkspaces.isEmpty, case .starting = connectionState {
            ProgressView("Starting Core…")
        } else if activeWorkspaces.isEmpty && archivedWorkspaces.isEmpty {
            ContentUnavailableView(
                "No Workspaces",
                systemImage: "square.grid.2x2",
                description: Text("Create a Workspace or open a Git folder.")
            )
        } else {
            List(selection: $selection) {
                ForEach(activeWorkspaces) { item in
                    WorkspaceChooserRow(workspace: item.workspace, projectPaths: item.projectPaths)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .listRowSeparator(.hidden)
                        .tag(item.workspace.workspaceID)
                }
                if !archivedWorkspaces.isEmpty {
                    DisclosureGroup("Archived", isExpanded: $archivedExpanded) {
                        ForEach(archivedWorkspaces) { item in
                            HStack {
                                WorkspaceChooserRow(workspace: item.workspace, projectPaths: item.projectPaths)
                                Spacer()
                                Button("Restore") {
                                    restoreAndOpen(item.workspace.workspaceID)
                                }
                                .buttonStyle(.borderless)
                            }
                        }
                    }
                }
            }
            .listStyle(.inset)
            .scrollContentBackground(.hidden)
            // The list's own primary action: double-click, Return, and the
            // row's accessibility press. Only active rows carry a tag, so an
            // archived one cannot arrive here.
            .contextMenu(forSelectionType: WorkspaceID.self) { _ in
            } primaryAction: { workspaceIDs in
                if let workspaceID = workspaceIDs.first {
                    openWorkspace(workspaceID)
                }
            }
        }
    }
}

private struct WorkspaceWelcomeAction: View {
    let title: String
    let action: () -> Void

    init(_ title: String, action: @escaping () -> Void) {
        self.title = title
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            Text(title)
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(.capsule)
        .controlSize(.extraLarge)
    }
}

private struct WorkspaceChooserRow: View {
    let workspace: WorkspaceSummary
    /// `nil` when the list has no row for this Workspace; the line keeps its
    /// height.
    let projectPaths: [String]?

    /// The tint picked in the Workspace's sidebar, so the disc matches there.
    @AppStorage private var storedTint: String

    init(workspace: WorkspaceSummary, projectPaths: [String]?) {
        self.workspace = workspace
        self.projectPaths = projectPaths
        _storedTint = AppStorage(
            wrappedValue: "",
            WorkspaceMonogramTint.storageKey(for: workspace.workspaceID)
        )
    }

    private var tint: WorkspaceMonogramTint {
        WorkspaceMonogramTint.stored(storedTint) ?? .derived(fromName: workspace.displayName)
    }

    var body: some View {
        HStack(spacing: 10) {
            WorkspaceMonogram(
                name: workspace.displayName,
                color: tint.color,
                diameter: 28,
                glyph: .title
            )
            VStack(alignment: .leading, spacing: 1) {
                Text(workspace.displayName)
                    .font(.headline)
                Text(projectPaths.map(workspaceFolderSummary(projectPaths:)) ?? " ")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
        }
        .padding(.vertical, 2)
        .help(lastOpenedText)
    }

    private var lastOpenedText: String {
        let date = Date(timeIntervalSince1970: TimeInterval(workspace.lastOpenedAt) / 1_000)
        return "Last opened \(date.formatted(.relative(presentation: .named)))"
    }
}
