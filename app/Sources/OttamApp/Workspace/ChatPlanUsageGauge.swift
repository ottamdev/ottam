import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// The selected agent's subscription usage beside the context gauge: one
/// vertical bar per account-wide window, five-hour first. Every chat on the
/// agent shows the same bars; the popover adds per-model windows (Fable),
/// the numbers, and the resets.
struct ChatPlanUsageGauge: View {
    let usage: AgentPlanUsage
    let isRefreshing: Bool
    let refresh: () -> Void
    @State private var isPresented = false

    var body: some View {
        Button {
            isPresented = true
        } label: {
            // Drawn, like the context ring: SwiftUI's linear gauges are
            // horizontal only, and their tint is unreliable on macOS.
            HStack(alignment: .bottom, spacing: 3) {
                ForEach(ChatPlanUsageLevel.barWindows(usage), id: \.identity) { window in
                    ChatPlanUsageBar(window: window)
                }
            }
            .frame(height: 15)
            .opacity(usage.error == nil ? 1 : 0.5)
            .frame(minWidth: 26, minHeight: 26)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .fixedSize()
        .help(ChatPlanUsageLevel.compact(usage))
        .accessibilityLabel("Plan usage")
        .accessibilityValue(ChatPlanUsageLevel.spoken(usage))
        .accessibilityIdentifier(AccessibilityID.chatPlanUsage)
        // The composer sits low in the window, so the popover opens upward.
        .popover(isPresented: $isPresented, arrowEdge: .top) {
            ChatPlanUsageDetail(usage: usage, isRefreshing: isRefreshing, refresh: refresh)
        }
        .animation(.snappy, value: usage.windows)
    }
}

/// One window's bar, filling from the bottom in the window's colour band.
private struct ChatPlanUsageBar: View {
    let window: AgentPlanUsageWindow

    var body: some View {
        let fraction = min(max(window.usedPercent, 0), 100) / 100
        let shape = RoundedRectangle(cornerRadius: 1.5, style: .continuous)
        // A flat-topped level clipped to the track, so a low reading is a
        // short fill rather than a dot.
        Rectangle()
            .fill(ChatPlanUsageLevel(percent: window.usedPercent).tint)
            .scaleEffect(x: 1, y: fraction, anchor: .bottom)
            .background(.quaternary)
            .clipShape(shape)
            .frame(width: 5)
    }
}

private struct ChatPlanUsageDetail: View {
    let usage: AgentPlanUsage
    let isRefreshing: Bool
    let refresh: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Plan Usage")
                .font(.headline)
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 6) {
                ForEach(usage.windows, id: \.identity) { window in
                    GridRow {
                        Text(ChatPlanUsageLevel.name(window))
                        ProgressView(value: min(window.usedPercent, 100), total: 100)
                            .tint(ChatPlanUsageLevel(percent: window.usedPercent).tint)
                            .frame(width: 90)
                        Text("\(Int(window.usedPercent.rounded()))%")
                            .monospacedDigit()
                            .gridColumnAlignment(.trailing)
                        Text(ChatPlanUsageLevel.resets(window))
                            .foregroundStyle(.secondary)
                    }
                }
            }
            HStack(spacing: 4) {
                Text(ChatPlanUsageLevel.footer(usage))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // One slot, so the row doesn't shift between button and spinner.
                ZStack {
                    if isRefreshing {
                        ProgressView()
                            .controlSize(.mini)
                    } else {
                        Button(action: refresh) {
                            Image(systemName: "arrow.clockwise")
                                .font(.caption)
                        }
                        .buttonStyle(.borderless)
                        .foregroundStyle(.secondary)
                        .help("Refresh plan usage")
                        .accessibilityLabel("Refresh plan usage")
                        .accessibilityIdentifier(AccessibilityID.chatPlanUsageRefresh)
                    }
                }
                .frame(width: 16, height: 16)
            }
        }
        .padding(14)
    }
}

extension AgentPlanUsageWindow {
    /// Which limit this is, matching core's merge rule: kind and model, plus
    /// the length label for Codex's unusual windows. The label alone is not
    /// unique, since a model could be named "wk".
    var identity: String {
        "\(kind.rawValue)|\(model ?? "")|\(kind == .other ? label : "")"
    }
}

/// The meter's colour band, from the fullest window or the vendor's verdict.
enum ChatPlanUsageLevel: Equatable {
    case normal
    case filling
    case nearlyFull
    case full

    init(_ usage: AgentPlanUsage) {
        if usage.limitStatus == "rejected" {
            self = .full
        } else {
            self.init(percent: usage.windows.map(\.usedPercent).max() ?? 0)
            if usage.limitStatus == "allowed_warning", self == .normal { self = .filling }
        }
    }

    init(percent: Double) {
        switch percent {
        case 100...: self = .full
        case 90...: self = .nearlyFull
        case 75...: self = .filling
        default: self = .normal
        }
    }

    var tint: Color {
        switch self {
        case .normal: .secondary
        case .filling: .yellow
        case .nearlyFull: .orange
        case .full: .red
        }
    }

    /// The windows the footer draws: account-wide ones only. A model's own
    /// cap (Fable) is popover detail.
    static func barWindows(_ usage: AgentPlanUsage) -> [AgentPlanUsageWindow] {
        usage.windows.filter { $0.model == nil }
    }

    /// "5h 12% · wk 15%", matching the bars.
    static func compact(_ usage: AgentPlanUsage) -> String {
        barWindows(usage)
            .map { "\($0.label) \(Int($0.usedPercent.rounded()))%" }
            .joined(separator: " · ")
    }

    /// "5h 12% used, wk 15% used".
    static func spoken(_ usage: AgentPlanUsage) -> String {
        usage.windows
            .map { "\($0.label) \(Int($0.usedPercent.rounded()))% used" }
            .joined(separator: ", ")
    }

    static func name(_ window: AgentPlanUsageWindow) -> String {
        switch window.kind {
        case .fiveHour: "5-hour"
        case .weekly: window.model ?? "Weekly"
        case .other: window.label
        }
    }

    /// "Resets in 1 day", from the window's Unix-seconds reset.
    static func resets(
        _ window: AgentPlanUsageWindow,
        now: Date = .now,
        locale: Locale = .autoupdatingCurrent
    ) -> String {
        guard let resetsAt = window.resetsAt else { return "" }
        let remaining = Date(timeIntervalSince1970: TimeInterval(resetsAt)).timeIntervalSince(now)
        guard remaining > 0 else { return "Resets now" }
        // "4d 10h", "6h 12m", "17m": the two largest units, minutes at the
        // least so the last minute still reads "1m".
        let style = Duration.UnitsFormatStyle(
            allowedUnits: [.days, .hours, .minutes],
            width: .narrow,
            maximumUnitCount: 2
        ).locale(locale)
        let duration = Duration.seconds(max(remaining, 60).rounded(.up))
        return "Resets in \(duration.formatted(style))"
    }

    /// "Updated 2 minutes ago", or why the last refresh failed.
    static func footer(_ usage: AgentPlanUsage, now: Date = .now) -> String {
        let observed = Date(timeIntervalSince1970: TimeInterval(usage.observedAt) / 1000)
        let updated = "Updated \(min(observed, now).formatted(.relative(presentation: .named)))"
        guard usage.error != nil else { return updated }
        return "Couldn't refresh. \(updated)"
    }
}
