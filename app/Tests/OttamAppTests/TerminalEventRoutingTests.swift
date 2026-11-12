import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// Terminal output reaches core's subscribers once per workspace, not once per
/// open terminal: each extra subscription decodes and drops every other
/// terminal's output.
@MainActor
@Suite("Terminal event routing")
struct TerminalEventRoutingTests {
    @Test("one workspace subscription serves every terminal, and closing releases it")
    func oneSubscriptionPerWorkspace() async {
        let backend = DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
        let workspace = TerminalWorkspaceModel()
        workspace.configure(
            workspaceID: WorkspaceID("terminals"),
            workspaceFiles: WorkingRootSummary(
                kind: .workspaceFiles,
                displayName: "Files",
                canonicalPath: "/tmp",
                projectID: nil,
                sourceID: nil,
                availability: .available
            ),
            projects: [],
            backend: backend
        )
        var sessions: [TerminalSessionModel] = []
        for _ in 0..<3 {
            let record = workspace.createTerminal(at: .workspaceFiles, backend: backend)
            let session = record.flatMap { workspace.sessionModel(for: $0.id) }
            session?.startIfNeeded(cwd: "/tmp", title: "Terminal")
            if let session { sessions.append(session) }
        }
        #expect(sessions.count == 3)

        // Each open subscribes first, then fails against the missing core. The
        // full suite can starve the main actor for seconds; wait it out.
        let deadline = ContinuousClock.now + .seconds(30)
        while sessions.contains(where: { $0.errorText == nil }), ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(10))
        }
        #expect(backend.liveEventSubscriptions.count == 1)

        workspace.closeAll(backend: backend)
        try? await Task.sleep(for: .milliseconds(100))
        #expect(backend.liveEventSubscriptions.isEmpty)
    }
}
