import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamCore
import OttamPresentationLogic
import OttamWorkspaceLogic
import OSLog
import SwiftUI
import UniformTypeIdentifiers

struct ChatWorkspaceView: View {
    @Bindable var workspace: WorkspaceStore
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let addProject: () -> Void

    var body: some View {
        ChatWorkbench(model: model, backend: backend, addProject: addProject)
            .task(id: workspace.workspaceID) {
                await model.loadIfNeeded(backend: backend)
            }
    }
}

/// The Chat detail pane: the composer-first landing when nothing is selected,
/// otherwise the conversation thread. History lives in the workspace sidebar;
/// the window title carries the conversation title.
private struct ChatWorkbench: View {
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let addProject: () -> Void

    var body: some View {
        // While a selection's detail is in flight the thread pane stays up (with
        // its loading state) — flashing the landing mid-switch reads as flicker.
        Group {
            if model.selectedDetail == nil, !model.isLoadingDetail {
                ChatEmptyLanding(model: model, backend: backend, addProject: addProject)
            } else {
                ChatCenterPane(model: model, backend: backend)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatSurface)
        // A login that went wrong: the terminal core has been running all along
        // is put on screen in place, scrollback and all, rather than restarted
        // somewhere the user can watch.
        .sheet(item: Binding(get: { model.signIn.revealed }, set: { revealed in
            if revealed == nil { model.signIn.dismissRevealedTerminal() }
        })) { revealed in
            AgentSignInTerminalSheet(revealed: revealed, signIn: model.signIn)
        }
    }
}

// MARK: - Empty-state landing

/// The empty state shown in the center pane when no conversation is selected:
/// agent setup until an engine is ready, then a centered prompt and composer. History lives in the
/// always-visible sidebar, so the landing carries no list of its own. Submitting creates
/// the first conversation and swaps the pane to the thread.
private struct ChatEmptyLanding: View {
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let addProject: () -> Void
    @State private var draft = ""
    // Attachments are per-composer, exactly like the draft: the landing and
    // the thread each own theirs, so one never renders the other's cards.
    @State private var attachments: [ConversationAttachment] = []
    @State private var attachmentWorkspaceID: WorkspaceID?

