import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// How full the chat's context window is, as the agent last reported it.
struct ChatContextGauge: View {
    let usage: ConversationContextUsage

    var body: some View {
        // Drawn rather than a circular `ProgressView`, which ignores tint on
        // macOS and so can't warn as the context fills.
        ZStack {
            Circle()
                .stroke(.quaternary, lineWidth: 2.5)
            Circle()
                .trim(from: 0, to: ChatContextLevel.fraction(usage))
                .stroke(ChatContextLevel(usage).tint, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: 15, height: 15)
        .frame(width: 26, height: 26)
        .contentShape(Rectangle())
        .help(ChatContextLevel.summary(usage))
        .animation(.snappy, value: usage)
        .accessibilityRepresentation {
            Text("Context")
                .accessibilityValue(ChatContextLevel.summary(usage))
        }
        .accessibilityIdentifier(AccessibilityID.chatContextUsage)
    }
}

/// The gauge's colour band: it warns as the context fills, before the agent
/// has to compact or refuse.
enum ChatContextLevel: Equatable {
    case normal
    case filling
    case nearlyFull
    case full

    init(_ usage: ConversationContextUsage) {
        switch ChatContextLevel.fraction(usage) {
        case 0.95...: self = .full
        case 0.90...: self = .nearlyFull
        case 0.75...: self = .filling
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

    static func fraction(_ usage: ConversationContextUsage) -> Double {
        guard usage.size > 0 else { return 0 }
        return min(1, Double(usage.used) / Double(usage.size))
    }

    /// "24.1K of 200K tokens (12%)".
    static func summary(_ usage: ConversationContextUsage) -> String {
        let percent = Int((fraction(usage) * 100).rounded())
        return "\(ChatTokenCount.format(usage.used)) of \(ChatTokenCount.format(usage.size)) tokens (\(percent)%)"
    }
}

enum ChatTokenCount {
    /// 999, 1.5K, 24.1K, 200K, 1.2M.
    static func format(_ count: UInt64) -> String {
        func scaled(_ divisor: Double, _ suffix: String) -> String {
            let value = (Double(count) / divisor * 10).rounded() / 10
            let text = value == value.rounded() ? String(Int(value)) : String(format: "%.1f", value)
            return text + suffix
        }
        switch count {
        case ..<1_000: return String(count)
        case ..<999_950: return scaled(1_000, "K")
        default: return scaled(1_000_000, "M")
        }
    }

    /// The trailer's tooltip: each category the agent reported, then what
    /// the count covers.
    static func breakdown(_ usage: ConversationRunUsage) -> String {
        let parts = [
            ("input", usage.inputTokens),
            ("output", usage.outputTokens),
            ("thinking", usage.thoughtTokens),
            ("cache read", usage.cachedReadTokens),
            ("cache write", usage.cachedWriteTokens),
        ]
        .compactMap { label, count in count.map { "\(format($0)) \(label)" } }
        let scope = switch usage.scope {
        case .turn: "Counts every model call in the turn."
        case .mainAgent: "Counts the main agent's model calls, not its subagents'."
        case .lastRequest: "Counts the turn's last model request only."
        }
        return parts.joined(separator: " · ") + "\n" + scope
    }
}
