import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import SwiftUI

/// A burst of tool calls between two text segments, hosted in the engine
/// transcript. The disclosure state lives in the payload (height purity) and
/// survives every timeline rebuild through `adopt`.
struct ActivityGroupCardPayload: HostedCardPayload {
    struct Actions {
        var onToggleExpanded: () -> Void = {}
        var onToggleItem: (ChatLiveActivity) -> Void = { _ in }
        /// Opens a subagent's thread in the Subagent tab.
        var onOpenThread: (String) -> Void = { _ in }
        /// Opens the delegated child a "Received message" row names.
        var onOpenConversation: (ConversationID) -> Void = { _ in }
        /// Opens a read file: markdown in a tab, anything else in its app.
        var onOpenFile: (URL) -> Void = { NSWorkspace.shared.open($0) }
        var readPayload: ChatPayloadDetailStore.PayloadReader?
        var readBody: ChatPayloadDetailStore.BodyReader?
        var onStopBackgroundTask: (ChatLiveActivity) -> Void = { _ in }
        /// The reader opened or closed the group, with its members.
        var onExpansionChange: (Bool, [ChatLiveActivity]) -> Void = { _, _ in }
    }

    var activities: [ChatLiveActivity]
    var isActive: Bool
    var isExpanded = false
    var details: ChatPayloadDetailStore?
    var detailRevision = 0

