import OttamAccessibility
import OttamMarkdownEngine
import SwiftUI

/// What is left in the transcript once a permission card has been answered,
/// hosted in the native transcript. The disclosure is payload state.
struct PermissionRecordCardPayload: HostedCardPayload {
    struct Actions {
        var onToggleExpanded: () -> Void = {}
        var onOpenPlan: () -> Void = {}
    }

    var record: ChatPermissionRecord
    var isExpanded = false

    var signature: AnyHashable {
        "\(record.headline)|\(record.subject ?? "")|\(record.wasAllowed)|\(record.plan != nil)|\(isExpanded ? record.detailJSON ?? "" : "")"
    }

    func makeBody(actions: Actions) -> some View {
        ChatPermissionRecordRow(
            record: record,
            isExpanded: isExpanded,
            onToggleExpanded: actions.onToggleExpanded,
            onOpenPlan: actions.onOpenPlan
        )
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    mutating func adopt(_ fresh: Self) {
        let isExpanded = self.isExpanded
        self = fresh
        self.isExpanded = isExpanded
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatPermissionRecord,
            value: record.wasAllowed ? "allowed" : "not allowed"
        )
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        actions.onToggleExpanded = { [weak card] in
            card?.updatePayload { $0.isExpanded.toggle() }
        }
    }
}

typealias HostedPermissionRecordCard = HostedCard<PermissionRecordCardPayload>

extension HostedCard where Payload == PermissionRecordCardPayload {
    static func permissionRecord(
        _ record: ChatPermissionRecord,
        theme: MarkdownTheme,
        environment: HostedCardEnvironment,
        openPlan: @escaping (ChatPlan) -> Void = { _ in }
    ) -> HostedPermissionRecordCard {
        let card = HostedPermissionRecordCard(
            payload: PermissionRecordCardPayload(record: record),
            actions: .init(onOpenPlan: { if let plan = record.chatPlan { openPlan(plan) } }),
            theme: theme,
            environment: environment
        )
        return card
    }
}

/// Quieter than the card by design — the decision is history now, not a demand —
/// but it keeps the whole exchange: which tool, what was chosen, and the request
/// itself a click away. It reads as one line in a thread you are scrolling past.
/// A plan's row names the decision and reopens the plan in the Plan tab.
struct ChatPermissionRecordRow: View {
    let record: ChatPermissionRecord
    let isExpanded: Bool
    let onToggleExpanded: () -> Void
    var onOpenPlan: () -> Void = {}

    private var icon: String {
        if record.plan != nil { return "list.bullet.clipboard" }
        return record.wasAllowed ? "lock.open" : "hand.raised.slash"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button(action: record.plan != nil ? onOpenPlan : onToggleExpanded) {
                HStack(spacing: 7) {
                    Image(systemName: icon)
                        .font(.system(size: 10))
                        .frame(width: 14)
                    Text(record.headline)
                        .appFont(.body, .bold)
                    if let subject = record.subject {
                        Text(subject)
                            .appFont(.body)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    if record.plan == nil, record.detailJSON != nil {
                        Image(systemName: "chevron.right")
                            .font(.system(size: 10, weight: .semibold))
                            .rotationEffect(.degrees(isExpanded ? 90 : 0))
                            .opacity(0.5)
                            .animation(.snappy(duration: 0.2), value: isExpanded)
                    }
                }
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(record.plan == nil && record.detailJSON == nil)
            .help(record.plan != nil ? "Show the plan in the tools pane" : "")
            .accessibilityIdentifier(AccessibilityID.chatPermissionRecord)
            .accessibilityValue(record.wasAllowed ? "allowed" : "not allowed")

            if record.plan == nil, let detail = record.detailJSON, isExpanded {
                ScrollView(.horizontal) {
                    Text(detail)
                        .appFont(.codeMeta)
                        .textSelection(.enabled)
                }
                .padding(.leading, 21)
                .transition(.opacity)
            }
        }
        // Height is reserved instantly by the transcript; only the reveal fades.
        .animation(.easeOut(duration: 0.18), value: isExpanded)
        // The reserved height snaps on collapse; keep the fading rows inside it.
        .clipped()
    }
}
