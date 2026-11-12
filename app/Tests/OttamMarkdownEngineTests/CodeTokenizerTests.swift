import Foundation
import Testing
@testable import OttamMarkdownEngine

@Suite("CodeTokenizer")
struct CodeTokenizerTests {
    private func slice(_ line: String, _ range: NSRange) -> String {
        (line as NSString).substring(with: range)
    }

    @Test("swift line tokenizes keywords, types, strings, numbers")
    func swiftLine() {
        let line = #"let name: String = "hi" + 42"#
        let tokens = CodeTokenizer.tokens(in: line, language: .swift)
        let byKind = Dictionary(grouping: tokens, by: \.kind)
        #expect(byKind[.keyword]?.map { slice(line, $0.range) } == ["let"])
        #expect(byKind[.type]?.map { slice(line, $0.range) } == ["String"])
        #expect(byKind[.string]?.map { slice(line, $0.range) } == [#""hi""#])
        #expect(byKind[.number]?.map { slice(line, $0.range) } == ["42"])
    }

    @Test("comment token covers the trailing comment")
    func swiftComment() {
        let line = "let x = 1 // trailing note"
        let tokens = CodeTokenizer.tokens(in: line, language: .swift)
        let comments = tokens.filter { $0.kind == .comment }.map { slice(line, $0.range) }
        #expect(comments == ["// trailing note"])
        // Comment is emitted last so in-order application overrides earlier tokens.
        #expect(tokens.last?.kind == .comment)
    }

    @Test("application order: string after type, comment last")
    func priorityOrder() {
        let line = #"Foo("Bar") // Baz"#
        let tokens = CodeTokenizer.tokens(in: line, language: .swift)
        let kindsInOrder = tokens.map(\.kind)
        let typeIndex = kindsInOrder.firstIndex(of: .type)
        let stringIndex = kindsInOrder.firstIndex(of: .string)
        let commentIndex = kindsInOrder.firstIndex(of: .comment)
        #expect(typeIndex != nil && stringIndex != nil && commentIndex != nil)
        #expect(typeIndex! < stringIndex!)
        #expect(stringIndex! < commentIndex!)
    }

    @Test("shell uses hash comments and shell keywords")
    func shellLine() {
        let line = "if true; then echo hi; fi # done"
        let tokens = CodeTokenizer.tokens(in: line, language: .shell)
        let keywords = Set(tokens.filter { $0.kind == .keyword }.map { slice(line, $0.range) })
        #expect(keywords.isSuperset(of: ["if", "then", "echo", "fi"]))
        let comments = tokens.filter { $0.kind == .comment }.map { slice(line, $0.range) }
        #expect(comments == ["# done"])
    }

    @Test("json highlights literals but no comments")
    func jsonLine() {
        let line = #"{"ok": true, "n": 3}"#
        let tokens = CodeTokenizer.tokens(in: line, language: .json)
        #expect(tokens.contains { $0.kind == .keyword && slice(line, $0.range) == "true" })
        #expect(!tokens.contains { $0.kind == .comment })
    }

    @Test("plain and markdown produce property/number/type/string only via shared regexes")
    func plainLine() {
        let tokens = CodeTokenizer.tokens(in: "just Words 12", language: .plain)
        #expect(!tokens.contains { $0.kind == .keyword })
        #expect(!tokens.contains { $0.kind == .comment })
    }

    @Test("language hint mapping")
    func hintMapping() {
        #expect(CodeTokenizer.Language(hint: "swift") == .swift)
        #expect(CodeTokenizer.Language(hint: "golang") == .go)
        #expect(CodeTokenizer.Language(hint: "ZSH") == .shell)
        #expect(CodeTokenizer.Language(hint: "yml") == .yaml)
        #expect(CodeTokenizer.Language(hint: nil) == .plain)
        #expect(CodeTokenizer.Language(hint: "mystery") == .plain)
    }

    @Test("file path mapping — the diff reviewer's entry point")
    func pathMapping() {
        #expect(CodeTokenizer.Language(path: "app/Sources/Foo.swift") == .swift)
        #expect(CodeTokenizer.Language(path: "cmd/main.go") == .go)
        #expect(CodeTokenizer.Language(path: "scripts/run-app.SH") == .shell)
        #expect(CodeTokenizer.Language(path: "Package.json") == .json)
        #expect(CodeTokenizer.Language(path: ".github/workflows/ci.yml") == .yaml)
        #expect(CodeTokenizer.Language(path: "README.md") == .markdown)
        #expect(CodeTokenizer.Language(path: "Makefile") == .plain)
    }

    @Test("a path and a fence hint resolve to the same language")
    func pathAndHintAgree() {
        // The two entry points used to live in different types with different
        // tables; they are one table now and must not drift again.
        for (path, hint) in [
            ("a.swift", "swift"), ("a.go", "go"), ("a.sh", "bash"),
            ("a.json", "json"), ("a.yaml", "yml"), ("a.md", "markdown")
        ] {
            #expect(
                CodeTokenizer.Language(path: path) == CodeTokenizer.Language(hint: hint),
                "\(path) and ```\(hint) disagree"
            )
        }
    }
}
