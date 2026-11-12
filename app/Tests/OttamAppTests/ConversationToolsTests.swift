import OttamBackendClient
import OttamCore
import Foundation
import Testing
@testable import OttamApp

@MainActor @Suite("ConversationToolsStore")
struct ConversationToolsStoreTests {
    private let first = ConversationID("first")
    private let second = ConversationID("second")
    private let term = ConversationTool.terminal(TerminalID("term"))

    private func store() -> ConversationToolsStore {
        ConversationToolsStore { project in
            DiffReviewModel(
                projectSummary: project,
                client: OttamCoreProcessClient(executableURL: URL(fileURLWithPath: "/nonexistent/ottam-core"))
            )
        }
    }

    @Test("one Subagent tab retargets to each thread a card opens")
    func subagentTabIsReused() {
        let store = store()
        store.show(.review, in: first)
        store.showSubagent("subagent:a", in: first)
        store.showSubagent("subagent:b", in: first)
        #expect(store.state(for: first).tools == [.review, .subagent])
        #expect(store.state(for: first).selected == .subagent)
        #expect(store.state(for: first).subagentThreadID == "subagent:b")
        #expect(store.isShown(for: first))
        #expect(store.state(for: second).subagentThreadID == nil)
    }

    @Test("one Plan tab shows the latest plan, over the selected tool")
    func planTabIsReused() {
        let store = store()
        store.show(term, in: first)
        store.showPlan(ChatPlan(title: "Implement this plan?", markdown: "# A"), in: first)
        store.select(term, in: first)
        store.showPlan(ChatPlan(title: "Implement this plan?", markdown: "# B"), in: first)
        #expect(store.state(for: first).tools == [term, .plan])
        #expect(store.state(for: first).selected == .plan)
        #expect(store.state(for: first).plan?.markdown == "# B")
        #expect(store.state(for: second).plan == nil)
    }

    @Test("a file opens in its own tab, and opening it again selects that tab")
    func fileTabIsReusedPerPath() throws {
        let store = store()
        let a = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/docs/a.md")))
        let b = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/docs/b.md")))
        store.showFile(a, in: first)
        store.showFile(b, in: first)
        store.open(URL(fileURLWithPath: "/repo/docs/../docs/a.md"), in: first)
        #expect(store.state(for: first).tools == [.file(a), .file(b)])
        #expect(store.state(for: first).selected == .file(a))
        #expect(store.state(for: second).tools.isEmpty)
    }

    @Test("files that share a name get separate tabs with telling titles")
    func sameNameFilesGetSeparateTabs() throws {
        let store = store()
        let app = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/app/README.md")))
        let docs = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/docs/README.md")))
        let plan = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/docs/plan.md")))
        store.showFile(app, in: first)
        store.showFile(docs, in: first)
        store.showFile(plan, in: first)
        let state = store.state(for: first)
        #expect(state.tools.count == 3)
        let titles = state.titles()
        #expect(titles[.file(app)] == "README.md — app")
        #expect(titles[.file(docs)] == "README.md — docs")
        #expect(titles[.file(plan)] == "plan.md")
        #expect(titles.count == 3)
    }

    @Test("titles add parent folders until same-name files differ")
    func fileTabTitlesDisambiguate() {
        let titles = ConversationToolsState.fileTabTitles([
            "/repo/specs/a/plan.md", "/repo/specs/b/plan.md", "/other/specs/a/plan.md", "/repo/NOW.md",
        ])
        #expect(titles["/repo/specs/a/plan.md"] == "plan.md — repo/specs/a")
        #expect(titles["/repo/specs/b/plan.md"] == "plan.md — repo/specs/b")
        #expect(titles["/other/specs/a/plan.md"] == "plan.md — other/specs/a")
        #expect(titles["/repo/NOW.md"] == "NOW.md")
    }