    var body: some View {
        // A ScrollView (not a bare VStack) so the shell's top scroll-edge treatment has a
        // backing; without it the unified titlebar goes transparent and the sidebar slides
        // up under the window controls. The min-height frame keeps the content centered.
        GeometryReader { proxy in
            ScrollView {
                VStack {
                    Spacer(minLength: 0)

                    VStack(spacing: 22) {
                        if model.isPreparingEngines {
                            ProgressView("Checking agents…")
                        } else if model.engineSetupNeeded {
                            ChatAgentSetupLanding(model: model, backend: backend)
                        } else {
                            Text("What do you want to work on?")
                                .appFont(.display)
                                .foregroundStyle(.primary.opacity(0.92))
                                .multilineTextAlignment(.center)
                                .fixedSize(horizontal: false, vertical: true)

                            ChatLandingComposer(
                                draft: $draft,
                                attachments: $attachments,
                                model: model,
                                backend: backend,
                                resetGeneration: model.draftGeneration,
                                addProject: addProject
                            )
                            .frame(maxWidth: 760)
                        }

                        if let errorText = model.errorText {
                            Label(errorText, systemImage: "exclamationmark.triangle")
                                .appFont(.callout)
                                .foregroundStyle(.orange)
                                .fixedSize(horizontal: false, vertical: true)
                        }

                        if let noticeText = model.visibleNoticeText {
                            Label(noticeText, systemImage: "info.circle")
                                .appFont(.callout)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .padding(.horizontal, 40)
                    .padding(.vertical, 24)

                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity, minHeight: proxy.size.height)
            }
            .toolbarScrollEdge()
        }
        .onDisappear {
            abandonDraft()
        }
        .onChange(of: model.draftGeneration) {
            abandonDraft()
        }
        .onChange(of: attachments.map(\.attachmentID)) { oldIDs, newIDs in
            if oldIDs.isEmpty, !newIDs.isEmpty {
                attachmentWorkspaceID = model.workspaceID
            } else if newIDs.isEmpty {
                attachmentWorkspaceID = nil
            }
        }
    }

    private func abandonDraft() {
        let abandoned = attachments
        let workspaceID = attachmentWorkspaceID ?? model.workspaceID
        draft = ""
        attachments.removeAll()
        attachmentWorkspaceID = nil
        guard !abandoned.isEmpty else { return }
        Task {
            for attachment in abandoned {
                await model.discardAttachment(
                    attachment,
                    workspaceID: workspaceID,
                    backend: backend
                )
            }
        }
    }
}

/// A useful first action while no agent can accept a new conversation.
private struct ChatAgentSetupLanding: View {
    @Bindable var model: ChatWorkspaceModel
    let backend: DiffReviewModel
    @Environment(OttamAppRuntime.self) private var runtime: OttamAppRuntime?
    @Environment(\.openWindow) private var openWindow

    private var signInEngine: EngineSummary? {
        EngineSummary.defaultSelection(from: model.engines.filter {
            $0.installState != .missing && $0.installState != .unsupported
                && EngineRuntimeProblem(engine: $0) == nil
                && ($0.authState == .missing || $0.authState == .expired)
        })
    }

    var body: some View {
        VStack(spacing: 16) {
            Text(signInEngine == nil ? "Set up your first agent" : "Finish setting up your agent")
                .appFont(.display)
                .multilineTextAlignment(.center)
            Text(signInEngine.map { "Sign in to \($0.displayName) to start working on your projects." }
                 ?? "Connect a coding agent to start working on your projects.")
                .appFont(.body)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            // Onboarding sets agents up; this only finishes a sign-in or points
            // at Settings, where installs and profiles live.
            if let engine = signInEngine {
                let agentID = AgentID(engine.engineID.rawValue)
                Button(model.signIn.isSigningIn(agentID) ? "Signing In…" : "Sign In to \(engine.displayName)") {
                    model.signIn.signIn(engine: engine)
                }
                .disabled(!model.signIn.canStartOrResume(agentID))
                .buttonStyle(.glassProminent)
                .controlSize(.large)
                .accessibilityIdentifier(AccessibilityID.chatSetUpAgents)

                if let failure = model.signIn.failure(for: agentID) {
                    Text(failure)
                        .appFont(.callout)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
            } else {
                Button("Open Agent Settings…") {
                    runtime?.pendingSettingsPane = .agents
                    openWindow(id: "settings")
                }
                .buttonStyle(.glassProminent)
                .controlSize(.large)
                .accessibilityIdentifier(AccessibilityID.chatSetUpAgents)
            }
        }
        .frame(maxWidth: 760)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatSurface)
    }
}

/// The rounded composer box shared by the empty-state landing and the in-workbench
/// loose thread: a growing text field over a quiet control bar (attach behind
/// `+`, engine, model, mic, and a circular send). The field starts one line tall, grows
/// with content up to ten lines, then scrolls. Return sends; Option-Return or Shift-Return
/// inserts a newline.
private struct ChatRoundedComposer: View {
    /// Leading/trailing inset of the editor (and attachment row) inside the card.
    static let editorHorizontalPadding: CGFloat = 16

    @Binding var draft: String
    @Binding var attachments: [ConversationAttachment]
    /// Quote replies that go out ahead of the draft. The landing has none.
    var quotes: Binding<[QuoteReply.Comment]> = .constant([])
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let placeholder: String
    var autoFocus: Bool = false
    var contextID: ConversationID?
    var resetGeneration = 0
    @State private var isEditorFocused = false
    @State private var quickLook = ChatAttachmentQuickLook()
    @State private var editorHeight: CGFloat = 18
    @State private var isDropTargeted = false
    @State private var pendingIngests = 0
    @State private var ingestQueue: [PendingAttachmentIngest] = []
    @State private var ingestTask: Task<Void, Never>?
    @State private var ingestGeneration = UUID()
    @State private var escapeStopConfirmation = ChatEscapeStopConfirmation()
    @State private var escapeResetTask: Task<Void, Never>?
    @State private var stoppingRunID: ConversationRunID?
    @State private var commandPicker = ChatCommandPicker()
    // Environment, not @FocusedValue — see ChatLandingEngineMenu.
    @Environment(OttamAppRuntime.self) private var runtime: OttamAppRuntime?
    @Environment(\.openWindow) private var openWindow

    private var canSend: Bool {
        guard model.canSubmit, pendingIngests == 0 else { return false }
        return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !attachments.isEmpty
            || !quotes.wrappedValue.isEmpty
    }

    /// The draft with any staged quote replies ahead of it.
    private var messageText: String {
        QuoteReply.messageBody(comments: quotes.wrappedValue, text: draft) ?? ""
    }

    // The canonical run, not `selectedLiveRun`: that one re-composes the whole
    // streamed reply on every read, and the composer only needs nil-ness.
    private var hasLiveRun: Bool { model.selectedCanonicalLiveRun != nil }

    /// The live run is the agent continuing on its own. A message takes over
    /// from it instead of waiting behind it, so there is nothing to queue
    /// behind and nothing to steer into.
    private var followUpIsLive: Bool { model.selectedCanonicalLiveRun?.origin == .autonomous }

    private var primaryAction: ChatComposerAction {
        let action = ChatComposerAction(
            hasLiveRun: hasLiveRun,
            canSend: canSend,
            isSubmitting: model.isSubmitting || model.isDrainingSelectedConversation
        )
        return followUpIsLive && action == .queue ? .submit : action
    }

    private var stopsLiveRun: Bool { primaryAction == .stop }

    /// A steer already on the wire. The run must survive to receive it, so
    /// neither the button nor Escape may aim at stopping the run.
    private var steerInFlight: Bool { hasLiveRun && primaryAction == .sending }

    private var primaryActionEnabled: Bool {
        switch primaryAction {
        case .stop: stoppingRunID == nil
        case .sending: false
        case .submit, .queue: canSend
        }
    }

    private var primaryActionLabel: String {
        // The model names a live run's action Queue; a follow-up is the one
        // live run a message takes over from instead.
        if followUpIsLive && primaryAction == .submit { return "Send" }
        guard primaryAction == .stop else { return model.submitActionLabel }
        return escapeStopConfirmation.isArmed
            ? "Press Escape again to stop"
            : "Stop"
    }

    private var primaryActionHelp: String {
        guard primaryAction == .queue else { return primaryActionLabel }
        // Return always takes the message, so the unsupported case is no longer
        // a refusal to explain — only a gesture that is not on offer.
        return canSteer
            ? "Queue behind this turn — ⌘↩ to steer into it"
            : "Queue behind this turn. This agent can't be steered mid-run."
    }

    /// Whether Command-Return has anything to do. Steering needs a working run
    /// and an engine that can take one; Return queues either way.
    private var canSteer: Bool {
        hasLiveRun && model.selectedSteeringCapability != .unsupported
    }

    /// Computed here rather than at the wrapper call sites so both the landing
    /// and in-conversation composers pick up the setup state with one edit.
    private var effectivePlaceholder: String {
        model.composerPlaceholderOverride ?? placeholder
    }

    var body: some View {
        VStack(spacing: 0) {
            if !quotes.wrappedValue.isEmpty {
                ChatQuoteCommentList(comments: quotes.wrappedValue) { comment in
                    quotes.wrappedValue.removeAll { $0.id == comment.id }
                }
                    .padding(.horizontal, Self.editorHorizontalPadding)
                    .padding(.top, 12)
                    .padding(.bottom, attachments.isEmpty ? 8 : 0)
            }
            if !attachments.isEmpty {
                ChatAttachmentCardRow(attachments: attachments, size: 84, quickLook: quickLook) { attachment in
                    remove(attachment)
                }
                    .padding(.leading, Self.editorHorizontalPadding)
                    .padding(.top, 12)
                    .padding(.bottom, 8)
            }

            MarkdownTextView(
                markdown: $draft,
                isFocused: $isEditorFocused,
                measuredHeight: $editorHeight,
                placeholder: effectivePlaceholder,
                onSubmit: send,
                onSteer: steer,
                onEscape: handleEscape,
                onAttach: attachFromPasteboard,
                onPickerKey: handlePickerKey,
                inlineHint: commandInputHint
            )
                .frame(height: editorHeight)
                .onAppear {
                    guard autoFocus, !model.isPreparingEngines else { return }
                    DispatchQueue.main.async {
                        isEditorFocused = true
                    }
                }
                // Focusing the field warms the system text-suggestions panel
                // (a remote popup window) which can flash on screen during the
                // launch churn — hold focus until the first paint settles.
                .onChange(of: model.isPreparingEngines) { _, preparing in
                    guard autoFocus, !preparing else { return }
                    DispatchQueue.main.async {
                        isEditorFocused = true
                    }
                }
                .padding(.horizontal, Self.editorHorizontalPadding)
                // The rows above bring their own bottom padding when present.
                .padding(.top, attachments.isEmpty && quotes.wrappedValue.isEmpty ? 14 : 0)

            HStack(spacing: 8) {
                // While launch data streams in, the controls render as static
                // stand-ins: mounting a SwiftUI Menu and then rebuilding its items
                // mid-load makes AppKit flash the menu's popup window on screen
                // (macOS 26). The real menus mount once, in final state, when the
                // catalog lands. Placeholder text only sizes the redacted capsules.
                // The session selector is a Button + popover — exempt from the
                // menu-flash constraint — but its label is launch data, so it
                // waits behind the same gate rather than flashing wrong text.
                if model.isPreparingEngines {
                    Image(systemName: "plus")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 28, height: 28)
                    ChatLandingPill(systemImage: "gearshape", text: "Claude Code")
                        .redacted(reason: .placeholder)
                } else {
                    ChatLandingPlusMenu(model: model, attachImage: attachFromPicker)
                    ChatSessionSelector(model: model, backend: backend)
                    if !model.queuedMessages.isEmpty {
                        ChatQueuedMessagesButton(model: model, backend: backend)
                    }
                }

                Spacer(minLength: 0)

                if let usage = model.selectedPlanUsage {
                    ChatPlanUsageGauge(
                        usage: usage,
                        isRefreshing: model.isRefreshingSelectedPlanUsage
                    ) {
                        Task { await model.forceRefreshSelectedPlanUsage(backend: backend) }
                    }
                }

                if let usage = model.selectedContextUsage {
                    ChatContextGauge(usage: usage)
                }

                Image(systemName: "mic")
                    .font(.system(size: 15))
                    .foregroundStyle(.tertiary)
                    .frame(width: 26, height: 26)

                if model.engineSetupNeeded {
                    // No engine can take this message — say so instead of
                    // mounting a send button that silently refuses. A plain
                    // Button, so the macOS 26 menu-flash constraint above
                    // doesn't apply.
                    Button("Set Up Agents") {
                        runtime?.pendingSettingsPane = .agents
                        openWindow(id: "settings")
                    }
                    .buttonStyle(.bordered)
                    .buttonBorderShape(.capsule)
                    .accessibilityIdentifier(AccessibilityID.chatSetUpAgents)
                } else {
                    Button(action: performPrimaryAction) {
                        ZStack {
                            Circle()
                                .fill(primaryActionEnabled ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
                                .frame(width: 30, height: 30)
                            if stoppingRunID != nil || primaryAction == .sending {
                                ProgressView().controlSize(.small)
                            } else if primaryAction == .stop, escapeStopConfirmation.isArmed {
                                Text("Esc")
                                    .font(.chrome(.caption, weight: .semibold, design: .rounded))
                                    .foregroundStyle(Color(nsColor: .windowBackgroundColor))
                            } else if let glyph = primaryAction.glyph {
                                Image(systemName: glyph)
                                    .font(.system(
                                        size: primaryAction == .stop ? 10 : 14,
                                        weight: .semibold
                                    ))
                                    .foregroundStyle(Color(nsColor: .windowBackgroundColor))
                            }
                        }
                    }
                    .buttonStyle(.plain)
                    .disabled(!primaryActionEnabled)
                    .help(primaryActionHelp)
                    .accessibilityLabel(primaryActionLabel)
                    .accessibilityIdentifier(primaryAction.accessibilityIdentifier)
                    .accessibilityValue(primaryAction.accessibilityValue)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            // The control bar is not a typing surface — keep the arrow there even
            // though the card as a whole reads as text input.
            .pointerStyle(.default)
            // Each pick of agent reads its plan usage; a stale snapshot
            // refreshes and arrives through the model's subscription.
            .task(id: model.selectedEngineID) {
                guard let engineID = model.selectedEngineID else { return }
                await model.refreshPlanUsage(engineID: engineID, backend: backend)
            }
        }
        .frame(maxWidth: .infinity)
        // Liquid Glass, not an opaque card: the composer floats over the thread
        // and the glass is what keeps the draft legible over scrolled turns.
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay {
            if isDropTargeted {
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(Color.accentColor, lineWidth: 2)
            }
        }
        // The card chrome owns the drop, not the text view — see the dragged-type
        // surgery in MarkdownTextView.
        .onDrop(of: [.image, .fileURL], isTargeted: $isDropTargeted) { providers in
            Task { await acceptDrop(providers) }
            return true
        }
        // The whole card is a click target for the editor: taps on the padding and
        // control-bar gaps focus the text view instead of dying on the background.
        // Interactive controls (menus, send) and the text view itself consume their
        // own clicks before this fires.
        .contentShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .onTapGesture {
            isEditorFocused = true
        }
        // Match the click target: anywhere that focuses the editor shows the I-beam.
        .pointerStyle(.horizontalText)
        // Floats over the thread just above the card, like the permission
        // pill, so opening it moves nothing; a question or plan panel steps aside
        // while it shows (`isCommandPickerOpen`).
        .overlay(alignment: .top) {
            // Its own container and an animated open, so the glass
            // materializes like the question panel's instead of popping in.
            GlassEffectContainer {
                commandPanel
            }
            .animation(.snappy(duration: 0.2), value: commandPicker.isOpen)
            .alignmentGuide(.top) { $0[.bottom] + 8 }
        }
        .onAppear(perform: refreshCommandPicker)
        .onChange(of: draft) { refreshCommandPicker() }
        .onChange(of: model.selectedCommands) { refreshCommandPicker() }
        .onChange(of: model.effectiveModeID) { refreshCommandPicker() }
        .onChange(of: model.availableModes) { refreshCommandPicker() }
        .onChange(of: contextID) {
            cancelPendingIngests()
        }
        .onChange(of: resetGeneration) {
            cancelPendingIngests()
        }
        .onChange(of: model.composerFocusRequest) {
            isEditorFocused = true
        }
        .onChange(of: model.selectedCanonicalLiveRun?.runID) {
            disarmEscapeStop()
            stoppingRunID = nil
        }
        .onDisappear {
            cancelPendingIngests()
            disarmEscapeStop()
            model.isCommandPickerOpen = false
        }
    }

    @ViewBuilder
    private var commandPanel: some View {
        if commandPicker.isOpen {
            ChatCommandPanel(picker: commandPicker, onPick: pickCommand)
        }
    }

    private func refreshCommandPicker() {
        commandPicker.update(
            draft: draft,
            modes: model.availableModes,
            currentModeID: model.effectiveModeID,
            commands: model.selectedCommands
        )
        if model.isCommandPickerOpen != commandPicker.isOpen {
            model.isCommandPickerOpen = commandPicker.isOpen
        }
    }

    private func handlePickerKey(_ key: ComposerPickerKey) -> Bool {
        guard commandPicker.isOpen else { return false }
        switch key {
        case .up:
            commandPicker.move(by: -1)
        case .down:
            commandPicker.move(by: 1)
        case .pick:
            // Nothing matches: Return sends the text as typed.
            guard let item = commandPicker.highlighted else { return false }
            pickCommand(item)
        case .complete:
            guard let item = commandPicker.highlighted else { return true }
            if case .command(let command) = item, command.isSkill {
                draft = command.name + " "
            } else if case .command(let command) = item {
                draft = "/" + command.name + (command.inputHint == nil ? "" : " ")
            } else {
                pickCommand(item)
            }
        case .dismiss:
            commandPicker.isDismissed = true
            refreshCommandPicker()
        }
        return true
    }

    /// A mode switches the mode, as the engine menu does, and clears the
    /// query. A command that takes input waits for it after `/name `; one
    /// that takes none goes now, bare, so the agent never sees text it would
    /// refuse. With attachments or quotes pending it waits too: Core sends a
    /// command as typed only without images, and staged quotes lead the
    /// message, so the command would no longer start it. Codex lists skills as `$name`, which
    /// it reads as a mention in the request, so a skill starts the request.
    private func pickCommand(_ item: ChatCommandPickerItem) {
        switch item {
        case .mode(let mode, _):
            model.selectMode(mode.id)
            Task { await model.applyPendingMode(backend: backend) }
            draft = ""
        case .command(let command) where command.isSkill:
            draft = command.name + " "
        case .command(let command) where command.inputHint != nil:
            draft = "/" + command.name + " "
        case .command(let command):
            draft = "/" + command.name
            if attachments.isEmpty, quotes.wrappedValue.isEmpty { send() }
        }
        isEditorFocused = true
    }

    /// The argument hint after `/name ` while nothing is typed after it.
    private var commandInputHint: String? {
        guard draft.hasPrefix("/"), draft.hasSuffix(" "), let commands = model.selectedCommands else { return nil }
        let name = String(draft.dropFirst().dropLast())
        return commands.first { $0.name == name }?.inputHint
    }

    private func performPrimaryAction() {
        if stopsLiveRun {
            stopSelectedRun()
        } else {
            send()
        }
    }

    private func handleEscape() -> Bool {
        // Escape is the dedicated stop gesture, so unlike the button it is not
        // re-aimed by a typed draft. The one exception is a steer already on
        // the wire: that run has to live long enough to receive it.
        guard hasLiveRun, !steerInFlight else {
            disarmEscapeStop()
            return false
        }
        if escapeStopConfirmation.press(at: ContinuousClock().now) {
            disarmEscapeStop()
            stopSelectedRun()
        } else {
            scheduleEscapeReset()
        }
        return true
    }

    private func scheduleEscapeReset() {
        escapeResetTask?.cancel()
        escapeResetTask = Task { @MainActor in
            try? await Task.sleep(for: ChatEscapeStopConfirmation.window)
            guard !Task.isCancelled else { return }
            escapeStopConfirmation.disarm()
            escapeResetTask = nil
        }
    }

    private func disarmEscapeStop() {
        escapeResetTask?.cancel()
        escapeResetTask = nil
        escapeStopConfirmation.disarm()
    }

    private func stopSelectedRun() {
        guard let runID = model.selectedCanonicalLiveRun?.runID,
              stoppingRunID == nil
        else { return }
        disarmEscapeStop()
        stoppingRunID = runID
        Task { @MainActor in
            await model.cancelSelectedRun(backend: backend)
            if stoppingRunID == runID { stoppingRunID = nil }
        }
    }

    /// Return. With a run working this holds the message behind it; the queue
    /// is client state, so it needs nothing from the adapter and every engine
    /// can accept a message at any time.
    private func send() {
        guard canSend else { return }
        if hasLiveRun, !followUpIsLive {
            // Deferred a turn, like the submit path beside it. Queueing empties
            // the draft, and an empty draft under a live run is the Stop
            // gesture — so doing this inline would have the button destroy
            // itself from inside its own action, which AppKit reports to the
            // caller as a failed press. Accessibility clients see that failure
            // even though the message was queued.
            Task { @MainActor in
                guard model.enqueue(messageText, attachments: attachments) else { return }
                draft = ""
                quotes.wrappedValue.removeAll()
                // The queued message owns these now — cleared from the composer,
                // but deliberately not discarded, since it still has to send
                // them.
                attachments.removeAll()
            }
            return
        }
        submitDraft()
    }

    /// Command-Return: join the working turn instead of queueing behind it.
    ///
    /// With no run to join it sends, which is what Return would have done. The
    /// text view consumes the keystroke either way, so anything this declines to
    /// handle is a message the reader watched disappear into nothing.
    private func steer() {
        guard canSend else { return }
        // An engine that cannot steer keeps the queue on Return; declining here
        // is what stops Command-Return from silently queueing instead, which
        // would put the message somewhere the reader did not ask for.
        guard canSteer || !hasLiveRun || followUpIsLive else { return }
        submitDraft()
    }

    private func submitDraft() {
        let sent = attachments
        let sentQuotes = quotes.wrappedValue.map(\.id)
        Task {
            if await model.submit(messageText, attachments: sent, backend: backend) {
                draft = ""
                quotes.wrappedValue.removeAll { sentQuotes.contains($0.id) }
                attachments.removeAll { sent.contains($0) }
            }
        }
    }

    private func remove(_ attachment: ConversationAttachment) {
        attachments.removeAll { $0.attachmentID == attachment.attachmentID }
        Task {
            await model.discardAttachment(attachment, backend: backend)
        }
    }

    // MARK: - Attachment intake

    /// Cards already on screen plus images still being normalized or uploaded.
    /// Counting only the former would let a ten-image drop start ten uploads
    /// that `claim` then rejects as a batch, losing the whole message.
    private var claimedSlots: Int {
        attachments.count + pendingIngests
    }

    private var hasAttachmentRoom: Bool {
        claimedSlots < ChatAttachmentIntake.maximumAttachmentsPerMessage
    }

    /// Claims a paste that carries an image. Returning false lets the paste
    /// continue into the text field unchanged.
    ///
    /// Runs on the main thread inside `paste(_:)`, so it only *looks* at the
    /// pasteboard — decoding and re-encoding a full-screen screenshot here would
    /// stall the keystroke.
    private func attachFromPasteboard(_ pasteboard: NSPasteboard) -> Bool {
        let prefersText = ChatAttachmentIntake.pasteboardPrefersText(pasteboard)
        let source = prefersText ? nil : ChatAttachmentIntake.imageSource(from: pasteboard)
        Self.attachmentLogger.notice(
            """
            attachFromPasteboard: canAttachImages=\(model.canAttachImages, privacy: .public) \
            disabledReason=\(model.attachDisabledReason ?? "-", privacy: .public) \
            engine=\(model.selectedEngine?.engineID.rawValue ?? "-", privacy: .public) \
            capabilities=\(model.selectedEngine?.capabilities.map(\.rawValue).joined(separator: ",") ?? "-", privacy: .public) \
            prefersText=\(prefersText, privacy: .public) \
            source=\(source.map { if case .file = $0 { "file" } else { "data" } } ?? "none", privacy: .public) \
            slots=\(claimedSlots, privacy: .public)
            """
        )
        guard model.canAttachImages, let source else { return false }
        guard hasAttachmentRoom else {
            model.reportAttachmentProblem(Self.tooManyAttachments)
            return true
        }
        ingest(source, sourceKind: .pasted, originPath: nil)
        return true
    }

    fileprivate static let attachmentLogger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "ChatAttachments"
    )

    @MainActor
    private func acceptDrop(_ providers: [NSItemProvider]) async {
        guard model.canAttachImages else {
            model.reportAttachmentProblem(model.attachDisabledReason)
            return
        }
        let generation = ingestGeneration
        for provider in providers {
            guard ingestGeneration == generation, !Task.isCancelled else { return }
            guard hasAttachmentRoom else {
                model.reportAttachmentProblem(Self.tooManyAttachments)
                return
            }
            // Reserve the slot before the item provider performs asynchronous
            // loading, so Return cannot submit while a drop is materializing.
            pendingIngests += 1
            guard let dropped = await loadDroppedImage(from: provider) else {
                if ingestGeneration == generation {
                    pendingIngests -= 1
                }
                continue
            }
            guard ingestGeneration == generation, !Task.isCancelled else { return }
            enqueue(
                dropped.source,
                sourceKind: dropped.originPath == nil ? .pasted : .dropped,
                originPath: dropped.originPath,
                slotAlreadyReserved: true
            )
        }
    }

    private func attachFromPicker() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowedContentTypes = ChatAttachmentIntake.acceptedContentTypes
        panel.prompt = "Attach"
        guard panel.runModal() == .OK else { return }
        for url in panel.urls {
            guard hasAttachmentRoom else {
                model.reportAttachmentProblem(Self.tooManyAttachments)
                return
            }
            ingest(.file(url), sourceKind: .picked, originPath: url.path)
        }
    }

    private func ingest(
        _ source: ChatAttachmentIntake.ChatAttachmentSource,
        sourceKind: ConversationAttachmentSourceKind,
        originPath: String?
    ) {
        enqueue(source, sourceKind: sourceKind, originPath: originPath)
    }

    /// Normalize and upload through one FIFO. This preserves the order of a
    /// multi-file picker/drop and makes the aggregate byte check deterministic.
    private func enqueue(
        _ source: ChatAttachmentIntake.ChatAttachmentSource,
        sourceKind: ConversationAttachmentSourceKind,
        originPath: String?,
        slotAlreadyReserved: Bool = false
    ) {
        if !slotAlreadyReserved {
            pendingIngests += 1
        }
        ingestQueue.append(
            PendingAttachmentIngest(
                source: source,
                sourceKind: sourceKind,
                originPath: originPath,
                workspaceID: model.workspaceID,
                draftGeneration: model.draftGeneration
            )
        )
        guard ingestTask == nil else { return }
        let generation = ingestGeneration
        ingestTask = Task {
            await processIngestQueue(generation: generation)
        }
    }

    @MainActor
    private func processIngestQueue(generation: UUID) async {
        defer {
            if ingestGeneration == generation {
                ingestTask = nil
            }
        }
        while ingestGeneration == generation, !Task.isCancelled, !ingestQueue.isEmpty {
            let request = ingestQueue.removeFirst()
            await ingest(request, generation: generation)
            if ingestGeneration == generation {
                pendingIngests -= 1
            }
        }
    }

    @MainActor
    private func ingest(_ request: PendingAttachmentIngest, generation: UUID) async {
        let payload: ChatAttachmentPayload
        do {
            // Off the main actor: decode, downscale, re-encode.
            payload = try await Task.detached(priority: .userInitiated) {
                try ChatAttachmentIntake.payload(for: request.source)
            }.value
            guard ingestGeneration == generation,
                  request.workspaceID == model.workspaceID,
                  request.draftGeneration == model.draftGeneration,
                  !Task.isCancelled
            else { return }
            Self.attachmentLogger.notice(
                """
                normalized attachment: mime=\(payload.mimeType, privacy: .public) \
                bytes=\(payload.data.count, privacy: .public) \
                pixels=\(payload.pixelWidth, privacy: .public)x\(payload.pixelHeight, privacy: .public)
                """
            )
        } catch {
            guard ingestGeneration == generation,
                  request.workspaceID == model.workspaceID,
                  request.draftGeneration == model.draftGeneration,
                  !Task.isCancelled
            else { return }
            Self.attachmentLogger.error(
                "attachment normalize failed: \(error.localizedDescription, privacy: .public)"
            )
            model.reportAttachmentProblem(error.localizedDescription)
            return
        }
        // Checked after normalizing, because the payload's size is what
        // `claim` will weigh — not the source file's.
        let total = attachments.reduce(0) { $0 + Int($1.byteCount) }
        guard total + payload.data.count
            <= ChatAttachmentIntake.maximumByteCountPerMessage else {
            model.reportAttachmentProblem(Self.attachmentsTooHeavy)
            return
        }
        guard let attachment = await model.ingestAttachment(
            payload,
            sourceKind: request.sourceKind,
            originPath: request.originPath,
            backend: backend
        ) else { return }
        guard ingestGeneration == generation,
              request.workspaceID == model.workspaceID,
              request.draftGeneration == model.draftGeneration,
              !Task.isCancelled
        else {
            await model.discardAttachment(
                attachment,
                workspaceID: request.workspaceID,
                backend: backend
            )
            return
        }
        attachments.append(attachment)
    }

    private func cancelPendingIngests() {
        ingestGeneration = UUID()
        ingestTask?.cancel()
        ingestTask = nil
        ingestQueue.removeAll()
        pendingIngests = 0
    }

    private static var attachmentsTooHeavy: String {
        "These images total more than one message can carry. Remove one and try again."
    }

    private static var tooManyAttachments: String {
        "A message can carry at most \(ChatAttachmentIntake.maximumAttachmentsPerMessage) images."
    }
}

/// What the composer's primary button means right now.
///
/// Derived in one place because the states are not independent. `canSend`
/// requires `model.canSubmit`, which goes false for the length of a send, so
/// reading "live run and cannot send" as Stop hands the button to Stop in the
/// middle of a steer and offers to kill the very run the reader is steering.
enum ChatComposerAction: Equatable {
    /// No run in flight; send when there is something to send.
    case submit
    /// A run is working, so Return holds the draft behind it. Steering into
    /// the turn is Command-Return, which is not what this button does.
    case queue
    /// A submission is on the wire. Holds the submit identity, disabled.
    case sending
    /// A run is working and there is nothing to steer it with.
    case stop

    init(hasLiveRun: Bool, canSend: Bool, isSubmitting: Bool) {
        if canSend {
            self = hasLiveRun ? .queue : .submit
        } else if isSubmitting {
            self = .sending
        } else {
            self = hasLiveRun ? .stop : .submit
        }
    }

    /// Nil while a spinner stands in for the glyph.
    var glyph: String? {
        switch self {
        case .submit: "arrow.up"
        case .queue: "arrow.down.to.line"
        case .sending: nil
        case .stop: "stop.fill"
        }
    }

    /// Only Stop is a different control to a reader driving by identifier;
    /// queueing and sending are both the send button doing its job.
    var accessibilityIdentifier: String {
        self == .stop ? AccessibilityID.chatStop : AccessibilityID.chatSubmit
    }

    /// What the identifier deliberately hides. Sending is otherwise silent — the
    /// glyph becomes a spinner and nothing announces why the button went inert —
    /// and it is indistinguishable from a button that is ready, since `canSend`
    /// does not consider a submission already in flight.
    var accessibilityValue: String {
        switch self {
        case .submit: "ready"
        case .queue: "queue"
        case .sending: "sending"
        case .stop: "stop"
        }
    }
}

struct ChatEscapeStopConfirmation {
    /// How long a first Escape stays armed; the composer's reset timer sleeps
    /// for the same value so the visible "Esc" state and the accepting window
    /// cannot drift apart.
    static let window: Duration = .seconds(3)

    private(set) var armedUntil: ContinuousClock.Instant?

    var isArmed: Bool { armedUntil != nil }

    /// Returns true only for the confirming press within `window`.
    mutating func press(at now: ContinuousClock.Instant) -> Bool {
        if let armedUntil, now <= armedUntil {
            self.armedUntil = nil
            return true
        }
        armedUntil = now.advanced(by: Self.window)
        return false
    }

    mutating func disarm() {
        armedUntil = nil
    }
}

/// Pulls bytes out of a dropped item. `loadDataRepresentation` rather than
/// `dropDestination(for: URL.self)`: an image dragged out of a browser or
/// Preview is promised in-memory data with no file behind it.
private func loadDroppedImage(
    from provider: NSItemProvider
) async -> (source: ChatAttachmentIntake.ChatAttachmentSource, originPath: String?)? {
    // A real file stays a file reference: the size check and memory mapping in
    // `payload(fromFileAt:)` are the only thing standing between us and reading
    // an arbitrarily large drop into memory.
    if provider.canLoadObject(ofClass: URL.self) {
        let url: URL? = await withCheckedContinuation { continuation in
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                continuation.resume(returning: url)
            }
        }
        if let url, url.isFileURL {
            return (.file(url), url.path)
        }
    }
    let data: Data? = await withCheckedContinuation { continuation in
        provider.loadDataRepresentation(forTypeIdentifier: UTType.image.identifier) { data, _ in
            continuation.resume(returning: data)
        }
    }
    guard let data else { return nil }
    return (.data(data), nil)
}

private struct PendingAttachmentIngest {
    let source: ChatAttachmentIntake.ChatAttachmentSource
    let sourceKind: ConversationAttachmentSourceKind
    let originPath: String?
    let workspaceID: WorkspaceID?
    let draftGeneration: Int
}

private struct ChatLandingComposer: View {
    @Binding var draft: String
    @Binding var attachments: [ConversationAttachment]
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let resetGeneration: Int
    let addProject: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            ChatRoundedComposer(
                draft: $draft,
                attachments: $attachments,
                model: model,
                backend: backend,
                placeholder: "What do you want to work on?",
                autoFocus: true,
                resetGeneration: resetGeneration
            )
            // Dropped rather than disabled where the Workspace has a single
            // implied root: a control offering one answer is noise, and the row
            // would otherwise claim a decision the user does not have.
            if !WorkspaceShape.of(projects: model.projects).collapsesRootChooser {
                ChatWorkingRootMenu(model: model, addProject: addProject)
            }
        }
    }
}

