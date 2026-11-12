import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// A closed window must leave nothing subscribed to core: every handler the chat
/// model installed is released, including ones still landing when it closed.
@MainActor
@Suite("Chat backend handler lifetime")
struct ChatBackendHandlerLifetimeTests {
    private func makeBackend() -> DiffReviewModel {
        DiffReviewModel(client: OttamCoreProcessClient(
            executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")
        ))
    }

    /// Waits for the load against the missing core to give up, then a few turns.
    private func settle(_ model: ChatWorkspaceModel) async {
        let deadline = ContinuousClock.now + .seconds(3)
        while model.errorText == nil, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(10))
        }
        try? await Task.sleep(for: .milliseconds(100))
    }

    @Test("closing while configure is still installing leaves nothing registered")
    func closeDuringConfigure() async {
        let backend = makeBackend()
        let model = ChatWorkspaceModel(defaults: UserDefaults(suiteName: "chat-lifetime-\(UUID())")!)
        model.configure(
            workspaceID: WorkspaceID("lifetime"),
            workspaceFiles: nil,
            projects: [],
            projectSummary: nil,
            backend: backend
        )
        await model.uninstallBackendHandlers(backend: backend)
        await settle(model)
        #expect(backend.livePermissionRegistrations.isEmpty)
        #expect(backend.liveEventSubscriptions.isEmpty)
    }

    @Test("uninstalling after a full load releases every subscription, sign-in included")
    func uninstallAfterLoad() async {
        let backend = makeBackend()
        let model = ChatWorkspaceModel(defaults: UserDefaults(suiteName: "chat-lifetime-\(UUID())")!)
        model.configure(
            workspaceID: WorkspaceID("lifetime"),
            workspaceFiles: nil,
            projects: [],
            projectSummary: nil,
            backend: backend
        )
        await model.installBackendHandlers(backend: backend)
        await model.loadIfNeeded(backend: backend)
        await settle(model)
        #expect(!backend.liveEventSubscriptions.isEmpty)
        await model.uninstallBackendHandlers(backend: backend)
        await settle(model)
        #expect(backend.livePermissionRegistrations.isEmpty)
        #expect(backend.liveEventSubscriptions.isEmpty)
    }
}
