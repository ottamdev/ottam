import AppKit
import OttamMarkdownEngine

/// Ottam's shared Markdown styling: typography scales for fonts,
/// `ReviewColors` for code chrome and syntax, and system semantic colors for
/// text. Native transcript artifacts and future Markdown view/edit surfaces use
/// this value.
extension MarkdownTheme {
    static func ottam(_ typography: AppTypography.Resolution) -> MarkdownTheme {
        MarkdownTheme(
            body: typography.nsFont(.prose),
            code: typography.nsFont(.code),
            heading1: typography.nsFont(.titleLarge, .bold),
            heading2: typography.nsFont(.headline, .bold),
            heading3: typography.nsFont(.title, .bold),
            heading4: typography.nsFont(.prose, .bold),
            textColor: .labelColor,
            secondaryTextColor: .secondaryLabelColor,
            inlineCodeBackground: NSColor.labelColor.withAlphaComponent(0.08),
            codeBackground: ReviewColors.editorBackgroundNS,
            codeBorder: ReviewColors.cardBorderNS,
            quoteBar: NSColor.tertiaryLabelColor,
            ruleColor: NSColor.separatorColor,
            syntaxColors: [
                .keyword: ReviewColors.keywordNS,
                .string: ReviewColors.stringNS,
                .type: ReviewColors.typeNS,
                .property: ReviewColors.propertyNS,
                .number: ReviewColors.numberNS,
                .comment: ReviewColors.commentNS
            ],
            inlineCode: typography.inlineCodeFont(within: .prose),
            codeChrome: typography.nsFont(.label),
            signature: typography.signature
        )
    }
}