/// The quiet project / environment / branch row beneath the composer.
///
/// Only mounted in the multi-project shape; see `ChatLandingComposer`.
private struct ChatWorkingRootMenu: View {
    @Bindable var model: ChatWorkspaceModel
    let addProject: () -> Void

    var body: some View {
        HStack {
            if model.isPreparingEngines {
                // Static stand-in while loading; see the composer control bar.
                Label(model.draftWorkingRootName, systemImage: "folder")
                    .fixedSize()
            } else {
                workingRootMenu
            }
            Spacer()
        }
        .appFont(.callout)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 4)
    }

    private var workingRootMenu: some View {
        Menu {
                Button {
                    model.selectDraftWorkingRoot(.workspaceFiles)
                } label: {
                    workingRootMenuLabel(
                        "Workspace files",
                        selected: model.draftWorkingRoot == .workspaceFiles
                    )
                }

                ForEach(model.projects) { member in
                    let selection = WorkingRootSelection.project(
                        projectID: member.project.projectID,
                        sourceID: member.source.projectSourceID
                    )
                    Button {
                        model.selectDraftWorkingRoot(selection)
                    } label: {
                        workingRootMenuLabel(
                            member.project.displayName,
                            selected: model.draftWorkingRoot == selection
                        )
                    }
                }

                Divider()
                Button("Add Project…", systemImage: "plus", action: addProject)
            } label: {
                Label(model.draftWorkingRootName, systemImage: "folder")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            // SwiftUI can flatten a borderless Menu into anonymous AX content
            // on hosted runners. Force a stable container without exposing the
            // static loading stand-in as an actionable chooser.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(AccessibilityID.chatWorkingRootMenu)
    }
}

