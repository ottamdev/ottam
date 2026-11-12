import Foundation
import SwiftUI
import OttamCore
import OttamMarkdownEngine

/// Maps `CodeTokenizer`'s token ranges to review colors, producing an
/// `AttributedString` for `Text`. Highlighting is per-line (good enough for a
/// diff view) and language-aware via file extension.
///
/// This is an adapter, not a highlighter: the patterns, the language table and
/// the token kinds all live in `CodeTokenizer` so the diff reviewer and the
/// markdown engine cannot drift apart.
enum SyntaxHighlighter {
    typealias Language = CodeTokenizer.Language

    static func attributed(
        _ content: String,
        language: Language,
        kind: CoreDiffLineKind,
        typography: AppTypography.Resolution,
        scale: AppTypography.Scale = .code
    ) -> AttributedString {
        var attributed = AttributedString(content)
        attributed.font = typography.font(scale)

        let base: Color = (kind == .note || kind == .metadata)
            ? ReviewColors.secondaryText
            : ReviewColors.primaryText
        attributed.foregroundColor = base

        // Only tokenize actual code lines.
        guard kind == .context || kind == .addition || kind == .deletion else {
            return attributed
        }

        // Tokens arrive in priority order; later ones win over earlier ones.
        for token in CodeTokenizer.tokens(in: content, language: language) {
            apply(token.range, color(for: token.kind), to: &attributed, content)
        }

        return attributed
    }

    static func plainAttributed(
        _ content: String,
        kind: CoreDiffLineKind,
        typography: AppTypography.Resolution,
        scale: AppTypography.Scale = .code
    ) -> AttributedString {
        var attributed = AttributedString(content)
        attributed.font = typography.font(scale)
        attributed.foregroundColor = (kind == .note || kind == .metadata)
            ? ReviewColors.secondaryText
            : ReviewColors.primaryText
        return attributed
    }

    // MARK: - Helpers

    private static func color(for kind: CodeTokenizer.TokenKind) -> Color {
        switch kind {
        case .property: return ReviewColors.property
        case .number: return ReviewColors.number
        case .keyword: return ReviewColors.keyword
        case .type: return ReviewColors.type
        case .string: return ReviewColors.string
        case .comment: return ReviewColors.comment
        }
    }

    private static func apply(
        _ range: NSRange,
        _ color: Color,
        to attributed: inout AttributedString,
        _ content: String
    ) {
        guard let stringRange = Range(range, in: content),
              let lower = AttributedString.Index(stringRange.lowerBound, within: attributed),
              let upper = AttributedString.Index(stringRange.upperBound, within: attributed)
        else { return }
        attributed[lower..<upper].foregroundColor = color
    }
}
