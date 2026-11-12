#if DEBUG
import OttamBackendClient
import Foundation

@MainActor
enum UITestWorkspaceLauncher {
    static func launchIfRequested(
        runtime: OttamAppRuntime,
        openWorkspace: (WorkspaceID) -> Void,
        openWorkspaceWindow: (WorkspaceID) -> Void,
        closeChooser: () -> Void
    ) async -> String? {
        guard UITestPreparation.needsWorkspacePreparation else { return nil }
        do {
            let workspaceIDs = try await createWorkspaceIDs(runtime: runtime)
            guard let primaryWorkspaceID = workspaceIDs.first else {
                throw WorkspaceLaunchError.noWorkspace
            }
            UITestPreparation.workspaceLaunchSucceeded(
                primaryWorkspaceID: primaryWorkspaceID,
                workspaceIDs: workspaceIDs
            )
            if workspaceIDs.count == 1 {
                openWorkspace(primaryWorkspaceID)
            } else {
                await openAndVerifyWorkspaceWindows(
                    workspaceIDs,
                    runtime: runtime,
                    openWorkspaceWindow: openWorkspaceWindow
                )
                closeChooser()
            }
            return nil
        } catch {
            let message = String(describing: error)
            UITestPreparation.workspaceLaunchFailed(message: message)
            return message
        }
    }

    private static func createWorkspaceIDs(
        runtime: OttamAppRuntime
    ) async throws -> [WorkspaceID] {
        if UITestPreparation.createsEmptyWorkspace {
            let detail = try await runtime.createWorkspace(displayName: "Empty Workspace")
            return [detail.workspace.workspaceID]
        }
        let result = try await runtime.openProject(
            path: ReviewEnvironment.defaultWorkingDirectory
        )
        guard let firstWorkspace = result.createdWorkspace ?? result.workspaces.first else {
            throw WorkspaceLaunchError.noWorkspace
        }
        // Orthogonal to the workspace-count loop below: this seeds the
        // multi-*project* sidebar shape (several projects, one Workspace),
        // not the multi-*window* shape (several Workspaces, one project
        // each).
        for path in UITestPreparation.additionalProjectPaths {
            _ = try await runtime.addProject(
                workspaceID: firstWorkspace.workspaceID,
                path: path
            )
        }
        var workspaceIDs = [firstWorkspace.workspaceID]
        while workspaceIDs.count < UITestPreparation.requestedWorkspaceCount {
            let created = try await runtime.createWorkspace(
                displayName: "UI Test Workspace \(workspaceIDs.count + 1)"
            )
            _ = try await runtime.addProject(
                workspaceID: created.workspace.workspaceID,
                path: ReviewEnvironment.defaultWorkingDirectory
            )
            workspaceIDs.append(created.workspace.workspaceID)
        }
        return workspaceIDs
    }

    private static func openAndVerifyWorkspaceWindows(
        _ workspaceIDs: [WorkspaceID],
        runtime: OttamAppRuntime,
        openWorkspaceWindow: (WorkspaceID) -> Void
    ) async {
        for workspaceID in workspaceIDs {
            openWorkspaceWindow(workspaceID)
        }
        for _ in 0..<80 {
            if runtime.windows.snapshots.count == workspaceIDs.count {
                break
            }
            try? await Task.sleep(for: .milliseconds(100))
        }
        let initial = runtime.windows.snapshots
        let primaryWorkspaceID = workspaceIDs[0]
        let firstWindowNumber = initial.first {
            $0.workspaceID == primaryWorkspaceID
        }?.windowNumber
        let focusedExistingWindow = runtime.windows.focus(primaryWorkspaceID)
        let afterFocus = runtime.windows.snapshots
        let preservedWindow = afterFocus.first {
            $0.workspaceID == primaryWorkspaceID
        }?.windowNumber == firstWindowNumber
        let passed = initial.count == workspaceIDs.count
            && afterFocus.count == workspaceIDs.count
            && focusedExistingWindow
            && preservedWindow

        UITestPreparation.publishFixture(
            name: "workspace-window-lifecycle",
            status: passed ? "passed" : "failed",
            fields: [
                "message": passed
                    ? "existing Workspace window focused without replacement"
                    : "Workspace window focus or identity verification failed",
                "expectedWindowCount": workspaceIDs.count,
                "initialWindows": windowEvidence(initial),
                "afterFocusWindows": windowEvidence(afterFocus),
                "focusedWorkspaceID": primaryWorkspaceID.rawValue,
                "focusedExistingWindow": focusedExistingWindow,
                "preservedWindowNumber": preservedWindow,
            ]
        )
    }

    private static func windowEvidence(
        _ snapshots: [WorkspaceWindowSnapshot]
    ) -> [[String: Any]] {
        snapshots.map {
            [
                "workspaceID": $0.workspaceID.rawValue,
                "windowNumber": $0.windowNumber,
                "title": $0.title,
                "isKeyWindow": $0.isKeyWindow,
            ]
        }
    }
}

private enum WorkspaceLaunchError: Error, CustomStringConvertible {
    case noWorkspace

    var description: String {
        switch self {
        case .noWorkspace:
            "UI-test workspace creation returned no Workspace"
        }
    }
}
#endif
