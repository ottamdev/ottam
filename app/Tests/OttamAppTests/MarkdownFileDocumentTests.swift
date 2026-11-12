import Foundation
import Testing
@testable import OttamApp

@MainActor @Suite("MarkdownFileDocument")
struct MarkdownFileDocumentTests {
    private let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)

    private func withFolder(_ body: (URL) async throws -> Void) async throws {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        try await body(folder)
    }

    private func waitUntil(_ condition: () -> Bool) async throws {
        for _ in 0..<300 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(condition())
    }

    @Test("reads a UTF-8 file")
    func loadsUTF8() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("a.md")
            try "# A".write(to: url, atomically: false, encoding: .utf8)
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .loaded(.init("# A")) }
        }
    }

    @Test("an edit in place reloads")
    func writeInPlaceReloads() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("a.md")
            try "# A".write(to: url, atomically: false, encoding: .utf8)
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .loaded(.init("# A")) }
            try "# B".write(to: url, atomically: false, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# B")) }
        }
    }

    @Test("a save that replaces the file reloads, and so does the next one")
    func atomicReplaceReloads() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("a.md")
            try "# A".write(to: url, atomically: true, encoding: .utf8)
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .loaded(.init("# A")) }
            try "# B".write(to: url, atomically: true, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# B")) }
            try "# C".write(to: url, atomically: false, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# C")) }
        }
    }

    @Test("a deleted file shows missing, and comes back when recreated")
    func deleteThenRecreate() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("a.md")
            try "# A".write(to: url, atomically: false, encoding: .utf8)
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .loaded(.init("# A")) }
            try FileManager.default.removeItem(at: url)
            try await waitUntil { document.content == .missing }
            try "# B".write(to: url, atomically: false, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# B")) }
        }
    }

    @Test("a file that doesn't exist yet loads once it does")
    func missingThenCreated() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("later.md")
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .missing }
            try "# Later".write(to: url, atomically: true, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# Later")) }
        }
    }

    @Test("a file in a folder that doesn't exist yet loads once both do")
    func missingFolderThenCreated() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("plans/later.md")
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .missing }
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try "# Later".write(to: url, atomically: true, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# Later")) }
        }
    }

    @Test("a folder replaced at the same path is followed")
    func replacedFolder() async throws {
        try await withFolder { folder in
            let sub = folder.appendingPathComponent("sub")
            let url = sub.appendingPathComponent("a.md")
            try FileManager.default.createDirectory(at: sub, withIntermediateDirectories: true)
            try "# A".write(to: url, atomically: false, encoding: .utf8)
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .loaded(.init("# A")) }
            try FileManager.default.removeItem(at: sub)
            try await waitUntil { document.content == .missing }
            try FileManager.default.createDirectory(at: sub, withIntermediateDirectories: true)
            try "# B".write(to: url, atomically: false, encoding: .utf8)
            try await waitUntil { document.content == .loaded(.init("# B")) }
        }
    }

    @Test("steady writes show while they continue")
    func steadyWritesShow() async throws {
        try await withFolder { folder in
            let url = folder.appendingPathComponent("a.md")
            try "# 0".write(to: url, atomically: false, encoding: .utf8)
            let document = MarkdownFileDocument(url: url)
            defer { document.stop() }
            try await waitUntil { document.content == .loaded(.init("# 0")) }
            var shownDuringWrites = false
            for index in 1...30 {
                try "# \(index)".write(to: url, atomically: false, encoding: .utf8)
                try await Task.sleep(for: .milliseconds(30))
                if index < 30, document.content != .loaded(.init("# 0")) { shownDuringWrites = true }
            }
            #expect(shownDuringWrites)
            try await waitUntil { document.content == .loaded(.init("# 30")) }
        }
    }

    @Test("oversized and non-UTF-8 files are refused")
    func refusals() throws {
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let large = folder.appendingPathComponent("large.md")
        try Data(count: MarkdownFileDocument.sizeLimit + 1).write(to: large)
        #expect(MarkdownFileDocument.read(large) == .tooLarge)
        let binary = folder.appendingPathComponent("binary.md")
        try Data([0xFF, 0xFE, 0xFD]).write(to: binary)
        #expect(MarkdownFileDocument.read(binary) == .notText)
        #expect(MarkdownFileDocument.read(folder) == .unreadable)
        #expect(MarkdownFileDocument.read(folder.appendingPathComponent("none.md")) == .missing)
    }
}

@Suite("MarkdownFrontmatter")
struct MarkdownFrontmatterTests {
    @Test("flat keys split from the body, with readable labels")
    func splitsFlatKeys() throws {
        let (frontmatter, body) = MarkdownFrontmatter.split(
            "---\nkind: spec\nlast_verified: 2026-10-20\ntitle: \"Plan: review\"\n---\n\n# Plan Review\n"
        )
        let fields = try #require(frontmatter).fields
        #expect(fields.map(\.label) == ["Kind", "Last verified", "Title"])
        #expect(fields.map(\.value) == ["spec", "2026-10-20", "Plan: review"])
        #expect(body == "\n# Plan Review\n")
    }

    @Test("list items fold into the key above")
    func foldsLists() throws {
        let (frontmatter, _) = MarkdownFrontmatter.split("---\ntags:\n  - a\n  - 'b'\nowner: x\n---\nbody")
        #expect(try #require(frontmatter).fields == [.init(key: "tags", value: "a, b"), .init(key: "owner", value: "x")])
    }

    @Test("text that isn't frontmatter is left as written")
    func leavesOtherText() {
        for text in [
            "# Title\n---\nkind: spec\n---\n",
            "---\nJust a rule, then prose.\n---\n",
            "---\nkind: spec\n",
            "---\n---\nbody",
            "---\nkind: spec\nauthor:\n  name: Alice\n---\nbody",
            "---\nowners:\n  - name: Alice\n---\nbody",
        ] {
            let (frontmatter, body) = MarkdownFrontmatter.split(text)
            #expect(frontmatter == nil)
            #expect(body == text)
        }
    }

    @Test("block scalars read as text, not as their indicator")
    func blockScalars() throws {
        let (frontmatter, _) = MarkdownFrontmatter.split(
            "---\nsummary: >-\n  Two lines\n  of prose.\nnotes: |\n  one\n  two: three\nowner: x\n---\nbody"
        )
        #expect(try #require(frontmatter).fields == [
            .init(key: "summary", value: "Two lines of prose."),
            .init(key: "notes", value: "one\ntwo: three"),
            .init(key: "owner", value: "x"),
        ])
    }

    @Test("CRLF files split too")
    func crlf() throws {
        let (frontmatter, body) = MarkdownFrontmatter.split("---\r\nkind: spec\r\n---\r\nbody")
        #expect(try #require(frontmatter).fields == [.init(key: "kind", value: "spec")])
        #expect(body == "body")
    }
}
