import Foundation

/// Compact relative time ("3h ago") from an ISO8601 timestamp, for lightweight rows.
func chatRelativeTime(_ iso: String) -> String? {
    guard let date = ChatTimestampFormatter.date(from: iso) else { return nil }
    let formatter = RelativeDateTimeFormatter()
    formatter.unitsStyle = .abbreviated
    return formatter.localizedString(for: date, relativeTo: Date())
}

/// ISO8601 parser tolerant of fractional seconds, shared by Chat row timestamps.
enum ChatTimestampFormatter {
    private static let withFraction: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    private static let plain = ISO8601DateFormatter()

    static func date(from iso: String) -> Date? {
        withFraction.date(from: iso) ?? plain.date(from: iso)
    }
}