    @Test("a symlinked spelling of a file shares its tab")
    func symlinkedPathSharesTab() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("a.md")
        try "# A".write(to: file, atomically: true, encoding: .utf8)
        let link = folder.appendingPathComponent("link.md")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)
        #expect(MarkdownFileID(url: link) == MarkdownFileID(url: file))
    }

    @Test("only markdown files get a file tab")
    func onlyMarkdownOpensInTab() {
        #expect(MarkdownFileID(url: URL(fileURLWithPath: "/repo/A.MD")) != nil)
        #expect(MarkdownFileID(url: URL(fileURLWithPath: "/repo/a.markdown")) != nil)
        #expect(MarkdownFileID(url: URL(fileURLWithPath: "/repo/a.swift")) == nil)
        #expect(MarkdownFileID(url: URL(string: "https://example.com/a.md")!) == nil)
    }

    @Test("a plan opens its tab without touching file tabs")
    func planKeepsFileTabs() throws {
        let store = store()
        let a = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/a.md")))
        store.showFile(a, in: first)
        store.showPlan(ChatPlan(title: "Implement this plan?", markdown: "# A"), in: first)
        #expect(store.state(for: first).tools == [.file(a), .plan])
        #expect(store.state(for: first).selected == .plan)
    }

    @Test("closing a file's last tab releases its document")
    func closingLastFileTabReleasesDocument() throws {
        let store = store()
        let a = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/a.md")))
        store.showFile(a, in: first)
        store.showFile(a, in: second)
        let document = store.document(for: a)
        store.close(.file(a), in: first)
        #expect(store.document(for: a) === document)
        store.close(.file(a), in: second)
        #expect(store.document(for: a) !== document)
    }

    @Test("two conversations on one file share its document but keep their own places")
    func readingPositionsPerTab() throws {
        let store = store()
        let a = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/a.md")))
        store.showFile(a, in: first)
        store.showFile(a, in: second)
        let mine = store.readingPosition(for: a, in: first)
        #expect(store.document(for: a) === store.document(for: a))
        #expect(store.readingPosition(for: a, in: first) === mine)
        #expect(store.readingPosition(for: a, in: second) !== mine)
        store.close(.file(a), in: first)
        store.showFile(a, in: first)
        #expect(store.readingPosition(for: a, in: first) !== mine)
    }

    @Test("closing the window releases every document")
    func releasingDocuments() throws {
        let store = store()
        let a = try #require(MarkdownFileID(url: URL(fileURLWithPath: "/repo/a.md")))
        store.showFile(a, in: first)
        let document = store.document(for: a)
        store.releaseDocuments()
        #expect(store.document(for: a) !== document)
    }

    @Test("tabs and visibility belong to their conversation")
    func perConversation() {
        let store = store()
        store.show(.review, in: first)
        store.show(term, in: first)
        store.select(.review, in: first)
        store.toggleVisibility(for: first)
        store.toggleVisibility(for: second)
        #expect(store.state(for: first).selected == .review)
        #expect(!store.isShown(for: first))
        #expect(store.isShown(for: second))
        #expect(!store.isShown(for: nil))
        store.toggleVisibility(for: first)
        #expect(store.state(for: first).tools == [.review, term])
        #expect(store.isShown(for: first))
        // A conversation without tabs shows the picker; it opens nothing.
        #expect(store.state(for: second).tools.isEmpty)
        store.toggleVisibility(for: second)
        #expect(!store.isShown(for: second))
    }

    @Test("closing the selected tab picks its neighbor; the last tab leaves the picker")
    func closeNeighborThenPicker() {
        let store = store()
        store.show(.review, in: first)
        store.show(term, in: first)
        store.show(.review, in: first)
        #expect(store.state(for: first).tools.count == 2)
        store.select(term, in: first)
        #expect(store.close(term, in: first))
        #expect(store.state(for: first).selected == .review)
        store.close(.review, in: first)
        #expect(store.isShown(for: first))
        #expect(store.state(for: first).tools.isEmpty)
        #expect(store.state(for: first).selected == nil)
        store.select(term, in: first)
        #expect(store.state(for: first).selected == nil)
    }

    @Test("a focus request is one-shot and only for its own tab")
    func focusRequest() {
        let store = store()
        store.show(term, in: first, focus: true)
        _ = store.remove(second)
        #expect(!store.consumeFocus(for: .review, in: first))
        #expect(!store.consumeFocus(for: term, in: second))
        #expect(store.consumeFocus(for: term, in: first))
        #expect(!store.consumeFocus(for: term, in: first))
    }

    @Test("selecting a tab asks it for focus once it mounts; the selected tab is already mounted")
    func selectFocus() {
        let store = store()
        store.show(.review, in: first)
        store.show(term, in: first)
        store.select(.review, in: first, focus: true)
        #expect(store.state(for: first).selected == .review)
        #expect(store.consumeFocus(for: .review, in: first))
        // Nothing would consume it: the session focuses a selected terminal
        // directly.
        store.show(term, in: first, focus: true)
        store.select(term, in: first, focus: true)
        #expect(!store.consumeFocus(for: term, in: first))
    }

    @Test("next and previous tab wrap at the ends, asking for focus only when told to")
    func selectAdjacent() {
        let store = store()
        let other = ConversationTool.terminal(TerminalID("other"))
        store.show(.review, in: first)
        store.show(term, in: first)
        store.show(other, in: first)
        store.selectAdjacent(1, in: first, focus: true)
        #expect(store.state(for: first).selected == .review)
        #expect(store.consumeFocus(for: .review, in: first))
        store.selectAdjacent(-1, in: first, focus: false)
        #expect(store.state(for: first).selected == other)
        #expect(!store.consumeFocus(for: other, in: first))
        store.selectAdjacent(-1, in: first, focus: true)
        #expect(store.state(for: first).selected == term)
        #expect(store.consumeFocus(for: term, in: first))
        // A lone tab stays, and asks for no focus it would never mount for.
        store.show(.review, in: second)
        store.selectAdjacent(1, in: second, focus: true)
        #expect(store.state(for: second).selected == .review)
        #expect(!store.consumeFocus(for: .review, in: second))
    }

    @Test("anything else that changes the pane cancels a focus request")
    func focusCancels() {
        let cancels: [(ConversationToolsStore) -> Void] = [
            { $0.select(.review, in: first) },
            { $0.selectAdjacent(1, in: first, focus: false) },
            { _ = $0.close(.review, in: first) },
            { $0.toggleVisibility(for: first) },
            { $0.show(.review, in: first) },
            { $0.cancelFocus() },
            { _ = $0.remove(first) },
        ]
        for cancel in cancels {
            let store = store()
            store.show(.review, in: first)
            store.show(term, in: first, focus: true)
            cancel(store)
            #expect(!store.consumeFocus(for: term, in: first))
        }
    }

    @Test("pruning closes only the terminal tabs whose terminal is gone")
    func pruneTerminals() {
        let store = store()
        let kept = ConversationTool.terminal(TerminalID("kept"))
        store.show(.review, in: first)
        store.show(kept, in: first)
        store.show(term, in: first)
        store.show(ConversationTool.terminal(TerminalID("gone")), in: second)
        store.pruneTerminals(keeping: [TerminalID("kept")])
        #expect(store.state(for: first).tools == [.review, kept])
        #expect(store.state(for: first).selected == kept)
        #expect(store.state(for: second).tools.isEmpty)
        #expect(store.isShown(for: second))
    }

    @Test("a removed conversation hands back its tools and keeps no state")
    func removeConversation() {
        let store = store()
        store.show(.review, in: first)
        store.show(term, in: first)
        #expect(store.remove(first) == [.review, term])
        #expect(store.state(for: first) == ConversationToolsState())
        #expect(store.remove(first).isEmpty)
    }

    @Test("terminals are numbered once there are two")
    func ordinals() {
        let other = ConversationTool.terminal(TerminalID("other"))
        var state = ConversationToolsState(tools: [.review, term])
        #expect(state.ordinal(of: term) == nil)
        state.tools.append(other)
        #expect(state.ordinal(of: .review) == nil)
        #expect(state.ordinal(of: term) == 1)
        #expect(state.ordinal(of: other) == 2)
    }

    @Test("review models are shared per project and evicted with it")
    func reviewCache() {
        let store = store()
        let alpha = resolved("alpha")
        let model = store.reviewModel(for: alpha)
        #expect(model.projectSummary == alpha)
        #expect(store.reviewModel(for: alpha) === model)
        store.retainReviewModels(for: [ProjectSourceID("source-beta")])
        #expect(store.reviewModel(for: alpha) !== model)
    }

    // The core here never launches, so every load fails.
    @Test("a failed first load shows the error; a failed refresh keeps the snapshot")
    func reviewLoadFailure() async {
        let model = store().reviewModel(for: resolved("alpha"))
        await model.load()
        #expect(model.snapshot == nil)
        #expect(model.errorText != nil)

        let shown = CoreDiffSnapshot(
            repoPath: "/tmp/alpha", branch: "main", ahead: 0, behind: 0,
            additions: 0, deletions: 0, files: [], untrackedFiles: [], omittedFiles: []
        )
        model.snapshot = shown
        await model.load()
        #expect(model.snapshot == shown)
        #expect(model.errorText == nil)
        #expect(!model.isLoading)
    }

    private func resolved(_ name: String) -> ResolvedProject {
        let projectID = ProjectID("project-\(name)")
        return ResolvedProject(
            project: ProjectSummary(
                projectID: projectID, displayName: name, createdAt: 0, updatedAt: 0, lastOpenedAt: 0
            ),
            source: ProjectSourceSummary(
                projectSourceID: ProjectSourceID("source-\(name)"),
                projectID: projectID,
                kind: .localGit,
                displayName: name,
                canonicalPath: "/tmp/\(name)",
                createdAt: 0,
                updatedAt: 0
            )
        )
    }
}

