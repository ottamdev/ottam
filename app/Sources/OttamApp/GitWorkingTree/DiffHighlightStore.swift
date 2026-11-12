import OttamCore
import Foundation
import SwiftUI

/// Highlighted diff lines and the memo table behind them.
///
/// Deliberately not `@Observable`, and deliberately not a member of
/// `DiffReviewModel`'s observable surface. Every diff row asks for its line on
/// every body, so if this lived on the model each row would take a dependency on
/// the model just to read a memo table — and a cache miss, which is a *write*
/// during a view update, would invalidate every row that had already read one.
/// `@ObservationIgnored` alone fixes that; owning the cache here keeps it fixed,
/// because there is no tracked property nearby to reach for by accident.
///
/// Lifetime is tied to a snapshot: the model invalidates on every load.
/// testing
/// 1233123
/// dasdasd
@MainActor
final class DiffHighlightStore {
    /// Past this, tokenizing costs more than the highlighting is worth — minified
    /// or generated lines fall back to plain attribution.
    private let maximumLineLength = 2_000
    private let maximumEntries = 12_000

    private var cache: [Key: AttributedString] = [:]
    private var insertionOrder: [Key] = []

    func line(
        _ content: String,
        language: SyntaxHighlighter.Language,
        kind: CoreDiffLineKind,
        typography: AppTypography.Resolution
    ) -> AttributedString {
        guard content.count <= maximumLineLength else {
            return SyntaxHighlighter.plainAttributed(
                content,
                kind: kind,
                typography: typography,
                scale: .codeDiff
            )
        }

        let key = Key(content: content, language: language, kind: kind, typography: typography)
        if let cached = cache[key] {
            return cached
        }

        let attributed = SyntaxHighlighter.attributed(
            content,
            language: language,
            kind: kind,
            typography: typography,
            scale: .codeDiff
        )
        cache[key] = attributed
        insertionOrder.append(key)
        if insertionOrder.count > maximumEntries {
            cache.removeValue(forKey: insertionOrder.removeFirst())
        }
        return attributed
    }

    func invalidate() {
        cache.removeAll(keepingCapacity: true)
        insertionOrder.removeAll(keepingCapacity: true)
    }

    /// Keyed by content rather than by position, so the identical lines a diff is
    /// full of — blanks, closing braces — are highlighted once. `content` shares
    /// storage with the line it came from; this does not copy the text.
    private struct Key: Hashable {
        var content: String
        var language: SyntaxHighlighter.Language
        var kind: CoreDiffLineKind
        /// The font is baked into the attributed string, so a font change has to
        /// miss rather than serve a stale face.
        var typography: AppTypography.Resolution
    }
}