/// Attachment-style "+" affordance for adding content to a message.
private struct ChatLandingPlusMenu: View {
    @Bindable var model: ChatWorkspaceModel
    let attachImage: () -> Void

    var body: some View {
        Menu {
            // Present unconditionally and merely disabled: rebuilding the item
            // list when the engine changes makes AppKit flash the popup window
            // (see the control-bar comment above).
            Button("Attach Image…", systemImage: "photo", action: attachImage)
                .disabled(!model.canAttachImages)
                .help(model.attachDisabledReason ?? "Attach an image to this message")
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 28, height: 28)
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Evidence expectation")
    }
}

/// The composer's session selector: one pill opening a popover that holds the
/// engine list — with inline status and sign-in/install actions — and the
/// selected engine's Model/Effort/Mode choices for new sessions. A Button +
/// popover rather than an NSMenu because the rows are richer than menu items
/// (status subtitles, trailing action buttons, pushed option pages), at the
/// stated cost of losing NSMenu's free keyboard navigation.
private struct ChatSessionSelector: View {
    @Bindable var model: ChatWorkspaceModel
    let backend: DiffReviewModel
    // Deliberately not @FocusedValue: opening the popover can steal key
    // status, at which point focused values resolve to nil and the row goes
    // dead. Environment survives the popup.
    @Environment(OttamAppRuntime.self) private var runtime: OttamAppRuntime?
    @Environment(\.openWindow) private var openWindow

    private enum Page: Hashable {
        case root
        case model
        case effort
        case mode
    }

    /// What the hover highlight is keyed by. Deliberately not the row's label:
    /// labels are not unique within a page — an agent that advertises its own
    /// "Default" option next to the resolved default row gives two rows the
    /// same text — and keying by text lit both of them at once.
    private enum RowID: Hashable {
        case engine(EngineID)
        /// One option row; `nil` is the synthesized "agent default" row.
        case option(String?)
        /// A row that pushes `page`.
        case navigate(Page)
        case manageAgents
    }

    @State private var isPresented = false
    @State private var page = Page.root
    @State private var hoveredRow: RowID?
    /// The filter on a long option page; cleared whenever the page changes so
    /// a search never carries over to the next page or the next opening.
    @State private var optionQuery = ""

    var body: some View {
        Button {
            page = .root
            isPresented = true
        } label: {
            ChatLandingPill(systemImage: "gearshape", text: pillText)
        }
        .buttonStyle(.plain)
        .fixedSize()
        .help("Agent, model, effort, and mode for new sessions")
        .accessibilityIdentifier(AccessibilityID.chatEngineMenu)
        // `.top`: the composer sits in the window's lower half, so the popover
        // opens upward — same reasoning as the sidebar identity popover.
        .popover(isPresented: $isPresented, arrowEdge: .top) {
            popoverContent
        }
    }

    /// "Claude Code · Opus · High": engine first, then the session options
    /// worth a glance. Unresolved defaults stay off the pill — "Default"
    /// says nothing — and the mode appears only when explicitly chosen, so
    /// the everyday state stays short.
    private var pillText: String {
        if model.selectedAgentRemoved { return "Agent removed" }
        if model.selectedProfileRemoved { return "Agent removed · Choose a profile" }
        guard let engine = model.selectedEngine else { return "Custom" }
        var parts = [engine.displayName]
        if !model.availableModels.isEmpty, !isDefaultish(model.selectedModelLabel) {
            parts.append(model.selectedModelLabel)
        }
        if !model.availableThoughtLevels.isEmpty, !isDefaultish(model.selectedThoughtLevelLabel) {
            parts.append(model.selectedThoughtLevelLabel)
        }
        // A conversation always has a confirmed mode; name it only when it
        // is not the engine's default, like model and effort.
        if let modeID = model.effectiveModeID, modeID != model.selectedEngine?.defaultModeID {
            parts.append(model.selectedModeLabel)
        }
        return parts.joined(separator: " · ")
    }

    private func isDefaultish(_ label: String) -> Bool {
        label.localizedCaseInsensitiveContains("default")
    }

    /// The Mode page's caption: why the last change failed, else when the
    /// picked mode takes effect.
    private var modeNote: String? {
        if let error = model.selectedModeErrorText { return error }
        if model.hasPendingMode { return "Applies to your next message" }
        if model.modeChangeWaitsForNextTurn { return "Applies to next turn" }
        return nil
    }

