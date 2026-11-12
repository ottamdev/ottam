import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// The tools pane column's content: the selected conversation's tools, or
/// the tool picker until it has a tab. A collapsed pane takes its content out
/// of the window; SwiftUI keeps the content's state.
struct ConversationToolsColumn: View {
    @Bindable var session: WorkspaceWindowSession
    private let typography = TypographySettings.shared

    var body: some View {
        Group {
            if let conversation = session.selectedConversation {
                Group {
                    if session.tools.state(for: conversation.conversationID).tools.isEmpty {
                        ConversationToolPicker(session: session)
                    } else {
                        ConversationToolsPane(
                            conversation: conversation,
                            tools: session.tools,
                            workspace: session.workspace,
                            terminals: session.terminalWorkspace,
                            chat: session.chatWorkspace,
                            backend: session.diffModel
                        )
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(AccessibilityID.conversationTools)
                .accessibilityLabel("Tools for \(conversation.title)")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .appTypographyRoot(typography.resolved)
        // Rebuilt once per Core generation, like the other columns.
        .id(session.readyCoreInstanceID)
        .environment(session.runtime)
    }
}

/// The pane before a conversation's first tab: the kinds of tool, one row
/// each. Choosing one opens it as the first tab.
private struct ConversationToolPicker: View {
    let session: WorkspaceWindowSession

    var body: some View {
        List {
            row(
                .review,
                detail: "Changes in this conversation’s project",
                id: AccessibilityID.conversationToolsPickReview,
                action: session.openReview
            )
            row(
                .terminal,
                detail: "A shell in this conversation’s folder",
                id: AccessibilityID.conversationToolsPickTerminal,
                action: session.openTerminal
            )
            .disabled(!session.canOpenTerminal)
            row(
                .plan,
                detail: "The latest plan in this conversation",
                id: AccessibilityID.conversationToolsPickPlan,
                action: session.openPlan
            )
            .disabled(!session.canOpenPlan)
        }
        .listStyle(.inset)
    }

    private func row(
        _ kind: ConversationToolKind, detail: String, id: String, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Label {
                VStack(alignment: .leading, spacing: 2) {
                    Text(kind.title)
                    Text(detail)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            } icon: {
                ConversationToolIcon(kind: kind)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 4)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(id)
    }
}

/// A conversation's tools. See `docs/architecture/app/macos-ui.md`.
struct ConversationToolsPane: View {
    let conversation: ConversationSummary
    let tools: ConversationToolsStore
    let workspace: WorkspaceStore
    let terminals: TerminalWorkspaceModel
    let chat: ChatWorkspaceModel
    let backend: DiffReviewModel

    private var project: ResolvedProject? {
        guard let sourceID = conversation.workingRoot.sourceID else { return nil }
        return workspace.projects.first { $0.source.projectSourceID == sourceID }?.resolvedProject
    }

    private var root: URL {
        URL(fileURLWithPath: conversation.workingRoot.canonicalPath)
    }

    private func open(_ url: URL) {
        tools.open(url, in: conversation.conversationID)
    }

    var body: some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(ReviewColors.editorBackground)
    }

    @ViewBuilder private var content: some View {
        switch tools.state(for: conversation.conversationID).selected {
        case .review:
            if let project {
                ConversationReviewTool(model: tools.reviewModel(for: project), sourceID: project.projectSourceID)
            } else {
                ContentUnavailableView(
                    "No Project",
                    systemImage: "folder",
                    description: Text("This conversation isn’t in a project, so there are no changes to review.")
                )
            }
        case .terminal(let terminalID):
            ConversationTerminalTool(
                conversationID: conversation.conversationID,
                terminalID: terminalID,
                tools: tools,
                terminals: terminals
            )
        case .subagent:
            if let thread = tools.state(for: conversation.conversationID).subagentThreadID {
                ConversationSubagentTool(
                    conversationID: conversation.conversationID,
                    thread: thread,
                    model: chat,
                    backend: backend,
                    tools: tools
                )
            } else {
                Color.clear
            }
        case .plan:
            ConversationPlanTool(
                plan: tools.state(for: conversation.conversationID).plan,
                resourceRoot: root,
                openLink: open
            )
        case .file(let file):
            ConversationFileTool(
                file: file,
                document: tools.document(for: file),
                readingPosition: tools.readingPosition(for: file, in: conversation.conversationID),
                root: root,
                openLink: open
            )
            // Per tab, so switching conversations on one file saves and
            // restores each tab's place.
            .id([conversation.conversationID.rawValue, file.path])
        case nil:
            Color.clear
        }
    }
}

/// A terminal tab. Core opens the terminal when the tab first shows; it takes
/// keyboard focus only when Open Terminal asked for it.
private struct ConversationTerminalTool: View {
    let conversationID: ConversationID
    let terminalID: TerminalID
    let tools: ConversationToolsStore
    let terminals: TerminalWorkspaceModel

    var body: some View {
        if let record = terminals.record(for: terminalID),
            let model = terminals.sessionModel(for: terminalID),
            let surface = terminals.surfaceController(for: record)
        {
            ZStack(alignment: .topLeading) {
                TerminalWorkspaceView(surfaceController: surface, isActive: false)
                if model.isOpening || model.errorText != nil {
                    TerminalStatusOverlay(model: model)
                        .padding(14)
                }
            }
            .task(id: terminalID) {
                model.startIfNeeded(cwd: record.cwd, title: record.title)
                if tools.consumeFocus(for: .terminal(terminalID), in: conversationID) {
                    surface.activateFocus()
                }
            }
        }
    }
}

/// A terminal still opening, or one that failed.
private struct TerminalStatusOverlay: View {
    let model: TerminalSessionModel

    var body: some View {
        HStack(spacing: 10) {
            if model.isOpening {
                ProgressView()
                    .controlSize(.small)
            } else if model.errorText != nil {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.yellow)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(model.statusText)
                    .appFont(.callout)
                if let errorText = model.errorText {
                    Text(errorText)
                        .appFont(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .frame(maxWidth: 460, alignment: .leading)
    }
}

/// One review model per project, so every conversation in it shares loads.
/// Each time Review shows, it reloads behind the snapshot it has, which
/// stays up until the new one lands.
private struct ConversationReviewTool: View {
    @Bindable var model: DiffReviewModel
    let sourceID: ProjectSourceID

    var body: some View {
        DiffReviewView(model: model)
            .task(id: sourceID) {
                if !model.isLoading {
                    await model.load()
                }
            }
    }
}
