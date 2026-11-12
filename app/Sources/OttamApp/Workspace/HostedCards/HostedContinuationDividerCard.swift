import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import SwiftUI

/// A labeled rule across the transcript, where something changed that no
/// message says: a run the agent started on its own, or a turn answered by
/// a different agent than the one before.
struct ChatTranscriptDivider: Hashable {
    var title: String
    var systemImage: String
    var help: String
    var accessibilityID: String

    /// Above a run the agent started on its own, such as Claude answering
    /// once a background command it started has finished. Nothing the
    /// reader sent asked for what follows, so the turn says where it came from.
    static let continuedAutomatically = Self(
        title: "Continued automatically",
        systemImage: "arrow.turn.down.right",
        help: "The agent continued on its own after background work it started finished.",
        accessibilityID: AccessibilityID.chatContinuedAutomatically
    )

    /// Above the first turn of each run whose agent differs from the run
    /// before it, or whose session was sent the earlier turns. A run whose
    /// predecessor is not loaded gets none: nothing says what changed.
    static func engineSwitches(
        in runs: [ConversationRunSummary],
        engineNames: [EngineID: String]
    ) -> [ConversationRunID: Self] {
        // Run ids are UUIDv7, so they sort in start order.
        let ordered = runs.sorted { $0.runID.rawValue < $1.runID.rawValue }
        var dividers: [ConversationRunID: Self] = [:]
        for (previous, run) in zip(ordered, ordered.dropFirst()) {
            let name = engineNames[run.engineID] ?? run.engineID.rawValue
            if let divider = engineSwitch(
                to: name,
                switched: run.engineID != previous.engineID,
                origin: run.sessionOrigin,
                replay: run.historyReplay
            ) {
                dividers[run.runID] = divider
            }
        }
        return dividers
    }

    private static func engineSwitch(
        to name: String,
        switched: Bool,
        origin: SessionOrigin?,
        replay: ConversationHistoryReplay?
    ) -> Self? {
        let shared = replay.map { replay in
            replay.truncated
                ? "Ottam sent the agent the latest \(replay.entries) entries; older ones and "
                    + "tool output were left out, and it can read them when it needs to."
                : "Ottam sent the agent the \(replay.entries) earlier entries; tool output "
                    + "was left out, and it can read it when it needs to."
        }
        let (title, help): (String, String)
        switch (switched, shared, origin) {
        case (true, let shared?, _):
            (title, help) = ("Switched to \(name) · earlier turns shared", shared)
        case (true, nil, .restored?), (true, nil, .continued?):
            (title, help) = (
                "Switched to \(name) · resumed its earlier session",
                "\(name) picked up where it left off. Turns other agents answered since then "
                    + "were not shared with it."
            )
        case (true, nil, _):
            (title, help) = ("Switched to \(name)", "\(name) answers from here on.")
        case (false, let shared?, .new?):
            (title, help) = ("Earlier turns shared with a fresh session", shared)
        case (false, let shared?, _):
            // A session whose first message was a command, such as /compact.
            (title, help) = ("Earlier turns shared", shared)
        case (false, nil, _):
            return nil
        }
        return Self(
            title: title,
            systemImage: "arrow.left.arrow.right",
            help: help,
            accessibilityID: AccessibilityID.chatEngineSwitched
        )
    }
}

struct ContinuationDividerCardPayload: HostedCardPayload {
    struct Actions {}

    var divider: ChatTranscriptDivider

    var signature: AnyHashable { divider }

    func makeBody(actions: Actions) -> some View {
        ChatContinuationDivider(divider: divider)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: divider.accessibilityID,
            value: divider.title
        )
    }
}

typealias HostedContinuationDividerCard = HostedCard<ContinuationDividerCardPayload>

struct ChatContinuationDivider: View {
    var divider: ChatTranscriptDivider

    var body: some View {
        HStack(spacing: 8) {
            rule
            Label(divider.title, systemImage: divider.systemImage)
                .appFont(.callout)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .fixedSize()
            rule
        }
        .padding(.vertical, 4)
        .help(divider.help)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier(divider.accessibilityID)
    }

    private var rule: some View {
        Rectangle()
            .fill(.separator)
            .frame(height: 1)
            .frame(maxWidth: .infinity)
    }
}