    private var popoverContent: some View {
        Group {
            switch page {
            case .root:
                rootPage
            case .model:
                optionPage(
                    title: "Model",
                    defaultID: model.selectedEngine?.defaultModelID,
                    defaultName: model.defaultModelDisplayName,
                    options: model.availableModels.map { ($0.modelID, $0.displayName) },
                    selectedID: model.selectedModelID
                ) { model.selectedModelID = $0 }
            case .effort:
                optionPage(
                    title: "Effort",
                    defaultID: model.defaultThoughtLevelID,
                    defaultName: model.defaultThoughtLevelDisplayName,
                    options: model.availableThoughtLevels.map { ($0.id, $0.displayName) },
                    selectedID: model.effectiveThoughtLevelID
                ) { model.selectedThoughtLevelID = $0 }
            case .mode:
                VStack(alignment: .leading, spacing: 0) {
                    optionPage(
                        title: "Mode",
                        defaultID: model.selectedEngine?.defaultModeID,
                        defaultName: model.defaultModeDisplayName,
                        options: model.availableModes.map { ($0.id, $0.displayName) },
                        selectedID: model.effectiveModeID
                    ) { modeID in
                        model.selectMode(modeID)
                        Task { await model.applyPendingMode(backend: backend) }
                    }
                    .disabled(model.isWritingMode)
                    if let note = modeNote {
                        Text(note)
                            .font(.caption)
                            .foregroundStyle(model.selectedModeErrorText == nil ? .secondary : Color.red)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(.horizontal, 8)
                            .padding(.top, 6)
                    }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        // Fixed width, not min/max: the pages swap in place, and a popover
        // that resizes on every push reads as broken.
        .frame(width: 280, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatEngineMenuPopover)
        .onChange(of: page) { optionQuery = "" }
        .task { await model.loadToolPermissions(backend: backend) }
    }

    private var rootPage: some View {
        VStack(alignment: .leading, spacing: 1) {
            menuHeader("Agent")
            // Installation belongs in Settings; the picker offers agents already present.
            ForEach(model.pickerEngines) { engine in
                engineRow(engine)
            }
            if !model.availableModels.isEmpty || !model.availableThoughtLevels.isEmpty
                || !model.availableModes.isEmpty || model.selectedEngineSupportsAlwaysAllow {
                Divider()
                    .padding(.vertical, 5)
                if !model.availableModels.isEmpty {
                    navRow("Model", page: .model, value: model.selectedModelLabel)
                }
                if !model.availableThoughtLevels.isEmpty {
                    navRow("Effort", page: .effort, value: model.selectedThoughtLevelLabel)
                }
                if !model.availableModes.isEmpty {
                    navRow("Mode", page: .mode, value: model.selectedModeLabel)
                }
                if model.selectedEngineSupportsAlwaysAllow {
                    alwaysAllowRow
                }
            }
            Divider()
                .padding(.vertical, 5)
            actionRow("Manage Agents…", id: .manageAgents) {
                isPresented = false
                runtime?.pendingSettingsPane = .agents
                openWindow(id: "settings")
            }
        }
    }

    /// Selecting an engine keeps the popover open: the option rows underneath
    /// are the engine's, and switching is usually followed by checking them.
    /// Leaf selections on the pushed pages close it instead — the choice is
    /// the errand.
    private func engineRow(_ engine: EngineSummary) -> some View {
        HStack(spacing: 8) {
            Button {
                model.selectEngine(engine.engineID, backend: backend)
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "checkmark")
                        .font(.system(size: 11, weight: .semibold))
                        .opacity(model.selectedEngine?.engineID == engine.engineID ? 1 : 0)
                        .frame(width: 13)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(engine.displayName)
                        if let status = statusLine(engine) {
                            Text(status)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(!engine.isUsable || model.selectedAgentRemoved)
            .accessibilityIdentifier(
                engine.runtime?.availability == .checking ? AccessibilityID.chatEngineChecking : ""
            )
            // A separate control, not a selectable row that quietly opens a
            // browser: choosing an engine and fixing it are different
            // intentions, and the trailing button carries the second one.
            engineAction(engine)
        }
        .rowHover(.engine(engine.engineID), in: $hoveredRow, highlighted: engine.isUsable)
    }

    @ViewBuilder
    private func engineAction(_ engine: EngineSummary) -> some View {
        if needsSignIn(engine) {
            Button(engine.authState == .expired ? "Sign In Again" : "Sign In") {
                isPresented = false
                model.signIn.signIn(engine: engine)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(!model.signIn.canStartOrResume(AgentID(engine.engineID.rawValue)))
        } else if EngineRuntimeProblem(engine: engine) != nil, let install = runtime?.runtimeInstall {
            Button(install.state.actionTitle) {
                install.install()
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(install.state.isInFlight || install.state.phase == .ready)
        }
    }

    /// The actionable half of the engine's state, as a subtitle. Runtime
    /// first: an engine whose Node is missing reports `missing` install state
    /// too, and "Node required" is the half worth reading.
    private func statusLine(_ engine: EngineSummary) -> String? {
        if let problem = EngineRuntimeProblem(engine: engine) {
            return problem.menuSuffix
        }
        if engine.installState == .missing || engine.installState == .unsupported {
            return engine.installState.rawValue.capitalized
        }
        if engine.authState == .missing || engine.authState == .expired {
            return "Sign in required"
        }
        return engine.runtime?.availability == .checking ? "Checking…" : nil
    }

    /// Only where signing in is the thing standing in the way. An engine that
    /// isn't installed has a different problem, and offering a login for it
    /// would send the user down the wrong path.
    private func needsSignIn(_ engine: EngineSummary) -> Bool {
        guard engine.installState != .missing, engine.installState != .unsupported,
              EngineRuntimeProblem(engine: engine) == nil
        else {
            return false
        }
        return engine.authState == .missing || engine.authState == .expired
    }

    // MARK: Option pages

    /// A leading row for "run whatever this agent defaults to", then the
    /// agent's own options.
    ///
    /// That row is dropped only when it would read exactly like the option row
    /// for the same id; `ChatSelectorDefaultRow` owns that rule.
    private func optionPage(
        title: String,
        defaultID: String?,
        defaultName: String?,
        options: [(String, String)],
        selectedID: String?,
        select: @escaping (String?) -> Void
    ) -> some View {
        let advertisedDefault = ChatSelectorDefaultRow.absorbingOption(
            defaultID: defaultID,
            defaultName: defaultName,
            options: options.map { (id: $0.0, label: $0.1) }
        )
        let searchable = ChatSelectorOptionSearch.isSearchable(optionCount: options.count)
        let visible = searchable
            ? ChatSelectorOptionSearch.matches(options, query: optionQuery, id: \.0, label: \.1)
            : options
        return VStack(alignment: .leading, spacing: 1) {
            backHeader(title)
            if searchable {
                TextField("Search", text: $optionQuery)
                    .textFieldStyle(.roundedBorder)
                    // Return takes the top match, so a few letters and Return
                    // is a whole selection without reaching for the pointer.
                    // Matched again here rather than reusing `visible`: the
                    // field keeps an earlier render's closure, which would
                    // pick from a query a few keystrokes old.
                    .onSubmit {
                        let matches = ChatSelectorOptionSearch.matches(
                            options, query: optionQuery, id: \.0, label: \.1
                        )
                        guard let (id, _) = matches.first else { return }
                        isPresented = false
                        select(id == advertisedDefault ? nil : id)
                    }
                    .padding(.horizontal, 4)
                    .padding(.bottom, 6)
                    .background(PopoverSearchFieldFocus())
            }
            // The synthesized default row belongs to the unfiltered page; a
            // search is for a specific option.
            if advertisedDefault == nil, optionQuery.isEmpty || !searchable {
                optionRow(
                    ChatSelectorDefaultRow.label(defaultName),
                    id: .option(nil),
                    isSelected: selectedID == nil
                ) {
                    select(nil)
                }
                Divider()
                    .padding(.vertical, 5)
            }
            if searchable {
                // A fixed height rather than a cap: filtering then never
                // resizes the popover under the pointer.
                ScrollViewReader { proxy in
                    ScrollView {
                        optionRows(visible, advertisedDefault: advertisedDefault, selectedID: selectedID, select: select)
                        if visible.isEmpty {
                            Text("No Matches")
                                .foregroundStyle(.secondary)
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 12)
                        }
                    }
                    .frame(height: 320)
                    .onAppear {
                        // Open on the current choice, not the top of the list.
                        if let selectedID {
                            proxy.scrollTo(RowID.option(selectedID), anchor: .center)
                        }
                    }
                }
            } else {
                optionRows(visible, advertisedDefault: advertisedDefault, selectedID: selectedID, select: select)
            }
        }
    }

    private func optionRows(
        _ options: [(String, String)],
        advertisedDefault: String?,
        selectedID: String?,
        select: @escaping (String?) -> Void
    ) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            ForEach(options, id: \.0) { id, label in
                optionRow(
                    label,
                    id: .option(id),
                    // The collapsed-into row stands in for the one that was
                    // dropped: it reads as selected while nothing is pinned, and
                    // choosing it clears the pin rather than freezing today's
                    // default id. Rows that did not absorb a default row keep
                    // ordinary pin semantics.
                    isSelected: id == advertisedDefault
                        ? selectedID == nil || selectedID == id
                        : selectedID == id
                ) {
                    select(id == advertisedDefault ? nil : id)
                }
                .id(RowID.option(id))
            }
        }
    }

    private func backHeader(_ title: String) -> some View {
        Button {
            page = .root
        } label: {
            HStack(spacing: 4) {
                Image(systemName: "chevron.left")
                    .font(.system(size: 10, weight: .semibold))
                Text(title)
                    .font(.caption)
            }
            .foregroundStyle(.secondary)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 8)
        .padding(.bottom, 2)
    }

    private func optionRow(
        _ label: String,
        id: RowID,
        isSelected: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button {
            isPresented = false
            action()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "checkmark")
                    .font(.system(size: 11, weight: .semibold))
                    .opacity(isSelected ? 1 : 0)
                    .frame(width: 13)
                Text(label)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .rowHover(id, in: $hoveredRow)
        }
        .buttonStyle(.plain)
    }

    // MARK: Shared row chrome

    /// Agents with a bypass mode of their own (Claude, Codex) offer it under
    /// Mode instead, so the row shows only for the rest.
    private var alwaysAllowRow: some View {
        Toggle(isOn: Binding(
            get: { model.alwaysAllowToolActions },
            set: { enabled in
                model.setAlwaysAllowToolActions(enabled, backend: backend)
            }
        )) {
            Text("Always allow tool actions")
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .toggleStyle(.switch)
        .controlSize(.mini)
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .help("Ottam approves this chat's tool requests, one at a time. New chats start with the last value set.")
    }

    private func menuHeader(_ title: String) -> some View {
        Text(title)
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.bottom, 2)
    }

    private func navRow(_ label: String, page destination: Page, value: String) -> some View {
        Button { page = destination } label: {
            HStack(spacing: 6) {
                Text(label)
                Spacer(minLength: 12)
                Text(value)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(.tertiary)
            }
            .rowHover(.navigate(destination), in: $hoveredRow)
        }
        .buttonStyle(.plain)
    }

    private func actionRow(_ label: String, id: RowID, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Text(label)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .rowHover(id, in: $hoveredRow)
        }
        .buttonStyle(.plain)
    }
}

private extension View {
    /// The popover rows' shared hover chrome: padding, the highlight fill, and
    /// the hover tracking that drives it, in one place so the fill and the key
    /// it is matched on can never disagree.
    func rowHover<ID: Hashable>(
        _ id: ID,
        in hovered: Binding<ID?>,
        highlighted: Bool = true
    ) -> some View {
        padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(.primary.opacity(hovered.wrappedValue == id && highlighted ? 0.09 : 0))
            )
            .contentShape(.rect)
            .onHover { isHovering in
                if isHovering {
                    hovered.wrappedValue = id
                } else if hovered.wrappedValue == id {
                    hovered.wrappedValue = nil
                }
            }
    }
}