    /// A reasoning row's revision moves with every streamed chunk but shows
    /// nothing; the open text follows it through `details`, which
    /// `didBindActions` configures on every adoption. Equal here means the
    /// card neither redraws nor asks the transcript for layout.
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.isActive == rhs.isActive && lhs.isExpanded == rhs.isExpanded && lhs.detailRevision == rhs.detailRevision
            && lhs.activities.count == rhs.activities.count
            && zip(lhs.activities, rhs.activities).allSatisfy { Self.rendered($0) == Self.rendered($1) }
    }

    private static func rendered(_ activity: ChatLiveActivity) -> ChatLiveActivity {
        var activity = activity
        if activity.kind == "think" { activity.revision = 0; activity.bodyByteCount = 0; activity.detail = nil }
        if let members = activity.thread?.members { activity.thread?.members = members.map(rendered) }
        return activity
    }

    var signature: AnyHashable {
        "\(activities.map { Self.signatureLine($0) })|\(isActive)|\(isExpanded)|\(detailRevision)"
    }

    /// Leaves out streaming reasoning and detail reads, which redraw the card
    /// on every chunk; the open detail contributes only its layout.
    @MainActor var heightSignature: AnyHashable {
        makeHeightSignature(details?.layoutSignature)
    }

    @MainActor func heightSignature(forWidth width: CGFloat, environment: HostedCardEnvironment) -> AnyHashable {
        makeHeightSignature(details?.layoutSignature(forWidth: width, font: environment.typography.nsFont(.code)))
    }

    private func makeHeightSignature(_ details: AnyHashable?) -> AnyHashable {
        AnyHashable([
            AnyHashable(activities.map { Self.signatureLine($0) }),
            AnyHashable(isActive),
            AnyHashable(isExpanded),
            AnyHashable(details)
        ])
    }

    /// Reasoning rows show only their label; `detail` is a preview nothing
    /// renders, and the open reasoning reads the body through `details`.
    private static func signatureLine(_ activity: ChatLiveActivity) -> String {
        let detail = activity.kind == "think" ? "" : activity.detail ?? ""
        var line = "\(activity.id)|\(activity.label)|\(activity.subtitle ?? "")|\(activity.kind ?? "")|\(detail)|\(activity.filePath ?? "")|\(activity.payloads)|\(activity.backgroundTask?.state.rawValue ?? "")|\(activity.backgroundTask?.id ?? "")|\(activity.runID?.rawValue ?? "")"
        if let note = activity.note {
            line += "|note:\(note.state.rawValue)|\(note.stopCause?.rawValue ?? "")|\(activity.noteAcknowledged)"
        }
        if let question = activity.question {
            line += "|question:\(question.state.rawValue)|\(question.answers.map { $0.values.joined(separator: ",") })"
        }
        if let thread = activity.thread {
            line += "|thread:\(thread.id)|\(thread.name ?? "")|\(thread.task ?? "")|\(thread.state?.rawValue ?? "")|\(thread.hasFinalAnswer)|[\(thread.members.map(signatureLine).joined(separator: ","))]"
        }
        return line
    }

    func makeBody(actions: Actions) -> some View {
        ChatActivityGroupRow(
            activities: activities,
            isActive: isActive,
            isExpanded: isExpanded,
            onToggleExpanded: actions.onToggleExpanded,
            details: details, onToggleItem: actions.onToggleItem,
            onStopBackgroundTask: actions.onStopBackgroundTask,
            onOpenThread: actions.onOpenThread,
            onOpenConversation: actions.onOpenConversation,
            onOpenFile: actions.onOpenFile
        )
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    mutating func adopt(_ fresh: Self) {
        let isExpanded = self.isExpanded
        let details = self.details
        let detailRevision = self.detailRevision
        self = fresh
        self.isExpanded = isExpanded
        self.details = details ?? fresh.details
        self.detailRevision = detailRevision
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        if !isActive, activities.count == 1, let activity = activities.first {
            if let thread = activity.thread {
                return TranscriptCardAccessibilityPlaceholder(
                    identifier: AccessibilityID.chatSubagentThread, value: thread.state?.rawValue ?? ""
                )
            }
            if activity.kind == "plan" {
                return TranscriptCardAccessibilityPlaceholder(identifier: "chat.plan.activity", value: activity.detail ?? activity.label)
            }
            if let note = activity.note {
                return TranscriptCardAccessibilityPlaceholder(
                    identifier: AccessibilityID.chatDelegationReceived,
                    value: [note.state.rawValue, activity.receivedOutcome].compactMap { $0 }.joined(separator: ", ")
                )
            }
            return TranscriptCardAccessibilityPlaceholder(
                identifier: activity.kind == "read" && activity.filePath != nil
                    ? "chat.file.open.\(activity.id)" : "chat.payload.toggle.\(activity.id)",
                value: activity.captureWarning ?? ""
            )
        }
        return TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatActivityGroup,
            value: isActive ? "active" : "finished"
        )
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        actions.onToggleExpanded = { [weak card] in
            guard let card else { return }
            if card.payload.isExpanded { card.payload.details?.close() }
            card.updatePayload { $0.isExpanded.toggle() }
            card.actions.onExpansionChange(card.payload.isExpanded, card.payload.activities)
        }
        actions.onToggleItem = { [weak card] activity in card?.payload.details?.toggle(activity) }
    }

    @MainActor static func didBindActions(to card: HostedCard<Self>) {
        card.payload.details?.onChange = { [weak card] in card?.updatePayload { $0.detailRevision &+= 1 } }
        card.payload.details?.configure(card.payload.activities.flatMap(\.flattened), readPayload: card.actions.readPayload, readBody: card.actions.readBody)
    }
}

typealias HostedActivityGroupCard = HostedCard<ActivityGroupCardPayload>

extension HostedCard where Payload == ActivityGroupCardPayload {
    static func activityGroup(
        _ activities: [ChatLiveActivity],
        isActive: Bool,
        isExpanded: Bool = false,
        theme: MarkdownTheme,
        environment: HostedCardEnvironment,
        readPayload: ChatPayloadDetailStore.PayloadReader? = nil,
        readBody: ChatPayloadDetailStore.BodyReader? = nil,
        stopBackgroundTask: @escaping (ConversationRunID, String) -> Void = { _, _ in },
        openThread: @escaping (String) -> Void = { _ in },
        openConversation: @escaping (ConversationID) -> Void = { _ in },
        openFile: @escaping (URL) -> Void = { NSWorkspace.shared.open($0) },
        onExpansionChange: @escaping (Bool, [ChatLiveActivity]) -> Void = { _, _ in }
    ) -> HostedActivityGroupCard {
        let card = HostedActivityGroupCard(
            payload: ActivityGroupCardPayload(
                activities: activities, isActive: isActive, isExpanded: isExpanded, details: ChatPayloadDetailStore()
            ),
            actions: .init(onOpenThread: openThread, onOpenConversation: openConversation, onOpenFile: openFile, readPayload: readPayload, readBody: readBody, onStopBackgroundTask: { activity in
                guard activity.canStopBackgroundTask, let runID = activity.runID,
                      let taskID = activity.backgroundTask?.id else { return }
                stopBackgroundTask(runID, taskID)
            }, onExpansionChange: onExpansionChange),
            theme: theme,
            environment: environment
        )
        return card
    }
}

