import CoreGraphics
import Foundation

/// Builds the message a quote reply sends: the highlighted passage as a
/// markdown block quote, then the comment. The thread renders it with the same
/// markdown engine as any message, so nothing downstream knows it is a reply.
public enum QuoteReply {
    /// Past this the quote is cut and marked; a whole page of transcript is a
    /// pasted document, not a passage being replied to.
    public static let maximumQuoteLength = 2_000

    /// The quoted passage on one line, for the card header.
    public static func headerText(for quote: String) -> String {
        quote
            .split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
    }

    /// A reply waiting in the composer: a quoted passage and what was said about it.
    public struct Comment: Identifiable, Equatable, Sendable {
        public let id: UUID
        public let quote: String
        public let comment: String

        public init(id: UUID = UUID(), quote: String, comment: String) {
            self.id = id
            self.quote = quote
            self.comment = comment
        }
    }

    /// The message body, or nil when there is no comment to send.
    public static func messageBody(quote: String, comment: String) -> String? {
        let comment = comment.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !comment.isEmpty else { return nil }
        var passage = quote.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !passage.isEmpty else { return comment }
        if passage.count > maximumQuoteLength {
            passage = String(passage.prefix(maximumQuoteLength))
                .trimmingCharacters(in: .whitespacesAndNewlines) + "…"
        }
        let quoted = passage
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces).isEmpty ? ">" : "> \($0)" }
            .joined(separator: "\n")
        return "\(quoted)\n\n\(comment)"
    }

    /// One message from the staged comments, in order, then the composer's
    /// own text. Nil when there is nothing to send.
    public static func messageBody(comments: [Comment], text: String) -> String? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let parts = comments.compactMap { messageBody(quote: $0.quote, comment: $0.comment) }
            + (text.isEmpty ? [] : [text])
        return parts.isEmpty ? nil : parts.joined(separator: "\n\n")
    }

    /// Where the reply surface sits relative to its selection.
    public enum Pin: Equatable, Sendable {
        /// Next to the selection, which is in view.
        case none
        /// The selection is above the visible band; the surface waits at its top.
        case top
        /// The selection is below the visible band; the surface waits at its bottom.
        case bottom
    }

    /// Which side of the selection the surface takes.
    public enum Side: Equatable, Sendable {
        case below
        case above
    }

    public struct Placement: Equatable, Sendable {
        public let origin: CGPoint
        public let pin: Pin
        public let side: Side

        public init(origin: CGPoint, pin: Pin, side: Side) {
            self.origin = origin
            self.pin = pin
            self.side = side
        }
    }

    /// Places a reply surface of `size` for the selection `anchor`, inside the
    /// unobscured `band`, all in top-left coordinates.
    ///
    /// Pass the `side` an earlier placement returned to keep it: the surface
    /// then clamps into the band rather than flipping across the selection
    /// while it scrolls. Without one, it goes below when that fits and above
    /// otherwise. Once the selection leaves the band it pins to the nearer edge.
    public static func placement(
        anchor: CGRect,
        band: CGRect,
        size: CGSize,
        side: Side? = nil,
        gap: CGFloat = 6,
        margin: CGFloat = 12
    ) -> Placement {
        let minX = band.minX + margin
        let x = max(minX, min(anchor.minX, band.maxX - margin - size.width))
        let minY = band.minY + margin
        let maxY = max(minY, band.maxY - margin - size.height)
        let below = anchor.maxY + gap
        let above = anchor.minY - gap - size.height
        let side = side ?? (below <= maxY ? .below : .above)
        if anchor.maxY <= band.minY {
            return Placement(origin: CGPoint(x: x, y: minY), pin: .top, side: side)
        }
        if anchor.minY >= band.maxY {
            return Placement(origin: CGPoint(x: x, y: maxY), pin: .bottom, side: side)
        }
        let y = side == .below ? below : above
        return Placement(origin: CGPoint(x: x, y: min(maxY, max(minY, y))), pin: .none, side: side)
    }
}
