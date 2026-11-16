import Foundation
import OttamBackendClient
import Testing
@testable import OttamApp

@Suite("Composer focus handoff")
@MainActor
struct ComposerFocusHandoffTests {
    private func backend() -> DiffReviewModel {
        DiffReviewModel(client: OttamCoreProcessClient(executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core")))
    }

    @Test("the next composer to appear takes the handoff once")
    func takesOnce() {
        let model = ChatWorkspaceModel(transcriptCache: ConversationTranscriptCache(policy: .init()))
        #expect(!model.takeComposerFocusHandoff())
        model.handOffComposerFocus(true)
        #expect(model.takeComposerFocusHandoff())
        #expect(!model.takeComposerFocusHandoff())
    }

    @Test("picking a conversation or New Chat mid-send drops the handoff")
    func navigationDrops() {
        let model = ChatWorkspaceModel(transcriptCache: ConversationTranscriptCache(policy: .init()))
        let backend = backend()

        model.handOffComposerFocus(true)
        model.select(ConversationID("other"), backend: backend)
        #expect(!model.takeComposerFocusHandoff())

        model.handOffComposerFocus(true)
        model.beginNewConversation(backend: backend)
        #expect(!model.takeComposerFocusHandoff())
    }
}