struct ChatActivityGroupRow: View {
    let activities: [ChatLiveActivity]
    let isActive: Bool
    let isExpanded: Bool
    let onToggleExpanded: () -> Void
    var details: ChatPayloadDetailStore?
    var onToggleItem: (ChatLiveActivity) -> Void = { _ in }
    var onStopBackgroundTask: (ChatLiveActivity) -> Void = { _ in }
    var onOpenThread: (String) -> Void = { _ in }
    var onOpenConversation: (ConversationID) -> Void = { _ in }
    var onOpenFile: (URL) -> Void = { NSWorkspace.shared.open($0) }

    private var summary: String { Self.summary(of: activities) }

    /// "Read files, ran commands"; "Received 2 messages, read files".
    static func summary(of activities: [ChatLiveActivity]) -> String {
        var phrases: [String] = []
        let received = activities.count { $0.kind == "receivedMessage" }
        for activity in activities where !phrases.contains(activity.summaryPhrase) {
            phrases.append(activity.summaryPhrase)
        }
        // "Received 2 messages, read files"
        if let index = phrases.firstIndex(of: "received messages") {
            phrases[index] = received == 1 ? "received a message" : "received \(received) messages"
        }
        guard let first = phrases.first else { return "Working" }
        let capitalized = first.prefix(1).uppercased() + first.dropFirst()
        return ([capitalized] + phrases.dropFirst()).joined(separator: ", ")
    }

    private static func icon(forKind kind: String?) -> String {
        switch kind {
        case "edit", "delete", "move": return "pencil.line"
        case "execute": return "terminal"
        case "read": return "book"
        case "search": return "magnifyingglass"
        case "fetch": return "globe"
        case "plan": return "list.bullet"
        case "think": return "brain"
        case "image": return "photo"
        case "subagent": return "person.2"
        case "receivedMessage": return "arrow.down.message"
        case "subagentAnswer": return "text.bubble"
        case "subagentError": return "exclamationmark.triangle"
        case "transcriptError", "streamGap": return "exclamationmark.triangle"
        default: return "hammer"
        }
    }

    private var headlineIcon: String {
        if isActive, !isExpanded, let latest = activities.last {
            return Self.icon(forKind: latest.kind)
        }
        return Self.icon(forKind: activities.compactMap(\.kind).first)
    }

    var body: some View {
        if !isActive, activities.count == 1, let activity = activities.first {
            activityRow(activity)
        } else {
            groupedBody
        }
    }

    private func disclosureArrow(isExpanded: Bool) -> some View {
        Image(systemName: "chevron.right")
            .font(.system(size: 11, weight: .semibold))
            .rotationEffect(.degrees(isExpanded ? 90 : 0))
            .opacity(0.5)
            .animation(.snappy(duration: 0.2), value: isExpanded)
    }