/// Puts typing in the popover's search field as soon as its page opens.
///
/// SwiftUI focus cannot do this: a popover's window reports itself key while
/// its parent is, so `.focused` "succeeds" and the keystrokes still go to the
/// composer behind it. Making the field first responder of a popover window
/// that is ordered key is what a click in the field does.
private struct PopoverSearchFieldFocus: NSViewRepresentable {
    final class RequestView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            // Next turn: the field beside this view is not in the hierarchy yet.
            DispatchQueue.main.async { [weak self] in
                guard let window = self?.window, let content = window.contentView,
                      let field = Self.firstTextField(in: content) else { return }
                window.makeKeyAndOrderFront(nil)
                window.makeFirstResponder(field)
            }
        }

        private static func firstTextField(in view: NSView) -> NSTextField? {
            if let field = view as? NSTextField, field.isEditable { return field }
            for subview in view.subviews {
                if let field = firstTextField(in: subview) { return field }
            }
            return nil
        }
    }

    func makeNSView(context: Context) -> RequestView { RequestView() }
    func updateNSView(_ nsView: RequestView, context: Context) {}
}

/// A compact rounded control used inside the landing composer's control bar.
/// The composer's queue: one pill opening a popover that lists the messages
/// waiting for the working turn, each with Send Now and Remove.
///
/// A Button + popover for the same reason `ChatSessionSelector` is one — the
/// rows carry body text and trailing action buttons rather than being plain
/// choices — and at the same stated cost, that a popover has none of NSMenu's
/// free keyboard navigation. Escape dismisses it and the rows are ordinary
/// buttons, so they stay tab-reachable.
private struct ChatQueuedMessagesButton: View {
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    @State private var isPresented = false

    var body: some View {
        Button {
            isPresented = true
        } label: {
            // Deliberately not `ChatLandingPill`, which draws no capsule and
            // would make this a pixel twin of the engine selector beside it.
            // That control is a setting; this is state, and a filled badge is
            // how the platform says so — Mail's unread count, Xcode's issue
            // count. No chevron for the same reason: a chevron promises a menu.
            // No glyph either, because "2 queued" already says it and the row
            // is crowded enough.
            //
            // Material fill under a tinted stroke, matching the permission-jump
            // capsule: the two are the same thing — a capsule in the chat
            // surface counting what is waiting on the reader — and should read
            // as one family. Accent rather than that capsule's orange, which
            // this app spends on "something needs fixing" and a queue is not
            // that.
            Text(pillText)
                .appFont(.callout)
                .lineLimit(1)
                .foregroundStyle(.tint)
                .padding(.horizontal, 10)
                .padding(.vertical, 3)
                .background(.regularMaterial, in: Capsule())
                .overlay(Capsule().stroke(.tint.opacity(0.35)))
                // Restores the height the row's other controls claim, so the
                // badge reads smaller than them without being harder to hit.
                .padding(.vertical, 3)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .fixedSize()
        .help("Messages waiting for this turn to finish")
        .accessibilityIdentifier(AccessibilityID.chatQueuedMessages)
        // The count as a value, not just as the label's text: it is the one
        // thing about this control worth asserting, and a label derived from a
        // nested view is not a contract.
        .accessibilityValue(pillText)
        // `.top`: the composer sits in the window's lower half, so the popover
        // opens upward — the same reasoning as the session selector beside it.
        .popover(isPresented: $isPresented, arrowEdge: .top) {
            popoverContent
        }
    }

    private var pillText: String {
        let count = model.queuedMessages.count
        return count == 1 ? "1 queued" : "\(count) queued"
    }

    private var popoverContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(model.queuedMessages.enumerated()), id: \.element.id) { index, message in
                if index > 0 {
                    Divider()
                }
                row(message)
            }
        }
        .padding(6)
        // Fixed, not min/max: rows swap in place as the queue drains, and a
        // popover that resizes under the pointer is hard to aim at.
        .frame(width: 320)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.chatQueuedMessagesPopover)
    }

    private func row(_ message: QueuedMessage) -> some View {
        HStack(alignment: .top, spacing: 6) {
            // An attachment-only message has no body to show, so name it rather
            // than rendering an empty row the reader cannot tell apart.
            Text(message.body.isEmpty ? "Image" : message.body)
                .appFont(.callout)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)

            Button {
                sendNow(message)
            } label: {
                Image(systemName: "arrow.up.circle")
                    .font(.system(size: 13))
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help("Send Now")
            .accessibilityLabel("Send Now")
            .accessibilityIdentifier(AccessibilityID.chatQueuedMessageSendNow)

            Button {
                remove(message)
            } label: {
                Image(systemName: "trash")
                    .font(.system(size: 12))
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help("Remove")
            .accessibilityLabel("Remove")
            .accessibilityIdentifier(AccessibilityID.chatQueuedMessageRemove)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 7)
    }

    /// Promotes one message past the rest. It leaves by the ordinary send path,
    /// so with the run still working this steers.
    private func sendNow(_ message: QueuedMessage) {
        guard let conversationID = model.selectedConversationID else { return }
        isPresented = false
        Task {
            await model.sendQueuedNow(message.id, in: conversationID, backend: backend)
        }
    }

    private func remove(_ message: QueuedMessage) {
        guard let conversationID = model.selectedConversationID else { return }
        model.removeQueued(message.id, in: conversationID)
        // The pill unmounts with the last message; dismiss so the popover does
        // not outlive the control it is anchored to.
        if model.queuedMessages.isEmpty {
            isPresented = false
        }
    }
}

private struct ChatLandingPill: View {
    let systemImage: String
    let text: String

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: systemImage)
                .font(.system(size: 12))
            Text(text)
                .appFont(.callout)
                .lineLimit(1)
            Image(systemName: "chevron.down")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(.tertiary)
        }
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .contentShape(Rectangle())
    }
}

// MARK: - Center pane

/// Shared reading-column metrics for the thread and composer. 760 is the app's
/// established reading width (Chat landing, dashboard); both the thread column
/// and the composer center on it so they stay aligned at any window width.
enum ChatLayout {
    static let contentWidth: CGFloat = 760
}

private struct ChatCenterPane: View {
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    @State private var draft = ""
    // Attachments are per-composer, exactly like the draft: the landing and
    // the thread each own theirs, so one never renders the other's cards.
    @State private var attachments: [ConversationAttachment] = []
    /// Quote replies staged from the transcript, sent with the next message.
    @State private var quotes: [QuoteReply.Comment] = []

    var body: some View {
        // The thread title lives in the toolbar strip (see ChatWorkbench's overlay),
        // so the pane is just the thread — no second chrome band.
        ProposedSizeLayout {
            VStack(spacing: 0) {
                if model.selectedConversationID != nil, let error = model.transcriptLoadError {
                    HStack {
                        Label("Could not load saved transcript", systemImage: "exclamationmark.triangle")
                            .help(error)
                        Spacer()
                        Button("Retry") { Task { await model.retryTranscript(backend: backend) } }
                    }
                    .appFont(.callout)
                    .padding(12)
                }
                if let errorText = model.errorText ?? model.sessionNoticeText {
                    Label(errorText, systemImage: "exclamationmark.triangle")
                        .accessibilityIdentifier(AccessibilityID.chatSessionNotice)
                        .appFont(.callout)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: ChatLayout.contentWidth, alignment: .leading)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.horizontal, 22)
                        .padding(.vertical, 10)

                    Divider()
                }

                if let noticeText = model.visibleNoticeText {
                    Label(noticeText, systemImage: "info.circle")
                        .appFont(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: ChatLayout.contentWidth, alignment: .leading)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .padding(.horizontal, 22)
                        .padding(.vertical, 10)

                    Divider()
                }

                ChatLooseThread(
                    model: model,
                    backend: backend,
                    contentWidth: ChatLayout.contentWidth,
                    onSelectStarter: { draft = $0 },
                    onQuoteReply: { quotes.append($0) }
                ) {
                    ChatComposer(
                        draft: $draft,
                        attachments: $attachments,
                        quotes: $quotes,
                        model: model,
                        backend: backend,
                        contextID: model.selectedConversationID
                    )
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .toolbarScrollEdge()
        .onChange(of: model.selectedConversationID) {
            abandonDraft()
        }
        .onDisappear {
            abandonDraft()
        }
    }

    private func abandonDraft() {
        let abandoned = attachments
        draft = ""
        quotes.removeAll()
        attachments.removeAll()
        guard !abandoned.isEmpty else { return }
        Task {
            for attachment in abandoned {
                await model.discardAttachment(attachment, backend: backend)
            }
        }
    }
}

// Closure-based and stateless on purpose: stage 3 of the markdown engine hosts
// this card in the engine surface (`HostedPermissionCard`), where height must
// be a pure function of the payload — so the disclosure state is an input, and
// toggling it is a request to the owner, not a local `@State` flip.
struct ChatPermissionCard: View {
    let permission: ChatPendingPermission
    let showsDetails: Bool
    let onToggleDetails: () -> Void
    /// Called with the chosen option ID, or nil for cancel.
    let onResolve: (String?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "lock.shield")
                    .foregroundStyle(.orange)
                Text("Approval required")
                    .appFont(.body, .bold)
                    .accessibilityIdentifier(AccessibilityID.chatPermissionCard)
                // A subagent's request: say which one asks, since its work
                // shows in its own tab rather than here.
                if permission.request.subagentThreadID != nil {
                    Text(permission.request.subagentName.map { "Asked by \($0)" } ?? "Asked by a subagent")
                        .appFont(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .accessibilityIdentifier(AccessibilityID.chatPermissionSubagent)
                        .accessibilityValue(permission.request.subagentThreadID ?? "")
                }
                Spacer(minLength: 0)
            }
            Text(toolSummary)
                .appFont(.body)
                .fixedSize(horizontal: false, vertical: true)
            DisclosureGroup(
                "Request details",
                isExpanded: Binding(get: { showsDetails }, set: { _ in onToggleDetails() })
            ) {
                ScrollView(.horizontal) {
                    Text(toolDetails)
                        .appFont(.codeMeta)
                        .textSelection(.enabled)
                        .padding(.top, 4)
                }
            }
            .appFont(.callout)
            HStack(spacing: 8) {
                ForEach(permission.request.options, id: \.optionID) { option in
                    if option.kind == .allowOnce || option.kind == .allowAlways {
                        Button(option.name) {
                            onResolve(option.optionID)
                        }
                        .buttonStyle(.borderedProminent)
                    } else {
                        Button(option.name) {
                            onResolve(option.optionID)
                        }
                        .buttonStyle(.bordered)
                    }
                }
                Button("Cancel") {
                    onResolve(nil)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(12)
        .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(.orange.opacity(0.35)))
    }