@Suite("Tools tab strip")
struct ToolTabStripTests {
    private typealias Strip = ToolTabStripLayout

    @Test("a lone tab fills the strip")
    func loneTab() {
        #expect(Strip.widths(count: 1, selected: 0, available: 258) == [258])
    }

    @Test("the selected tab is weighted and capped, but never narrower than an even split")
    func selectedShare() {
        let narrow = Strip.widths(count: 3, selected: 1, available: 600)
        #expect(narrow[1] > narrow[0])
        #expect(Strip.widths(count: 4, selected: 0, available: 1000)[0] == Strip.selectedWidths.upperBound)
        #expect(Strip.widths(count: 2, selected: 0, available: 800) == [400, 400])
    }

    @Test("tabs fit the strip until the selected tab reaches its floor; then it scrolls")
    func fitThenScroll() {
        for count in 2...8 {
            for available in stride(from: CGFloat(120), through: 900, by: 1) {
                let widths = Strip.widths(count: count, selected: count / 2, available: available)
                let total = widths.reduce(0, +)
                let room = available - CGFloat(count - 1) * Strip.collapsedWidth
                #expect(widths.allSatisfy { $0 >= Strip.collapsedWidth })
                #expect(widths[count / 2] >= Strip.selectedFloor)
                #expect(widths.allSatisfy { $0 <= widths[count / 2] + 0.001 }, "\(widths)")
                if room >= Strip.selectedFloor {
                    #expect(total <= available + 0.001, "\(count) tabs in \(available)")
                } else {
                    #expect(total > available, "\(count) tabs in \(available)")
                }
            }
        }
    }

