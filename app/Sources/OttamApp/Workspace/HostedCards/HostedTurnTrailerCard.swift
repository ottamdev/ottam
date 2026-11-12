import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import Foundation
import SwiftUI

/// One fixed-height host for every phase of an assistant turn's trailing row.
/// `showsCopied` is UI-only and does not participate in geometry.
struct TurnTrailerCardPayload: HostedCardPayload {
    struct Actions {
        /// The host's clipboard write; `onCopy` is bound on top of it.
        var copy: (String) -> Void = { _ in }
        var onCopy: () -> Void = {}
        var onSendToParent: () -> Void = {}
    }

    var presentation: ChatTurnTrailerPresentation
    var showsCopied = false

    /// Every phase uses the same one-line container and therefore the same
    /// cached measurement. Payload changes only refresh its presentation.
    var signature: AnyHashable { "turn-trailer" }

    func makeBody(actions: Actions) -> some View {
        ChatTurnTrailerRow(
            presentation: presentation,
            showsCopied: showsCopied,
            onCopy: actions.onCopy,
            onSendToParent: actions.onSendToParent
        )
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    mutating func adopt(_ fresh: Self) {
        let showsCopied = self.showsCopied
        self = fresh
        self.showsCopied = showsCopied
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        switch presentation {
        case .running(_, let status, _, _, let activityLabel, _, _):
            TranscriptCardAccessibilityPlaceholder(
                identifier: AccessibilityID.chatRunStatus,
                value: activityLabel ?? (status == .queued ? "starting" : "working")
            )
        case .finished(_, _, let outcome), .persisted(_, _, _, let outcome, _, _, _):
            // One identifier for every cut-short turn: the reason varies, the
            // fact the reader is being warned about does not.
            outcome == .complete
                ? nil
                : TranscriptCardAccessibilityPlaceholder(
                    identifier: AccessibilityID.chatPartialOutput,
                    value: outcome.accessibilityValue
                )
        }
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        let copy = actions.copy
        actions.onCopy = { [weak card] in
            guard let card, let body = card.payload.presentation.copyBody else { return }
            copy(body)
            card.updatePayload { $0.showsCopied = true }
            Task { [weak card] in
                try? await Task.sleep(for: .seconds(1.5))
                card?.updatePayload { $0.showsCopied = false }
            }
        }
    }
}

typealias HostedTurnTrailerCard = HostedCard<TurnTrailerCardPayload>

extension HostedCard where Payload == TurnTrailerCardPayload {
    static func turnTrailer(
        _ presentation: ChatTurnTrailerPresentation,
        theme: MarkdownTheme,
        environment: HostedCardEnvironment,
        copy: @escaping (String) -> Void,
        sendToParent: @escaping () -> Void = {}
    ) -> HostedTurnTrailerCard {
        HostedTurnTrailerCard(
            payload: TurnTrailerCardPayload(presentation: presentation),
            actions: .init(copy: copy, onSendToParent: sendToParent),
            theme: theme,
            environment: environment
        )
    }
}

private struct ChatTurnTrailerRow: View {
    let presentation: ChatTurnTrailerPresentation
    let showsCopied: Bool
    let onCopy: () -> Void
    let onSendToParent: () -> Void

    var body: some View {
        HStack(spacing: 0) {
            // This zero-width strut supplies the same callout line height to
            // every phase without hard-coding a pixel height or clipping when
            // the app's typography scale changes. `fixedSize` first, or the
            // zero-width frame wraps it to two lines and the row measures
            // twice the height it needs.
            Text("Ag")
                .appFont(.callout)
                .hidden()
                .fixedSize()
                .frame(width: 0)
                .accessibilityHidden(true)

            switch presentation {
            case .running(_, let status, let startedAt, let activityCount, let activityLabel, let backgroundTasks, let subagents):
                runningRow(
                    status: status,
                    startedAt: startedAt,
                    activityCount: activityCount,
                    activityLabel: activityLabel,
                    backgroundTasks: backgroundTasks,
                    subagents: subagents
                )
            case .finished(_, _, let outcome):
                completedRow(createdAt: nil, outcome: outcome, backgroundTasks: 0, usage: nil, sendsToParent: false)
            case .persisted(_, _, let createdAt, let outcome, let backgroundTasks, let usage, let sendsToParent):
                completedRow(
                    createdAt: createdAt, outcome: outcome, backgroundTasks: backgroundTasks,
                    usage: usage, sendsToParent: sendsToParent
                )
            }
        }
        .padding(.top, 4)
    }

    private func runningRow(
        status: ConversationRunStatus,
        startedAt: String?,
        activityCount: Int,
        activityLabel: String?,
        backgroundTasks: Int,
        subagents: Int
    ) -> some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)

            Text(activityLabel ?? (status == .queued ? "Starting agent…" : "Agent is working"))
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(1)

            if activityCount > 0 {
                metric("\(activityCount) activities")
                    .contentTransition(.numericText())
                    .animation(.snappy, value: activityCount)
            }

