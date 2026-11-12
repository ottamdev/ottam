import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// The Subagent tab: one subagent's conversation, drawn by the main
/// transcript's renderer with no composer. A subagent can't be prompted, so
/// the tab is read-only; a nested subagent's card retargets it.
struct ConversationSubagentTool: View {
    let conversationID: ConversationID
    let thread: String
    let model: ChatWorkspaceModel
    let backend: DiffReviewModel
    let tools: ConversationToolsStore
    @State private var owner = UUID()

    var body: some View {
        VStack(spacing: 0) {
            SubagentThreadHeader(path: model.subagentPath(to: thread)) { ancestor in
                tools.showSubagent(ancestor, in: conversationID)
            }
            Divider()
            if let error = model.subagentLoadError, model.subagentTranscript.items.isEmpty {
                ContentUnavailableView(
                    "Couldn’t Load Subagent",
                    systemImage: "exclamationmark.triangle",
                    description: Text(error)
                )
            } else {
                SubagentTranscript(conversationID: conversationID, model: model, backend: backend, tools: tools)
                    // A different thread is a new document, not a diff of the
                    // old one: its own scroll position, selection, and cards.
                    .id(thread)
            }
        }
        .task(id: "\(conversationID.rawValue)/\(thread)") {
            await model.loadSubagentThread(thread, in: conversationID, owner: owner, backend: backend)
        }
        // Closed or hidden, the tab follows nothing; shown again, it reloads.
        .onDisappear { model.releaseSubagentThread(owner: owner) }
    }
}

/// Where the subagent sits: the subagents that launched it, outermost first,
/// each a link back, then its own name and state.
private struct SubagentThreadHeader: View {
    let path: [ChatWorkspaceModel.SubagentPathStep]
    let open: (String) -> Void

    var body: some View {
        HStack(spacing: 6) {
            ForEach(Array(path.enumerated()), id: \.element.id) { index, step in
                if index > 0 {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.tertiary)
                }
                if index == path.count - 1 {
                    // The tab's title: it names the thread the tab shows.
                    Text(step.title)
                        .appFont(.body)
                        .fontWeight(.semibold)
                        .lineLimit(1)
                        .accessibilityIdentifier(AccessibilityID.chatSubagentTab)
                        .accessibilityValue(step.id)
                    if let state = step.stateLabel {
                        Text(state)
                            .appFont(.callout)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .fixedSize()
                    }
                } else {
                    Button(step.title) { open(step.id) }
                        .buttonStyle(.link)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatSubagentBreadcrumb)
        .accessibilityValue(path.map(\.id).joined(separator: " › "))
    }
}

private struct SubagentTranscript: View {
    let conversationID: ConversationID
    let model: ChatWorkspaceModel
    let backend: DiffReviewModel
    let tools: ConversationToolsStore
    @State private var surface = MacAgentTranscriptView.Controller()
    @State private var presentation = ChatTranscriptPresentationCoordinator()

    var body: some View {
        MacAgentTranscriptView(
            snapshot: model.subagentTranscriptSnapshot(),
            actions: actions,
            controller: surface,
            presentation: presentation,
            transcriptStore: model.subagentTranscript,
            isLoadingOlderRows: model.isLoadingOlderSubagentRows,
            onNearTop: {
                Task { await model.loadOlderSubagentRows(backend: backend) }
            },
            bottomOverlayInset: 0,
            accessibilityIdentifier: AccessibilityID.chatSubagentTranscript
        )
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// Only what reads or navigates: nothing here sends, retries, or answers.
    private var actions: ChatTranscriptActions {
        let workspaceID = model.workspaceID
        let conversationID = conversationID
        let tools = tools
        let backend = backend
        return ChatTranscriptActions(
            openSubagentThread: { thread in tools.showSubagent(thread, in: conversationID) },
            openPlan: { plan in tools.showPlan(plan, in: conversationID) },
            openFile: { url in tools.open(url, in: conversationID) },
            readPayload: { id, revision, part, offset in
                guard let workspaceID else { throw CancellationError() }
                return try await backend.readConversationTranscriptPayload(.init(
                    workspaceID: workspaceID, conversationID: conversationID,
                    payloadID: id, revision: revision, part: part, offset: offset
                ))
            },
            readBody: { id, revision, offset in
                guard let workspaceID else { throw CancellationError() }
                return try await backend.readConversationTranscriptBody(
                    workspaceID: workspaceID, conversationID: conversationID,
                    itemID: id, revision: revision, offset: offset
                )
            }
        )
    }
}

extension ChatWorkspaceModel {
    struct SubagentPathStep: Equatable {
        var id: String
        var title: String
        var stateLabel: String?
    }

    /// `thread` and the threads that launched it, outermost first, labeled
    /// from whichever transcript has their anchors loaded. A subagent the
    /// parent resumed has an anchor per run; the latest has its state.
    func subagentPath(to thread: String) -> [SubagentPathStep] {
        let anchors = Dictionary(
            (Array(subagentTranscript.items.values) + Array(transcriptStore.items.values)).compactMap { item in
                item.tool?.subagentThread.map { ($0.id, (item.position, $0)) }
            },
            uniquingKeysWith: { $0.0 >= $1.0 ? $0 : $1 }
        ).mapValues(\.1)
        var path: [SubagentPathStep] = []
        var next: String? = thread
        while let id = next, !path.contains(where: { $0.id == id }) {
            let anchor = anchors[id]
            let state = anchor.map { ChatSubagentThread(id: id, state: $0.state, members: []).stateLabel } ?? nil
            path.insert(SubagentPathStep(id: id, title: anchor?.name ?? anchor?.task ?? "Subagent", stateLabel: state), at: 0)
            next = anchor?.parentThreadID
        }
        return path
    }
}