    @Test("a pane at its minimum labels one other tab and collapses the rest")
    func collapseAtMinimum() {
        // A 280 pane's strip, less its insets.
        let two = Strip.widths(count: 2, selected: 0, available: 258)
        #expect(two[1] >= ConversationToolTab.labelMinimum)
        let three = Strip.widths(count: 3, selected: 0, available: 258)
        #expect(three[1] < ConversationToolTab.labelMinimum)
    }

    @Test("close shows on the selected tab, and on hover only where it clears the label")
    func closeVisibility() {
        let clear = ConversationToolTab.closeClearance
        #expect(ConversationToolTab.showsClose(selected: true, labelInset: 8, hovered: false))
        #expect(!ConversationToolTab.showsClose(selected: false, labelInset: clear, hovered: false))
        #expect(ConversationToolTab.showsClose(selected: false, labelInset: clear, hovered: true))
        #expect(!ConversationToolTab.showsClose(selected: false, labelInset: clear - 1, hovered: true))
    }
}

@MainActor @Suite("Reopen plan")
struct ReopenPlanTests {
    private func permission(
        _ id: String, position: UInt64, plan: String?, thread: String? = nil
    ) -> ConversationTranscriptItem {
        var item = TranscriptFixture.item(id, position: position, body: "")
        item.kind = .permission
        item.subagentThreadID = thread
        item.permission = TranscriptPermission(
            requestID: id, title: "Implement this plan?", outcomeLabel: "Allow once", allowed: true, plan: plan
        )
        return item
    }

    @Test("Plan reopens the newest main-thread plan record in the loaded transcript")
    func newestMainThreadPlanRecord() {
        let model = ChatWorkspaceModel()
        let id = TranscriptFixture.conversationID
        model.selectedConversationID = id
        #expect(model.selectedLatestPlanRecord == nil)
        model.transcriptStore.merge(.init(coreInstanceID: .init("core"), sequence: 1, items: [
            permission("old", position: 1, plan: "# Old"),
            permission("new", position: 3, plan: "# New"),
            permission("subagent", position: 5, plan: "# Subagent", thread: "subagent:a"),
            permission("tool", position: 6, plan: nil),
            TranscriptFixture.item("answer", position: 7),
        ]), conversationID: id)
        #expect(model.selectedLatestPlanRecord == ChatPlan(title: "Implement this plan?", markdown: "# New"))
        model.selectedConversationID = ConversationID("other")
        #expect(model.selectedLatestPlanRecord == nil, "another conversation's transcript is not this one's")
    }
}
