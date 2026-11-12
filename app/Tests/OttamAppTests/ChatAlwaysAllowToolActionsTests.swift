import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

/// "Always allow tool actions" is per chat: a draft shows and writes the
/// value new chats start from, and a chat writes its own, which core also
/// makes the default.
@Suite("Chat always allow tool actions")
@MainActor
struct ChatAlwaysAllowToolActionsTests {
    private func makeModel() -> ChatWorkspaceModel {
        let suite = "ChatAlwaysAllowToolActionsTests.\(UUID().uuidString)"
        return ChatWorkspaceModel(defaults: UserDefaults(suiteName: suite)!)
    }

    private func conversation(_ id: String, alwaysAllow: Bool) -> ConversationSummary {
        ConversationSummary(
            conversationID: ConversationID(id),
            workspaceID: WorkspaceID("workspace-1"),
            workingRoot: WorkingRootSummary(
                kind: .workspaceFiles,
                displayName: "Workspace files",
                canonicalPath: "/tmp/workspace/files",
                availability: .available
            ),
            title: id,
            createdAt: "2026-10-18T00:00:00Z",
            updatedAt: "2026-10-18T00:00:00Z",
            alwaysAllowToolActions: alwaysAllow
        )
    }

    @Test("A draft writes the default without naming a chat")
    func draftWritesDefault() async {
        let model = makeModel()
        var sent: [(ConversationID?, Bool)] = []
        await model.setAlwaysAllowToolActions(true) { conversationID, enabled in
            sent.append((conversationID, enabled))
            return ToolPermissionsState(alwaysAllowToolActions: enabled)
        }.value
        #expect(sent.count == 1 && sent[0].0 == nil && sent[0].1)
        #expect(model.alwaysAllowToolActions)
    }

    @Test("Each chat shows its own value, and a write moves the draft default too")
    func chatWritesItsOwnValue() async {
        let model = makeModel()
        model.apply(conversations: [
            conversation("a", alwaysAllow: false),
            conversation("b", alwaysAllow: true),
        ])
        model.selectedConversationID = ConversationID("b")
        #expect(model.alwaysAllowToolActions)
        model.selectedConversationID = ConversationID("a")
        #expect(model.alwaysAllowToolActions == false)

        await model.setAlwaysAllowToolActions(true) { conversationID, enabled in
            #expect(conversationID == ConversationID("a"))
            return ToolPermissionsState(alwaysAllowToolActions: enabled)
        }.value
        #expect(model.alwaysAllowToolActions)
        model.selectedConversationID = nil
        #expect(model.alwaysAllowToolActions, "new chats start from the last value set")
    }

    @Test("A refused write leaves the value alone")
    func refusedWriteKeepsValue() async {
        let model = makeModel()
        model.apply(conversations: [conversation("a", alwaysAllow: false)])
        model.selectedConversationID = ConversationID("a")
        await model.setAlwaysAllowToolActions(true) { _, _ in
            throw CancellationError()
        }.value
        #expect(model.alwaysAllowToolActions == false)
    }

    @Test("A send waits for a write still on its way")
    func settleWaitsForPendingWrite() async {
        let model = makeModel()
        await model.setAlwaysAllowToolActions(true) { _, enabled in
            ToolPermissionsState(alwaysAllowToolActions: enabled)
        }.value
        let (gate, open) = AsyncStream<Void>.makeStream()
        model.setAlwaysAllowToolActions(false) { _, enabled in
            for await _ in gate { break }
            return ToolPermissionsState(alwaysAllowToolActions: enabled)
        }
        let settled = Task { @MainActor in
            await model.settleAlwaysAllowToolActions()
            return model.alwaysAllowToolActions
        }
        #expect(model.alwaysAllowToolActions, "the held write has not landed")
        open.yield()
        #expect(await settled.value == false, "settling returns only once it lands")
    }
}