    private var toolSummary: String {
        chatPermissionToolSummary(permission.request.toolCall)
    }

    private var toolDetails: String {
        chatPermissionToolDetails(permission.request.toolCall)
    }
}

/// The pretty form of a permission's tool call, for the details disclosure.
func chatPermissionToolDetails(_ toolCall: JSONValue) -> String {
    guard let data = try? JSONEncoder().encode(toolCall),
          let value = String(data: data, encoding: .utf8)
    else { return "{}" }
    return value
}

// MARK: - Loose mode (conversation)

private struct ChatLooseThread<Composer: View>: View {
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let contentWidth: CGFloat
    let onSelectStarter: (String) -> Void
    let onQuoteReply: (QuoteReply.Comment) -> Void
    let composer: Composer
    /// Mirrors the engine's follow state (`onFollowsBottomChange`): true while
    /// the transcript is glued to the bottom of the stream.
    @State private var pinnedToBottom = true
    @State private var surface = MacAgentTranscriptView.Controller()
    @State private var transcriptPresentation = ChatTranscriptPresentationCoordinator()
    /// Measured height of the floating composer, including its column padding;
    /// the thread's bottom inset so the last turn scrolls clear of the card.
    @State private var composerHeight: CGFloat = 0
    /// The question the panel shows, trailing the model's so each change runs
    /// in an explicit animation. A value-keyed `.animation` on the stack never
    /// reached the panel's insertion or removal: it snapped in and out.
    @State private var shownQuestion: ChatPendingQuestion?
    /// The plan the plan panel shows, trailing the model's for the same reason.
    @State private var shownPlan: ChatPendingPermission?
    /// The managed-runtime install is process-wide, so the recovery card shares
    /// whatever the Agents pane or another window already started.
    @Environment(OttamAppRuntime.self) private var appRuntime: OttamAppRuntime?

    init(
        model: ChatWorkspaceModel,
        backend: DiffReviewModel,
        contentWidth: CGFloat,
        onSelectStarter: @escaping (String) -> Void,
        onQuoteReply: @escaping (QuoteReply.Comment) -> Void,
        @ViewBuilder composer: () -> Composer
    ) {
        self.model = model
        self.backend = backend
        self.contentWidth = contentWidth
        self.onSelectStarter = onSelectStarter
        self.onQuoteReply = onQuoteReply
        self.composer = composer()
    }

