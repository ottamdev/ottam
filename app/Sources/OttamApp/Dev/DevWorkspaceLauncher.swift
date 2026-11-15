#if DEBUG
import Foundation
import OttamBackendClient

/// Isolated `run-app.sh` instances open their worktree's Workspace at launch
/// instead of waiting in the Welcome window: `OTTAM_DEV_OPEN_WORKSPACE=1`
/// opens `OTTAM_WORKSPACE`, creating its Workspace the way Open Git Folder
/// does. Once per process, so closing the window still reaches the chooser.
@MainActor
enum DevWorkspaceLauncher {
    private static var didRun = false

    /// The error to show, if opening failed.
    static func openIfRequested(
        runtime: OttamAppRuntime,
        openWorkspace: (WorkspaceID) -> Void,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) async -> String? {
        guard !didRun,
              environment["OTTAM_DEV_OPEN_WORKSPACE"] == "1",
              environment["OTTAM_UI_TEST_MODE"] != "1",
              let path = environment["OTTAM_WORKSPACE"], !path.isEmpty
        else { return nil }
        didRun = true
        // A restored window already put the developer somewhere.
        guard runtime.windows.openWorkspaceCount == 0 else { return nil }
        do {
            let result = try await runtime.openProject(path: path)
            let active = result.workspaces.filter { $0.archivedAt == nil }
            guard let workspace = result.createdWorkspace ?? active.first else { return nil }
            openWorkspace(workspace.workspaceID)
            return nil
        } catch {
            return error.localizedDescription
        }
    }
}
#endif