    private var groupedBody: some View {
        VStack(alignment: .leading, spacing: 8) {
            // One clickable headline for both states: the latest call ticks by with a
            // shimmer while the burst is in flight, then settles into the summary.
            Button(action: onToggleExpanded) {
                HStack(spacing: 7) {
                    Image(systemName: headlineIcon)
                        .font(.system(size: 11))
                        .frame(width: 14)
                        .contentTransition(.symbolEffect(.replace))
                        // No `.symbolEffect(.pulse)` here: a running symbol effect inside
                        // an NSHostingView hosted by the text view makes every AX query
                        // of the card wait on the animation (live-transcript capture
                        // 3.2 s -> 0.12 s without it). The shimmer label already says
                        // "in flight".
                    if isActive, !isExpanded, let latest = activities.last {
                        ChatShimmerText(text: latest.label)
                            .contentTransition(.opacity)
                    } else {
                        Text(summary)
                            .appFont(.body)
                            .lineLimit(1)
                    }
                    disclosureArrow(isExpanded: isExpanded)
                }
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
                // The hosted root disables animations (the transcript reserves
                // the card's height instantly); these opt back in for the
                // bits that never change the height.
                .animation(.easeInOut(duration: 0.2), value: headlineIcon)
                .animation(.easeInOut(duration: 0.2), value: isActive)
                // Same-kind tool changes swap the label without touching the
                // icon; this keeps the crossfade and chevron slide for those.
                .animation(.easeInOut(duration: 0.2), value: activities.last?.label)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier(AccessibilityID.chatActivityGroup)
            .accessibilityLabel(isActive && !isExpanded ? activities.last?.label ?? summary : summary)
            .accessibilityValue(isActive ? "active" : "finished")
            .help(activities.count == 1 ? activities[0].hoverText : summary)

            if isExpanded {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(activities) { activity in
                        activityRow(activity)
                    }
                }
                .padding(.leading, 21)
            }
        }
    }