            if let startedAt = startedAt.flatMap(ChatTimestampFormatter.date(from:)) {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    metric(elapsed(from: startedAt, to: context.date))
                }
            }

            backgroundTaskMetric(backgroundTasks)

            if subagents > 0 {
                metric(subagents == 1 ? "1 subagent running" : "\(subagents) subagents running")
                    .contentTransition(.numericText())
                    .animation(.snappy, value: subagents)
                    .help("Subagents this turn launched that are still working")
                    .accessibilityIdentifier(AccessibilityID.chatSubagentCount)
                    .accessibilityValue("\(subagents)")
            }

            Spacer(minLength: 0)
        }
        // One element for the row, not a label on the text inside it. The row
        // carries nothing focusable, and a hosting view with no interactive
        // content publishes no accessibility subtree of its own — so an
        // identifier buried on a child never reaches the tree.
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier(AccessibilityID.chatRunStatus)
        .accessibilityValue(activityLabel ?? (status == .queued ? "starting" : "working"))
    }

    private func completedRow(
        createdAt: String?,
        outcome: ChatTurnOutcome,
        backgroundTasks: Int,
        usage: ChatTurnUsage?,
        sendsToParent: Bool
    ) -> some View {
        HStack(spacing: 10) {
            if presentation.copyBody != nil {
                Button(action: onCopy) {
                    Image(systemName: showsCopied ? "checkmark" : "square.on.square")
                        .appFont(.callout)
                        .contentTransition(.symbolEffect(.replace))
                        .frame(width: 16, height: 16)
                        .animation(.easeInOut(duration: 0.2), value: showsCopied)
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .pointerStyle(.link)
                .help("Copy message")
                .accessibilityLabel(showsCopied ? "Copied" : "Copy message")
            }

            if let createdAt, let timestamp = chatMessageTimestamp(createdAt) {
                Text(timestamp)
                    .appFont(.callout)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }

            if let label = outcome.label {
                Text("·")
                    .appFont(.callout)
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)

                Text(label)
                    .appFont(.callout)
                    .foregroundStyle(.orange)
                    .lineLimit(1)
                    .help(outcome.explanation ?? "")
                    .accessibilityIdentifier(AccessibilityID.chatPartialOutput)
            }

            backgroundTaskMetric(backgroundTasks)

            usageMetric(usage)

            if sendsToParent {
                // This turn's report waits in the parent without waking it.
                Button("Send to Parent", action: onSendToParent)
                    .buttonStyle(.borderless)
                    .appFont(.callout)
                    .lineLimit(1)
                    .help("Wake the agent that delegated this task with this turn's report")
                    .accessibilityIdentifier(AccessibilityID.chatDelegationSendToParent)
            }

            Spacer(minLength: 0)
        }
    }

    /// The tokens the turn used, with what they cover in the tooltip.
    @ViewBuilder
    private func usageMetric(_ usage: ChatTurnUsage?) -> some View {
        switch usage {
        case .tokens(let usage):
            let total = ChatTokenCount.format(usage.totalTokens)
            metric("\(total) tokens")
                .help(ChatTokenCount.breakdown(usage))
                .accessibilityIdentifier(AccessibilityID.chatTurnUsage)
                .accessibilityValue("\(total) tokens")
        case .unavailable:
            metric("Usage unavailable")
                .help("The agent didn't report the tokens this turn used")
                .accessibilityIdentifier(AccessibilityID.chatTurnUsage)
                .accessibilityValue("unavailable")
        case nil:
            EmptyView()
        }
    }

    /// Work the turn handed off that is still running, wherever the turn is.
    @ViewBuilder
    private func backgroundTaskMetric(_ count: Int) -> some View {
        if count > 0 {
            metric(count == 1 ? "1 background task" : "\(count) background tasks")
                .contentTransition(.numericText())
                .animation(.snappy, value: count)
                .help("Commands or agents this turn started that are still running")
                .accessibilityIdentifier(AccessibilityID.chatBackgroundTaskCount)
                .accessibilityValue("\(count)")
        }
    }

    private func metric(_ text: String) -> some View {
        Text("·  \(text)")
            .appFont(.callout)
            .foregroundStyle(.tertiary)
            .monospacedDigit()
            .lineLimit(1)
    }

    private func elapsed(from start: Date, to now: Date) -> String {
        let seconds = max(0, Int(now.timeIntervalSince(start)))
        if seconds < 60 { return "\(seconds)s" }
        return "\(seconds / 60)m \(seconds % 60)s"
    }
}

/// Received-time label in the Claude/Codex style: time alone for today, weekday + time
/// within the last week, full date beyond that.
func chatMessageTimestamp(_ iso: String) -> String? {
    guard let date = ChatTimestampFormatter.date(from: iso) else { return nil }
    let calendar = Calendar.current
    let time = date.formatted(date: .omitted, time: .shortened)
    if calendar.isDateInToday(date) { return time }
    if calendar.isDateInYesterday(date) { return "Yesterday \(time)" }
    if let weekAgo = calendar.date(byAdding: .day, value: -6, to: calendar.startOfDay(for: Date())),
       date >= weekAgo {
        return "\(date.formatted(.dateTime.weekday(.wide))) \(time)"
    }
    return date.formatted(date: .abbreviated, time: .shortened)
}

extension ChatTurnOutcome {
    /// Nil for a turn that ended on its own: the ordinary case earns no chip.
    var label: String? {
        switch self {
        case .complete: nil
        case .stopped: "Stopped"
        case .steered: "Interrupted"
        case .endedEarly: "Partial Output"
        case .failed: "Failed"
        }
    }

    var explanation: String? {
        switch self {
        case .complete:
            nil
        case .stopped:
            "You stopped this response. Only the output received so far is shown."
        case .steered:
            "You sent another message before this response finished. "
                + "The agent received it and picks up below."
        case .endedEarly:
            "This response ended before the agent finished. "
                + "Only the output received so far is shown."
        case .failed(let reason):
            reason ?? "This response ended in an error. Only the output received so far is shown."
        }
    }

    var accessibilityValue: String? {
        switch self {
        case .complete: nil
        case .stopped: "stopped"
        case .steered: "interrupted to steer"
        case .endedEarly: "partial"
        case .failed: "failed"
        }
    }
}