    var body: some View {
        Group {
            // Keep the normalized transcript visible while lifecycle detail refreshes.
            if model.selectedConversationID != nil,
               !model.isLoadingDetail,
               !(model.selectedDetail?.messages.isEmpty ?? true)
                   || !model.transcriptStore.items.isEmpty
                   || model.selectedLiveRun != nil
                   || !model.selectedPendingPermissions.isEmpty {
                // AppKit owns the virtualized transcript viewport. The snapshot
                // is built here so every model value a semantic card reads is
                // tracked and reconciles through the renderer-neutral plan.
                MacAgentTranscriptView(
                    snapshot: model.transcriptSnapshot(runtimeInstall: appRuntime?.runtimeInstall.state),
                    actions: transcriptActions,
                    controller: surface,
                    presentation: transcriptPresentation,
                    transcriptStore: model.transcriptStore,
                    viewSessions: model.transcriptCache,
                    isLoadingOlderRows: model.isLoadingOlderMessages,
                    onFollowsBottomChange: { pinnedToBottom = $0 },
                    onNearTop: {
                        // Load Earlier, without a button: reaching the top of the
                        // loaded history fetches the page before it, and the engine
                        // keeps the first old element where the reader left it.
                        guard model.canLoadOlderMessages, !model.isLoadingOlderMessages else { return }
                        Task { await model.loadOlderMessages(backend: backend) }
                    },
                    quoteReply: TranscriptQuoteReplyConfiguration(
                        agentName: model.selectedEngine?.displayName ?? "agent",
                        onAdd: { quote, comment in
                            onQuoteReply(QuoteReply.Comment(quote: quote, comment: comment))
                            model.focusComposer()
                        }
                    ),
                    bottomOverlayInset: composerHeight
                )
                // Under the titlebar: the scroll view insets its content by the
                // safe area itself, and AppKit draws the top scroll edge effect
                // only for a scroll view that extends beneath the toolbar
                // (SwiftUI does that for its own ScrollView only).
                .ignoresSafeArea(.container, edges: [.top, .bottom])
                .background {
                    StreamingCommitLifecycleView(coordinator: model.transcriptStore.streamCommits)
                        .frame(width: 0, height: 0)
                        .accessibilityHidden(true)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay(alignment: .topLeading) {
                    TranscriptScrollProbes(surface: surface)
                }
                .safeAreaInset(edge: .top) {
                    if transcriptPresentation.hasFailedPublication {
                        HStack {
                            Label("Could not display transcript", systemImage: "exclamationmark.triangle")
                            Spacer()
                            Button("Retry") { transcriptPresentation.retryFailedPublication() }
                        }
                        .appFont(.callout)
                        .padding(12)
                        .background(.regularMaterial)
                    }
                }
            } else {
                ScrollView {
                    ChatMeasuredColumn(contentWidth: contentWidth) {
                        if model.isLoadingDetail {
                            // Selection landed but the detail is still in flight: hold the
                            // destination with a quiet spinner, then flip once to content.
                            ChatDetailLoadingState()
                        } else {
                            ChatEmptyState(prompts: model.starterPrompts, onSelect: onSelectStarter)
                        }
                    }
                }
                .safeAreaPadding(.bottom, composerHeight)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        // The composer floats over the thread as a glass card, Messages-style:
        // no bar, no scroll edge effect, the thread scrolls straight under it.
        // Its measured height feeds the transcript's bottom inset so the last
        // turn can clear it. (A split-item bottom accessory was the previous
        // host; on macOS 27 every accessory draws a hard-edged frosted band
        // whatever style it asks for, and there is no "none".)
        .overlay(alignment: .bottom) {
            // A selected conversation is one presentation unit: do not mount a
            // half-populated composer while its detail (including the working
            // root shown beneath the editor) is still in flight. The centered
            // detail spinner owns this interval; transcript and composer land
            // together once the canonical detail is available.
            if showsComposer {
                floatingComposer
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: {
                        composerHeight = $0
                    }
            }
        }
        .onChange(of: showsComposer) { _, shows in
            if !shows { composerHeight = 0 }
        }
        .onChange(of: model.selectedPendingQuestions.first?.id, initial: true) { old, new in
            let question = model.selectedPendingQuestions.first
            // Already pending when the thread mounts: there, not arriving.
            if old == new {
                shownQuestion = question
            } else {
                withAnimation(.snappy) { shownQuestion = question }
            }
        }
        .onChange(of: model.selectedPendingPlans.first?.id, initial: true) { old, new in
            let permission = model.selectedPendingPlans.first
            if old == new {
                shownPlan = permission
            } else {
                withAnimation(.snappy) { shownPlan = permission }
            }
            // The agent is blocked until the plan is answered, so its tab
            // comes forward, over whichever tool was showing.
            if let permission, let plan = permission.plan, let conversationID = permission.request.conversationID {
                model.openPlan(conversationID, plan)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var transcriptActions: ChatTranscriptActions {
        let workspaceID = model.workspaceID
        let conversationID = model.selectedConversationID
        return ChatTranscriptActions(
            resolveDelivery: { input, retry in
                Task { await model.resolveDelivery(input, retry: retry, backend: backend) }
            },
            retryFailedRun: { runID in
                Task { await model.retryFailedRun(runID, backend: backend) }
            },
            checkRuntime: {
                Task { await model.refreshEngines(backend: backend, refreshRuntime: true) }
            },
            installRuntime: { appRuntime?.runtimeInstall.install() },
            signIn: { recovery in model.signIn.signIn(engine: recovery.engine) },
            cancelSignIn: { recovery in model.signIn.cancel(recovery.agentID) },
            signInMcp: { serverID in appRuntime?.mcpSignIn.signIn(serverID) },
            cancelMcpSignIn: { serverID in appRuntime?.mcpSignIn.cancel(serverID) },
            resolvePermission: { permission, optionID in
                model.resolvePermission(permission, optionID: optionID)
            },
            stopBackgroundTask: { runID, asyncTaskID in
                Task { await model.stopBackgroundTask(runID: runID, asyncTaskID: asyncTaskID, backend: backend) }
            },
            openSubagentThread: { thread in
                guard let conversationID else { return }
                model.openSubagentThread(conversationID, thread)
            },
            openPlan: { plan in
                guard let conversationID else { return }
                model.openPlan(conversationID, plan)
            },
            openConversation: { child in model.select(child, backend: backend) },
            continueDelegation: {
                Task { await model.continueDelegation(backend: backend) }
            },
            sendToParent: {
                Task { await model.sendToParent(backend: backend) }
            },
            openFile: { url in
                guard let conversationID else { return }
                model.openFile(conversationID, url)
            },
            readPayload: { id, revision, part, offset in
                guard let workspaceID, let conversationID else { throw CancellationError() }
                #if DEBUG
                if let milliseconds = ProcessInfo.processInfo.environment["OTTAM_TEST_PAYLOAD_READ_DELAY_MS"].flatMap(Int.init), milliseconds > 0 {
                    try await Task.sleep(for: .milliseconds(min(milliseconds, 5000)))
                }
                #endif
                return try await backend.readConversationTranscriptPayload(.init(workspaceID: workspaceID, conversationID: conversationID, payloadID: id, revision: revision, part: part, offset: offset))
            },
            readBody: { id, revision, offset in
                guard let workspaceID, let conversationID else { throw CancellationError() }
                return try await backend.readConversationTranscriptBody(workspaceID: workspaceID, conversationID: conversationID, itemID: id, revision: revision, offset: offset)
            }
        )
    }

    /// A conversation waits for its detail so the transcript, the editor and the
    /// working root arrive together. Mounting the composer early for a live run
    /// bought reachable stop controls at the cost of a bar that grows by the
    /// working-root row when detail lands, displacing the thread a second time;
    /// the stop control is one click away again as soon as the load settles.
    private var showsComposer: Bool {
        !model.isLoadingDetail
    }

    private var floatingComposer: some View {
        ChatComposerColumn {
            // One container, its spacing the stack's: the two cards share a
            // render pass and stay apart at rest. The panel's glass
            // materializes in its own place rather than growing out of the
            // composer, which would carry its content over the composer.
            GlassEffectContainer(spacing: 8) {
                VStack(spacing: 8) {
                    if let question = shownQuestion, !model.isCommandPickerOpen {
                        ChatQuestionPanel(question: question) { outcome in
                            model.resolveQuestion(question, outcome)
                        }
                        .id(question.id)
                    }
                    if let permission = shownPlan, let plan = permission.plan, !model.isCommandPickerOpen {
                        ChatPlanPanel(
                            permission: permission,
                            plan: plan,
                            onResolve: { optionID in
                                model.resolvePermission(permission, optionID: optionID)
                            },
                            onRevise: { optionID in
                                model.resolvePermission(permission, optionID: optionID)
                                model.focusComposer()
                            },
                            onOpenPlan: {
                                guard let conversationID = permission.request.conversationID else { return }
                                model.openPlan(conversationID, plan)
                            }
                        )
                        .id(permission.id)
                    }
                    composer
                }
            }
        }
        // The pill rides the bar rather than the scroll view, because an overlay on
        // the scroll view is placed in its full frame — safe area and all — and lands
        // under the composer at the window's bottom edge. Anchored here and lifted
        // clear of the bar's top by its own height, it floats over the thread just
        // above the composer. Still an overlay, so it adds nothing to the height the
        // bar reports: a bar that outgrows its height proposal is the bug that started
        // all this (see `ProposedSizeLayout`), and a row in the bar's own stack, or a
        // `safeAreaInset`, would push back towards it.
        .overlay(alignment: .top) {
            permissionJumpPill
                .animation(.snappy, value: pinnedToBottom)
                .alignmentGuide(.top) { $0[.bottom] + 8 }
        }
    }

    @ViewBuilder
    private var permissionJumpPill: some View {
        // Only while there is something to go back to, and only once the reader has
        // left it. The pill carries no Allow/Deny: approving a command you cannot see
        // is the wrong affordance, so all it does is take you to the card.
        if !pinnedToBottom, !model.selectedPendingPermissions.isEmpty {
            Button {
                surface.scrollToBottom()
                // Set directly rather than waiting on the engine's callback, so
                // the pill leaves in the same frame as the scroll.
                pinnedToBottom = true
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "lock.shield")
                        .foregroundStyle(.orange)
                    Text(
                        model.selectedPendingPermissions.count == 1
                            ? "Approval required"
                            : "\(model.selectedPendingPermissions.count) approvals required"
                    )
                    .appFont(.callout)
                    Image(systemName: "chevron.down")
                        .appFont(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 7)
                .background(.regularMaterial, in: Capsule())
                .overlay(Capsule().stroke(.orange.opacity(0.35)))
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier(AccessibilityID.chatPermissionJump)
            .transition(.scale(scale: 0.9).combined(with: .opacity))
        }
    }

}

/// Placeholder while a selected conversation's detail is in flight. The spinner sits
/// behind a short grace period so fast loads flip straight to content without a
/// spinner blink; only genuinely slow (large-session) loads ever show it.
private struct ChatDetailLoadingState: View {
    @State private var spinnerVisible = false

    var body: some View {
        VStack {
            ProgressView()
                .controlSize(.small)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // Fill the scroll viewport so the spinner centers in the visible pane
        // rather than sitting at the top of a short content column.
        .containerRelativeFrame(.vertical)
        .opacity(spinnerVisible ? 1 : 0)
        .task {
            try? await Task.sleep(for: .milliseconds(150))
            guard !Task.isCancelled else { return }
            withAnimation(.easeIn(duration: 0.2)) {
                spinnerVisible = true
            }
        }
    }
}

private struct ChatEmptyState: View {
    let prompts: [String]
    let onSelect: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Start a conversation")
                    .appFont(.title, .bold)
                Text("Ask a question, plan work, or run an ad-hoc task while preserving the transcript, activity, and evidence.")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            LazyVGrid(columns: [GridItem(.adaptive(minimum: 240), spacing: 10)], alignment: .leading, spacing: 10) {
                ForEach(prompts, id: \.self) { prompt in
                    Button {
                        onSelect(prompt)
                    } label: {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Image(systemName: "text.bubble")
                                .font(.system(size: 10, weight: .semibold))
                            Text(prompt)
                                .appFont(.callout)
                                .lineLimit(2)
                                .fixedSize(horizontal: false, vertical: true)
                            Spacer(minLength: 0)
                        }
                        .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 5)
                    }
                    .buttonStyle(.bordered)
                    .help("Use starter")
                }
            }
        }
    }
}

/// Chat-style turn: user messages sit right-aligned in a rounded bubble, assistant
/// markdown renders flat on the shared canvas, and system notes are quiet centered
/// captions — no role labels or icons.
// MARK: - Anchored composer

private struct ChatMeasuredColumn<Content: View>: View {
    let contentWidth: CGFloat
    let content: Content

    init(contentWidth: CGFloat, @ViewBuilder content: () -> Content) {
        self.contentWidth = contentWidth
        self.content = content()
    }

    var body: some View {
        content
            .frame(maxWidth: contentWidth, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, 22)
            .padding(.vertical, 18)
    }
}

private struct ChatComposerColumn<Content: View>: View {
    /// How far the card overhangs the thread's reading column on each side: the
    /// editor inset plus NSTextContainer's default 5pt line fragment padding, so
    /// the draft's first glyph lines up with the transcript text above while the
    /// glass edge sits clear of it.
    static var overhang: CGFloat { ChatRoundedComposer.editorHorizontalPadding + 5 }
    /// Closest the card gets to the pane edge. Below the transcript's own 22pt
    /// gutter, so in a narrow window the card still overhangs the text column
    /// rather than going flush with it.
    static var minimumGutter: CGFloat { 10 }

    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    // The composer centers on the thread's reading column, widened by `overhang`
    // so the floating card frames the text instead of cropping it. The top
    // padding is the gap the last turn keeps above the card when the thread is
    // pinned to the bottom.
    var body: some View {
        content
            .frame(maxWidth: ChatLayout.contentWidth + 2 * Self.overhang, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.horizontal, Self.minimumGutter)
            .padding(.top, 10)
            .padding(.bottom, 18)
    }
}

private struct ChatComposer: View {
    @Binding var draft: String
    @Binding var attachments: [ConversationAttachment]
    @Binding var quotes: [QuoteReply.Comment]
    @Bindable var model: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let contextID: ConversationID?

    var body: some View {
        ChatRoundedComposer(
            draft: $draft,
            attachments: $attachments,
            quotes: $quotes,
            model: model,
            backend: backend,
            placeholder: model.selectedDetail == nil && !model.isLoadingDetail
                ? "What do you want to work on?" : "Continue the conversation…",
            contextID: contextID
        )
        .shadow(color: .black.opacity(0.14), radius: 12, x: 0, y: 6)
    }
}


/// Sizes to what the window offers, never to what the pane contains.
///
/// `safeAreaBar` stops honouring the height it is handed once the bar grows past
/// roughly a third of the container, and answers with the scroll content's full
/// height instead. Nothing above it clamps that, so the pane laid out at twice the
/// window and carried the composer off the bottom edge. An approval card stacked
/// above the composer was enough to trigger it; that card now renders inline in the
/// thread, so nothing in the bar is close to the threshold today. This stays as the
/// guard: re-proposing the exact offered size puts the scroll view back in charge of
/// its own overflow, which is where it belongs. Keep the bar's content short anyway.
struct ProposedSizeLayout: Layout {
    func sizeThatFits(
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) -> CGSize {
        proposal.replacingUnspecifiedDimensions()
    }

    func placeSubviews(
        in bounds: CGRect,
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) {
        for subview in subviews {
            subview.place(
                at: bounds.origin,
                anchor: .topLeading,
                proposal: ProposedViewSize(bounds.size)
            )
        }
    }
}

/// A measurement-only affordance: each press scrolls the transcript back one
/// viewport.
///
/// A scenario needs to put the reader in a reproducible scroll state — detached
/// mid-stream, or near the top so history pages in — and every route for
/// synthesizing the gesture from outside the process failed, two of them
/// silently. Pressing a control is the one path the harness drives reliably.
///
/// Deliberately not a user-visible control: it exists only in DEBUG builds
/// launched by the scenario runner, and it carries no label or hit area beyond
/// what accessibility needs to find and press it.
private struct TranscriptScrollProbes: View {
    let surface: MacAgentTranscriptView.Controller
    @State private var selectionCopied = false
    @State private var windowResized = false
    @State private var scrollState = "Check Scroll State"

    var body: some View {
        #if DEBUG
        if UITestPreparation.isActive {
            VStack(spacing: 0) {
                Button("Scroll Back") { surface.scrollBack(viewports: 1) }
                    .accessibilityIdentifier(AccessibilityID.chatScrollBackProbe)
                Button("Scroll Bottom") { surface.scrollToBottom() }
                    .accessibilityIdentifier(AccessibilityID.chatScrollBottomProbe)
                Button(scrollState) {
                    scrollState = surface.isFollowingBottomForUITest()
                        ? "Following Bottom"
                        : "Detached"
                }
                .accessibilityIdentifier(AccessibilityID.chatScrollStateProbe)
                Button(selectionCopied ? "Selection Copied" : "Select and Copy") {
                    selectionCopied = surface.selectAndCopyForUITest()
                }
                .accessibilityIdentifier(AccessibilityID.chatSelectionCopyProbe)
                Button(windowResized ? "Window Resized" : "Resize Transcript") {
                    Task { @MainActor in
                        await Task.yield()
                        windowResized = surface.resizeWindowForUITest()
                    }
                }
                .accessibilityIdentifier(AccessibilityID.chatResizeProbe)
            }
            .buttonStyle(.plain)
            .frame(width: 1, height: 1)
            .opacity(0.01)
        }
        #endif
    }
}
