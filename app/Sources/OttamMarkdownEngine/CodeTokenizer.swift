import Foundation

/// Lightweight, regex-based, per-line code tokenizer. The single source of code
/// tokens in the app: it emits ranges only, and consumers map `TokenKind` to
/// their own colors (the engine's styler to `NSColor`, the diff reviewer's
/// `SyntaxHighlighter` to SwiftUI `Color`).
///
/// Tokens are returned in application order and may overlap: apply them in order
/// and let later tokens override earlier ones — string wins over type, comment last.
///
/// **Strictly line-scoped, deliberately.** Multi-line constructs — block
/// comments, heredocs and triple-quoted strings — are not recognized. This lets
/// viewers and editors retokenize changed lines without hidden cross-line state.
public enum CodeTokenizer {
    public enum Language: Hashable, Sendable {
        case swift, go, shell, json, yaml, markdown, plain

        /// Maps a file path by extension — the diff reviewer's entry point.
        public init(path: String) {
            switch (path as NSString).pathExtension.lowercased() {
            case "swift": self = .swift
            case "go": self = .go
            case "sh", "bash", "zsh": self = .shell
            case "json": self = .json
            case "yml", "yaml": self = .yaml
            case "md", "markdown": self = .markdown
            default: self = .plain
            }
        }

        /// Maps a fenced-code-block language hint (```swift, ```sh, …).
        public init(hint: String?) {
            switch hint?.trimmingCharacters(in: .whitespaces).lowercased() {
            case "swift": self = .swift
            case "go", "golang": self = .go
            case "sh", "bash", "zsh", "shell", "console": self = .shell
            case "json": self = .json
            case "yaml", "yml": self = .yaml
            case "md", "markdown": self = .markdown
            default: self = .plain
            }
        }

        var keywordRegex: NSRegularExpression? {
            switch self {
            case .swift: return CodeTokenizer.swiftKeywordRegex
            case .go: return CodeTokenizer.goKeywordRegex
            case .shell: return CodeTokenizer.shellKeywordRegex
            case .json, .yaml: return CodeTokenizer.literalKeywordRegex
            case .markdown, .plain: return nil
            }
        }

        var commentRegex: NSRegularExpression? {
            switch self {
            case .swift, .go: return CodeTokenizer.slashCommentRegex
            case .shell, .yaml: return CodeTokenizer.hashCommentRegex
            case .json, .markdown, .plain: return nil
            }
        }
    }

    public enum TokenKind: Hashable, CaseIterable, Sendable {
        case property, number, keyword, type, string, comment
    }

    public struct Token: Equatable, Sendable {
        public let range: NSRange
        public let kind: TokenKind
    }

    /// Tokenizes a single line. `.plain` and `.markdown` produce no tokens.
    public static func tokens(in line: String, language: Language) -> [Token] {
        var tokens: [Token] = []
        func apply(_ regex: NSRegularExpression?, _ kind: TokenKind) {
            guard let regex else { return }
            let full = NSRange(line.startIndex..<line.endIndex, in: line)
            for match in regex.matches(in: line, range: full) {
                tokens.append(Token(range: match.range, kind: kind))
            }
        }
        // Priority order; later kinds override earlier ones when applied in order.
        apply(propertyRegex, .property)
        apply(numberRegex, .number)
        apply(language.keywordRegex, .keyword)
        apply(typeRegex, .type)
        apply(stringRegex, .string)
        apply(language.commentRegex, .comment)
        return tokens
    }

    // MARK: - Regex tables

    private static let propertyRegex = regex(#"\.[A-Za-z_][A-Za-z0-9_]*"#)
    private static let numberRegex = regex(#"\b\d[\d_]*(?:\.[0-9]+)?\b"#)
    private static let typeRegex = regex(#"\b[A-Z][A-Za-z0-9_]*\b"#)
    private static let stringRegex = regex(#""[^"]*"|'[^']*'|`[^`]*`"#)
    private static let slashCommentRegex = regex(#"//[^\n]*"#)
    private static let hashCommentRegex = regex(#"#[^\n]*"#)
    private static let swiftKeywordRegex = keywordRegex([
        "import", "let", "var", "func", "private", "fileprivate", "public",
        "internal", "final", "class", "struct", "enum", "extension", "protocol",
        "if", "else", "guard", "return", "switch", "case", "default", "for", "in",
        "while", "do", "try", "catch", "throw", "throws", "rethrows", "async",
        "await", "defer", "init", "deinit", "super", "self", "Self", "where", "as",
        "is", "some", "any", "static", "override", "lazy", "weak", "unowned",
        "true", "false", "nil"
    ])
    private static let goKeywordRegex = keywordRegex([
        "package", "import", "func", "var", "const", "type", "struct", "interface",
        "map", "chan", "if", "else", "for", "range", "return", "switch", "case",
        "default", "go", "defer", "select", "break", "continue", "make", "new",
        "nil", "true", "false"
    ])
    private static let shellKeywordRegex = keywordRegex([
        "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "case",
        "esac", "while", "until", "function", "return", "local", "export", "set",
        "echo", "cd", "source"
    ])
    private static let literalKeywordRegex = keywordRegex(["true", "false", "null"])

    private static func keywordRegex(_ keywords: [String]) -> NSRegularExpression {
        let escaped = keywords.map { NSRegularExpression.escapedPattern(for: $0) }
        let pattern = "\\b(?:" + escaped.joined(separator: "|") + ")\\b"
        return regex(pattern)
    }

    private static func regex(_ pattern: String) -> NSRegularExpression {
        guard let regex = try? NSRegularExpression(pattern: pattern) else {
            preconditionFailure("Invalid code tokenizer regex: \(pattern)")
        }
        return regex
    }
}
