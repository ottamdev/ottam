import OttamBackendClient
import OttamMarkdownEngine
import Testing
@testable import OttamApp

@Suite("Run failure recovery")
@MainActor
struct HostedRunFailureCardTests {
    @Test("quota explanation keeps the exact provider error available")
    func quotaMessage() {
        let failure = ChatRunFailurePresentation(runID: .init("run"), reason: "Internal error: Go usage limit exceeded")
        #expect(failure.explanation == "OpenCode Go usage limit exceeded.")
        #expect(failure.providerMessage == "Internal error: Go usage limit exceeded")
        let unknown = ChatRunFailurePresentation(runID: .init("run"), reason: "Provider supplied a different error")
        #expect(unknown.explanation == unknown.providerMessage)
        let missing = ChatRunFailurePresentation(runID: .init("run"), reason: " \n")
        #expect(missing.providerMessage == nil)
        #expect(!missing.explanation.isEmpty)
    }

    @Test("details change measured height, survive adoption, and do not retry")
    func disclosureAndRetry() throws {
        var retried: [ConversationRunID] = []
        let factory = LiveChatTranscriptCardFactory(
            theme: .ottam(.builtIn), environment: .init(typography: .builtIn),
            actions: .init(retryFailedRun: { retried.append($0) })
        )
        let presentation = ChatRunFailurePresentation(runID: .init("run"), reason: String(repeating: "Provider detail. ", count: 30), canRetry: true)
        let card = try #require(factory.makeCard(.runFailure(presentation), id: "failure") as? HostedRunFailureCard)
        let collapsed = card.measuredHeight(forWidth: 450)
        card.actions.onToggleDetails()
        #expect(card.measuredHeight(forWidth: 450) > collapsed)
        var finished = presentation
        finished.canRetry = false
        let fresh = try #require(factory.makeCard(.runFailure(finished), id: "failure"))
        #expect(card.adoptPayload(from: fresh))
        #expect(card.payload.showsDetails)
        #expect(!card.payload.presentation.canRetry)
        #expect(retried.isEmpty)
        card.actions.onToggleDetails()
        #expect(!card.payload.showsDetails)
        card.actions.onRetry()
        #expect(retried == [.init("run")])
    }

    @Test("continuation preserves routing and reuses the post across repeated attempts")
    func continuationRouting() {
        let run = ConversationRunSummary(
            runID: .init("01a170b7-1a91-76f3-8b24-a252031754b5"), conversationID: TranscriptFixture.conversationID,
            engineID: .init("opencode"), modelID: "opencode-go/kimi-k3.1-pro",
            modeID: "plan", thoughtLevelID: "high", status: .failed,
            startedAt: "2026-10-25T01:40:10Z", cost: .object([:]), error: .object([:])
        )
        let continuation = ChatWorkspaceModel.failureContinuation(for: run)
        #expect(continuation == ChatWorkspaceModel.failureContinuation(for: run))
        #expect(continuation.engineID == run.engineID)
        #expect(continuation.modelID == run.modelID)
        #expect(continuation.modeID == run.modeID)
        #expect(continuation.thoughtLevelID == run.thoughtLevelID)
        #expect(continuation.body == "Continue from where the previous attempt stopped.")
        #expect(continuation.attachmentIDs.isEmpty)
    }

    @Test("retry waits for a stranded message but not for its own replay")
    func retryWaitsForPendingInputs() {
        let run = ConversationRunSummary(
            runID: .init("01a170b7-1a91-76f3-8b24-a252031754b5"), conversationID: TranscriptFixture.conversationID,
            engineID: .init("opencode"), status: .failed,
            startedAt: "2026-10-25T01:40:10Z", cost: .object([:]), error: .object([:])
        )
        let replayKey = ChatWorkspaceModel.failureContinuation(for: run).id.uuidString
        func waits(_ inputs: ConversationRunInput...) -> Bool {
            ChatWorkspaceModel.retryWaits(on: inputs, for: run)
        }
        #expect(!waits(TranscriptFixture.runInput("done", messageID: "m1", status: .delivered, targetRunID: run.runID.rawValue)))
        #expect(waits(TranscriptFixture.runInput("steer", messageID: "m1", kind: .steer, status: .sending, targetRunID: run.runID.rawValue)))
        #expect(waits(TranscriptFixture.runInput("stranded", messageID: "m2", status: .pending)))
        #expect(!waits(TranscriptFixture.runInput(replayKey, messageID: "m3", status: .pending)))
        #expect(!waits(TranscriptFixture.runInput("gave-up", messageID: "m2", status: .failed)))
    }
}
