import Foundation

/// The YAML block that opens a doc (`---` … `---`), read as a flat list of
/// properties. Markdown would draw it as a rule and a setext heading, so a
/// file tab shows it as a property list above the body instead.
///
/// Only flat `key: value` lines count, with indented or `- ` lines folding
/// into the key above and `|`/`>` block scalars read as their text.
/// Anything else, such as a nested mapping, leaves the text alone, so a doc
/// that opens with a plain `---` rule still renders as written.
struct MarkdownFrontmatter: Equatable, Sendable {
    struct Field: Equatable, Sendable {
        var key: String
        var value: String

        /// `last_verified` reads as "Last verified".
        var label: String {
            let words = key.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ")
            return words.prefix(1).uppercased() + words.dropFirst()
        }
    }

    var fields: [Field]

    /// The frontmatter and the markdown after it, or nil and the text
    /// unchanged when it doesn't open with one.
    static func split(_ text: String) -> (frontmatter: MarkdownFrontmatter?, body: String) {
        // `\r\n` is one Character, so lines end at any newline Character.
        var index = text.startIndex
        func nextLine() -> Substring? {
            guard index < text.endIndex else { return nil }
            let end = text[index...].firstIndex(where: \.isNewline) ?? text.endIndex
            defer { index = end < text.endIndex ? text.index(after: end) : end }
            return text[index..<end]
        }
        guard let opening = nextLine(), trimmed(opening) == "---" else { return (nil, text) }
        var fields: [Field] = []
        // `|` or `>` while the last key's value is a block scalar.
        var scalar: Character?
        while let line = nextLine() {
            let content = trimmed(line)
            let isIndented = line.first == " " || line.first == "\t"
            if let style = scalar, isIndented || content.isEmpty {
                // A block scalar's lines are literal: `|` keeps their breaks,
                // `>` folds them into one line.
                guard !content.isEmpty else { continue }
                let value = fields[fields.count - 1].value
                fields[fields.count - 1].value = value.isEmpty ? content : value + (style == "|" ? "\n" : " ") + content
                continue
            }
            scalar = nil
            if content == "---" || content == "..." {
                guard !fields.isEmpty else { return (nil, text) }
                return (MarkdownFrontmatter(fields: fields), String(text[index...]))
            }
            if content.isEmpty || content.hasPrefix("#") { continue }
            if isIndented || content.hasPrefix("- ") {
                let item = unquoted(content.hasPrefix("- ") ? String(content.dropFirst(2)) : content)
                // A nested mapping (`author:` then `  name: x`, or a list of
                // them) isn't flat, so the block renders as written.
                guard !fields.isEmpty, item.wholeMatch(of: Self.key) == nil else { return (nil, text) }
                fields[fields.count - 1].value += fields[fields.count - 1].value.isEmpty ? item : ", \(item)"
                continue
            }
            guard let match = content.wholeMatch(of: Self.key) else {
                return (nil, text)
            }
            let value = unquoted(String(match.2 ?? ""))
            if value.wholeMatch(of: #/[|>][-+0-9]*/#) != nil {
                scalar = value.first
                fields.append(Field(key: String(match.1), value: ""))
            } else {
                fields.append(Field(key: String(match.1), value: value))
            }
        }
        // An unclosed block is a rule followed by prose, not frontmatter.
        return (nil, text)
    }

    /// `key: value`, or `key:` alone.
    nonisolated(unsafe) private static let key = #/([A-Za-z0-9_][A-Za-z0-9_ -]*):(?:\s+(.*))?/#

    private static func trimmed(_ line: Substring) -> String {
        line.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func unquoted(_ value: String) -> String {
        let value = value.trimmingCharacters(in: .whitespaces)
        guard value.count >= 2, let first = value.first, first == "\"" || first == "'", value.last == first else {
            return value
        }
        return String(value.dropFirst().dropLast())
    }
}
