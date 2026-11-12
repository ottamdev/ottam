import OttamBackendClient
import Foundation
@testable import OttamApp

/// Transcript inputs with only the fields the assembler branches on carrying
/// meaning; everything else is filler.
enum TranscriptFixture {
    static let conversationID = ConversationID("conv")

    static func message(
        _ id: String,
        role: ConversationRole,
        body: String = "hello",
        position: UInt64 = 0,
        attachments: [ConversationAttachment] = []
    ) -> ConversationMessage {
        ConversationMessage(
            messageID: ConversationMessageID(id),
            conversationID: conversationID,
            role: role,
            body: body,
            position: position,
            createdAt: "2026-08-21T10:00:00Z",
            attachments: attachments
        )
    }

    static func attachment(_ id: String) -> ConversationAttachment {
        ConversationAttachment(
            attachmentID: ConversationAttachmentID(id),
            kind: .image, mimeType: "image/png", byteCount: 1, width: 1, height: 1,
            path: "/tmp/\(id).png", sourceKind: .pasted, originPath: nil, createdAt: "2026-08-21T10:00:00Z"
        )
    }

    static func runInput(
        _ id: String,
        messageID: String,
        kind: RunInputKind = .initial,
        status: RunInputStatus,
        targetRunID: String? = nil
    ) -> ConversationRunInput {
        ConversationRunInput(
            runInputID: ConversationRunInputID(id),
            conversationID: conversationID,
            messageID: ConversationMessageID(messageID),
            targetRunID: targetRunID.map(ConversationRunID.init),
            kind: kind,
            idempotencyKey: id,
            status: status
        )
    }

    /// The row that trails a user bubble. Defaults match `message(_:role:)`,
    /// so a fixture message and its trailer agree without restating either.
    static func userTrailer(
        _ messageID: String = "m1",
        body: String = "hello",
        input: ConversationRunInput? = nil,
        showsResend: Bool = false,
        isMutating: Bool = false
    ) -> ChatTranscriptCardSpec {
        .userTrailer(ChatUserTrailerPresentation(
            messageID: ConversationMessageID(messageID),
            body: body,
            createdAt: "2026-08-21T10:00:00Z",
            input: input,
            showsResend: showsResend,
            isMutating: isMutating
        ))
    }

    static func engine() -> EngineSummary {
        EngineSummary(
            engineID: EngineID("claude"), displayName: "Claude Code",
            authState: .ready, installState: .ready,
            capabilities: [], models: [], modes: [], thoughtLevels: [],
            steeringCapability: .interruptThenPrompt, authMethods: [], runtime: nil
        )
    }

    static func activity(_ id: String, label: String = "Read file") -> ChatLiveActivity {
        ChatLiveActivity(id: id, label: label, detail: nil, kind: "read", toolCallID: nil)
    }

    static func liveRun(_ runID: String) -> ChatLiveRunState {
        ChatLiveRunState(
            runID: ConversationRunID(runID), conversationID: conversationID,
            status: .running
        )
    }

    static func item(_ id: String, position: UInt64 = 1, revision: UInt64 = 1, body: String = "answer") -> ConversationTranscriptItem {
        ConversationTranscriptItem(itemID: id, conversationID: conversationID, runID: ConversationRunID("run-1"),
            position: position, revision: revision, kind: .assistantText, completion: .complete,
            createdAt: "2026-08-21T10:00:00Z", updatedAt: "2026-08-21T10:00:00Z",
            bodyPreview: body, bodyByteCount: UInt64(body.utf8.count))
    }

    /// A user turn followed by an assistant turn whose run events are loaded.
    static func snapshot(
        userBody: String = "question",
        liveRun: ChatLiveRunState? = nil,
        pending: [ChatPendingPermission] = []
    ) -> ChatTranscriptSnapshot {
        ChatTranscriptSnapshot(
            messages: [
                .init(message: message("m1", role: .user, body: userBody)),
                            ],
            liveRun: liveRun,
            runStatuses: [.init("run-1"): liveRun?.status ?? .completed],
            normalizedItems: [item("answer", position: 1, body: "answer")],
            pendingPermissions: pending
        )
    }
}