    @ViewBuilder
    private func activityRow(_ activity: ChatLiveActivity) -> some View {
        if let thread = activity.thread {
            // A nested thread is a row of its parent's: recursion needs erasure.
            AnyView(threadRow(thread, of: activity))
        } else if activity.kind == "plan" {
            activityLine(activity, shimmers: isActive && activity.id == activities.last?.id)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("chat.plan.activity")
                .accessibilityLabel(activity.detail ?? activity.label)
                .help(activity.detail ?? activity.label)
        } else if activity.note != nil {
            receivedRow(activity)
        } else if activity.kind == "read", let path = activity.filePath {
            Button {
                onOpenFile(URL(fileURLWithPath: path))
            } label: {
                activityLine(activity, shimmers: isActive && activity.id == activities.last?.id)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("chat.file.open.\(activity.id)")
            .accessibilityLabel(activity.label)
            .help("Open \(path)")
        } else {
            expandableActivityRow(activity)
        }
    }

    private func expandableActivityRow(_ activity: ChatLiveActivity) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                expandableActivityToggle(activity)
                if let task = activity.backgroundTask {
                    backgroundTaskStatus(task, of: activity)
                }
                if let question = activity.question {
                    questionStatus(question)
                }
            }
            if let details, details.expandedID == activity.id { ChatPayloadDetailView(store: details) }
        }
    }

    /// "Received message from <title> · stopped when Ottam quit ▸":
    /// the title opens the child, the rest shows the message it sent.
    private func receivedRow(_ activity: ChatLiveActivity) -> some View {
        let expanded = details?.expandedID == activity.id
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 7) {
                Button { onToggleItem(activity) } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        Image(systemName: Self.icon(forKind: activity.kind))
                            .font(.system(size: 11))
                            .frame(width: 14)
                        Text("Received message from")
                            .appFont(.body)
                            .fixedSize()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityHidden(true)
                if let sender = activity.noteSender {
                    // A child's tile, as on its card, sits with the title
                    // that names it.
                    HStack(spacing: 5) {
                        if let avatar = activity.noteAvatar {
                            AgentAvatarView(avatar: avatar, size: 16)
                        }
                        Button(activity.label) { onOpenConversation(sender) }
                            .buttonStyle(.plain)
                            .appFont(.body)
                            .underline(pattern: .dot)
                            .lineLimit(1)
                            .help("Open this agent's conversation")
                            .accessibilityIdentifier(AccessibilityID.chatDelegationOpen)
                    }
                } else {
                    Text(activity.label).appFont(.body).lineLimit(1)
                }
                if let outcome = activity.receivedOutcome {
                    Text("· " + outcome)
                        .appFont(.callout)
                        .foregroundStyle(activity.note?.state == .failed ? AnyShapeStyle(.orange) : AnyShapeStyle(.tertiary))
                        .lineLimit(1)
                        .fixedSize()
                }
                Button { onToggleItem(activity) } label: {
                    disclosureArrow(isExpanded: expanded).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier(AccessibilityID.chatDelegationReceived)
                .accessibilityLabel(activity.hoverText)
                .accessibilityValue([activity.note?.state.rawValue, activity.receivedOutcome].compactMap { $0 }.joined(separator: ", "))
            }
            .foregroundStyle(.secondary)
            if expanded {
                Text(Self.receivedMessage(activity.detail))
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 21)
            }
        }
    }

    /// The child's message with its inline markdown (bold, code, links)
    /// rendered and its line breaks kept.
    static func receivedMessage(_ body: String?) -> AttributedString {
        guard let body, !body.isEmpty else { return AttributedString("It sent no message.") }
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: body, options: options)) ?? AttributedString(body)
    }

    /// A subagent's work under one headline: what it was asked to do, how it
    /// is doing, and while it runs, its newest line. Clicking it opens the
    /// subagent's conversation in the Subagent tab.
    private func threadRow(_ thread: ChatSubagentThread, of activity: ChatLiveActivity) -> some View {
        let title = thread.task ?? (thread.state == nil ? "Subagent activity" : activity.label)
        let latest = thread.state == .running ? thread.latestMember : nil
        let alarming = thread.state == .failed || thread.state == .disconnected
        return VStack(alignment: .leading, spacing: 6) {
            Button { onOpenThread(thread.id) } label: {
                HStack(alignment: .firstTextBaseline, spacing: 7) {
                    Image(systemName: Self.icon(forKind: "subagent"))
                        .font(.system(size: 11))
                        .frame(width: 14)
                    Text(title)
                        .appFont(.body)
                        .lineLimit(1)
                    if let name = thread.name {
                        Text(name)
                            .appFont(.callout)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                    }
                    if let state = thread.stateLabel {
                        Text(state)
                            .appFont(.callout)
                            .foregroundStyle(alarming ? AnyShapeStyle(.orange) : AnyShapeStyle(.tertiary))
                            .lineLimit(1)
                            .fixedSize()
                    }
                    if let latest {
                        ChatShimmerText(text: latest.label)
                            .lineLimit(1)
                    }
                    disclosureArrow(isExpanded: false)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(AccessibilityID.chatSubagentThread)
            .accessibilityLabel([title, thread.name, thread.stateLabel].compactMap { $0 }.joined(separator: ", "))
            .accessibilityValue(thread.state?.rawValue ?? "")
            .help(thread.state == .disconnected
                ? "Ottam lost track of this subagent when the agent or the app stopped. It may have finished."
                : activity.hoverText)

            // The launch call's result, which lives on the parent's call and
            // not in the tab: a synchronous subagent's answer, or why the
            // launch failed.
            if thread.hasFinalAnswer && thread.state == .completed || thread.state == .failed {
                expandableActivityRow(Self.launchResult(activity, failed: thread.state == .failed))
                    .padding(.leading, 21)
            }
        }
    }

    /// The launch call itself, named for what its result holds; it keeps the
    /// call's id so its payload opens as any other row's does. Its own kind:
    /// the call's (`think` on Claude) would open the input as reasoning.
    static func launchResult(_ activity: ChatLiveActivity, failed: Bool) -> ChatLiveActivity {
        var result = activity
        result.label = failed ? "Launch error" : "Final answer"
        result.kind = failed ? "subagentError" : "subagentAnswer"
        result.thread = nil
        return result
    }

    /// Beside the disclosure, not inside it: Stop is its own control, and the
    /// state reads as part of the row rather than of its toggle's label.
    private func backgroundTaskStatus(_ task: TranscriptBackgroundTask, of activity: ChatLiveActivity) -> some View {
        HStack(spacing: 8) {
            Text(Self.backgroundStateLabel(task.state))
                .appFont(.callout)
                .foregroundStyle(task.state == .failed || task.state == .lost ? AnyShapeStyle(.orange) : AnyShapeStyle(.tertiary))
                .lineLimit(1)
                .fixedSize()
                .accessibilityIdentifier(AccessibilityID.chatBackgroundTaskState)
                .accessibilityValue(task.state.rawValue)
                .help(task.state == .lost
                    ? "Ottam lost track of this task when the agent or the app stopped. It may have finished."
                    : "")
            if activity.canStopBackgroundTask {
                Button("Stop") { onStopBackgroundTask(activity) }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .appFont(.callout)
                    .help("Ask the agent to stop this background task")
                    .accessibilityIdentifier(AccessibilityID.chatBackgroundTaskStop)
            }
        }
    }

    /// What became of a call's question: the answer itself, or why there is
    /// none. A secret answer reads only as answered.
    private func questionStatus(_ question: TranscriptQuestion) -> some View {
        Text(Self.questionLabel(question))
            .appFont(.callout)
            .foregroundStyle(question.state == .answered ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tertiary))
            .lineLimit(1)
            .truncationMode(.tail)
            .accessibilityIdentifier(AccessibilityID.chatQuestionRecord)
            .accessibilityValue(question.state.rawValue)
            .help(Self.questionHelp(question))
    }

    static func questionLabel(_ question: TranscriptQuestion) -> String {
        switch question.state {
        case .pending: return "Waiting for your answer"
        case .skipped: return "Skipped"
        case .notAnswered: return "Not answered"
        case .answered:
            let answers = question.answers.compactMap { answer -> String? in
                if answer.secure { return "Answered" }
                return answer.values.isEmpty ? nil : answer.values.joined(separator: ", ")
            }
            return answers.isEmpty ? "Answered" : answers.joined(separator: " · ")
        }
    }

    private static func questionHelp(_ question: TranscriptQuestion) -> String {
        guard question.state == .answered else { return "" }
        return question.answers.map { answer in
            let value = answer.secure ? "Answered" : (answer.values.isEmpty ? "No answer" : answer.values.joined(separator: ", "))
            return "\(answer.prompt): \(value)"
        }
        .joined(separator: "\n")
    }

    static func backgroundStateLabel(_ state: AsyncTaskState) -> String {
        switch state {
        case .running: "Running in background"
        case .paused: "Paused in background"
        case .completed: "Completed"
        case .failed: "Failed"
        case .stopped: "Stopped"
        case .lost: "Lost"
        }
    }

    private func expandableActivityToggle(_ activity: ChatLiveActivity) -> some View {
        Group {
            Button { onToggleItem(activity) } label: {
                HStack(spacing: 7) {
                    activityLine(activity, shimmers: isActive && activity.id == activities.last?.id)
                    if let warning = activity.captureWarning {
                        HStack(spacing: 4) {
                            Image(systemName: "exclamationmark.triangle")
                            Text(warning)
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .help(activity.captureWarningHelp)
                    }
                    disclosureArrow(isExpanded: details?.expandedID == activity.id)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("chat.payload.toggle.\(activity.id)")
            .accessibilityLabel(activity.subtitle.map { "\(activity.label), \($0)" } ?? activity.label)
            .accessibilityValue(activity.captureWarning ?? "")
        }
    }

    private func activityLine(_ activity: ChatLiveActivity, shimmers: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 7) {
            Image(systemName: Self.icon(forKind: activity.kind))
                .font(.system(size: 11))
                .frame(width: 14)
            if shimmers {
                ChatShimmerText(text: activity.label)
            } else if let path = activity.filePath {
                Text("\(String(activity.label.dropLast(URL(fileURLWithPath: path).lastPathComponent.count)))\(Text(URL(fileURLWithPath: path).lastPathComponent).underline(pattern: .dot))")
                    .appFont(.body)
                    .lineLimit(1)
            } else {
                Text(activity.label)
                    .appFont(.body)
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            if let subtitle = activity.subtitle {
                Text(subtitle)
                    .appFont(.callout)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .help(activity.hoverText)
    }
}
