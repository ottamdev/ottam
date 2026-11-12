import AppKit
import OttamBackendClient
import Observation

/// One tab in a conversation's tools pane.
enum ConversationTool: Hashable {
    case review
    case terminal(TerminalID)
    /// One reusable tab: a click on a subagent card retargets it.
    case subagent
    /// One reusable tab: the latest plan an agent asked to approve.
    case plan
    /// One tab per markdown file, so opening an open file selects its tab.
    case file(MarkdownFileID)

    var kind: ConversationToolKind {
        switch self {
        case .review: .review
        case .terminal: .terminal
        case .subagent: .subagent
        case .plan: .plan
        case .file: .file
        }
    }
}

/// A markdown file a tab shows, named by its resolved absolute path: two
/// spellings of one file share a tab, and two files that share a name don't.
struct MarkdownFileID: Hashable {
    static let extensions: Set<String> = ["md", "markdown"]

    let path: String

    /// Nil unless `url` is a markdown file URL.
    init?(url: URL) {
        guard url.isFileURL, Self.extensions.contains(url.pathExtension.lowercased()) else { return nil }
        path = url.standardizedFileURL.resolvingSymlinksInPath().path
    }

    var url: URL { URL(fileURLWithPath: path) }
    var name: String { url.lastPathComponent }
}

/// One conversation's tab for one file.
private struct FileTab: Hashable {
    let conversation: ConversationID
    let file: MarkdownFileID
}

/// A kind of tool, as its tabs, Add Tool, and the picker name it.
enum ConversationToolKind {
    case review
    case terminal
    case subagent
    case plan
    case file

    /// Terminal's icon, a prompt set in SF Mono: SF Symbols draws one only
    /// inside a window frame, too small to read at a tab's size.
    static let terminalGlyph = ">_"

    var title: String {
        switch self {
        case .review: "Review"
        case .terminal: "Terminal"
        case .subagent: "Subagent"
        case .plan: "Plan"
        case .file: "File"
        }
    }
}

struct ConversationToolsState: Equatable {
    var tools: [ConversationTool] = []
    var selected: ConversationTool?
    /// Whether the pane shows. Without tabs it shows the tool picker.
    var isShown = false
    /// The thread the Subagent tab shows.
    var subagentThreadID: String?
    /// The plan the Plan tab shows. It outlives its request, so a plan stays
    /// readable after it is approved.
    var plan: ChatPlan?

    /// Numbers terminals once there are two, since collapsed tabs show only
    /// their icon. Review is single-instance.
    func ordinal(of tool: ConversationTool) -> Int? {
        guard case .terminal = tool else { return nil }
        let terminals = tools.filter { if case .terminal = $0 { true } else { false } }
        guard terminals.count > 1 else { return nil }
        return terminals.firstIndex(of: tool).map { $0 + 1 }
    }

    /// Each tab's name: its kind, or a file tab's file name, with as many
    /// parent folders as it takes to tell same-name files apart.
    func titles() -> [ConversationTool: String] {
        let files = Self.fileTabTitles(tools.compactMap { if case .file(let file) = $0 { file.path } else { nil } })
        var titles: [ConversationTool: String] = [:]
        for tool in tools {
            if case .file(let file) = tool {
                titles[tool] = files[file.path] ?? file.name
            } else {
                titles[tool] = tool.kind.title
            }
        }
        return titles
    }

    /// Titles each path by its file name; where names collide, adds trailing
    /// parent folders until every title differs: `README.md — app`.
    static func fileTabTitles(_ paths: [String]) -> [String: String] {
        let components = Dictionary(paths.map { ($0, URL(fileURLWithPath: $0).pathComponents) }) { first, _ in first }
        var titles: [String: String] = [:]
        for group in Dictionary(grouping: components.keys, by: { components[$0]?.last ?? $0 }).values {
            guard group.count > 1 else {
                if let path = group.first { titles[path] = URL(fileURLWithPath: path).lastPathComponent }
                continue
            }
            let deepest = group.map { (components[$0]?.count ?? 1) - 1 }.max() ?? 0
            var depth = 1
            while depth < deepest {
                let parents = group.map { parentSuffix(components[$0] ?? [], depth: depth) }
                if Set(parents).count == group.count { break }
                depth += 1
            }
            for path in group {
                let parts = components[path] ?? []
                titles[path] = "\(parts.last ?? path) — \(parentSuffix(parts, depth: depth))"
            }
        }
        return titles
    }

    /// The last `depth` folders above a path's file name.
    private static func parentSuffix(_ components: [String], depth: Int) -> String {
        let folders = components.dropLast().filter { $0 != "/" }
        return folders.suffix(depth).joined(separator: "/")
    }
}

/// Each conversation's tools pane: its tabs and whether it shows, for the
/// window's lifetime. Nothing here is persisted.
@MainActor @Observable
final class ConversationToolsStore {
    private var states: [ConversationID: ConversationToolsState] = [:]
    /// A tool that takes keyboard focus once its view mounts, asked for by
    /// Open Terminal, a tab click, and Show Next/Previous Tab. Anything else
    /// that changes the tabs or the pane's visibility, or switches
    /// conversations, cancels it.
    @ObservationIgnored private var focusRequest: (conversation: ConversationID, tool: ConversationTool)?

    /// A cache, not state: views observe each `DiffReviewModel` directly, and a
    /// miss fills this during a view update.
    @ObservationIgnored private var reviewModels: [ProjectSourceID: DiffReviewModel] = [:]
    @ObservationIgnored private var documents: [MarkdownFileID: MarkdownFileDocument] = [:]
    @ObservationIgnored private var readingPositions: [FileTab: ReadingPosition] = [:]
    @ObservationIgnored private let makeReviewModel: (ResolvedProject) -> DiffReviewModel

    init(makeReviewModel: @escaping (ResolvedProject) -> DiffReviewModel) {
        self.makeReviewModel = makeReviewModel
    }

    func state(for id: ConversationID) -> ConversationToolsState {
        states[id] ?? ConversationToolsState()
    }

    func isShown(for id: ConversationID?) -> Bool {
        id.map { state(for: $0).isShown } ?? false
    }

    /// Adds `tool` if needed, selects it, and shows the pane; with `focus`,
    /// the tool takes keyboard focus once it mounts.
    func show(_ tool: ConversationTool, in id: ConversationID, focus: Bool = false) {
        var state = state(for: id)
        if !state.tools.contains(tool) { state.tools.append(tool) }
        state.selected = tool
        state.isShown = true
        states[id] = state
        focusRequest = focus ? (id, tool) : nil
    }

    /// Shows `thread` in the Subagent tab, adding the tab if needed.
    func showSubagent(_ thread: String, in id: ConversationID) {
        states[id, default: ConversationToolsState()].subagentThreadID = thread
        show(.subagent, in: id)
    }

    /// Shows `plan` in the Plan tab, adding the tab if needed.
    func showPlan(_ plan: ChatPlan, in id: ConversationID) {
        states[id, default: ConversationToolsState()].plan = plan
        show(.plan, in: id)
    }

    /// Shows `file` in its own tab, selecting the tab if it is already open.
    func showFile(_ file: MarkdownFileID, in id: ConversationID) {
        show(.file(file), in: id)
    }

    /// Opens a link or a read file: markdown in a file tab, anything else in
    /// its app.
    func open(_ url: URL, in id: ConversationID) {
        if let file = MarkdownFileID(url: url) {
            showFile(file, in: id)
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    /// With `focus`, a newly selected tool takes keyboard focus once it
    /// mounts; the selected one is already mounted.
    func select(_ tool: ConversationTool, in id: ConversationID, focus: Bool = false) {
        let state = state(for: id)
        guard state.tools.contains(tool) else { return }
        focusRequest = focus && state.selected != tool ? (id, tool) : nil
        states[id]?.selected = tool
    }

    /// Show Next/Previous Tab: selects the tab `offset` away, wrapping at the
    /// ends; with `focus`, it takes keyboard focus once it mounts.
    func selectAdjacent(_ offset: Int, in id: ConversationID, focus: Bool) {
        let state = state(for: id)
        let count = state.tools.count
        guard count > 1, let selected = state.selected, let index = state.tools.firstIndex(of: selected) else { return }
        let tool = state.tools[((index + offset) % count + count) % count]
        states[id]?.selected = tool
        focusRequest = focus ? (id, tool) : nil
    }

    /// Removes `tool`, selecting its neighbor. Closing the last tab leaves the
    /// pane showing the tool picker.
    @discardableResult
    func close(_ tool: ConversationTool, in id: ConversationID) -> Bool {
        guard var state = states[id], let index = state.tools.firstIndex(of: tool) else { return false }
        state.tools.remove(at: index)
        if state.selected == tool {
            state.selected = state.tools.indices.contains(index) ? state.tools[index] : state.tools.last
        }
        states[id] = state
        focusRequest = nil
        if case .file = tool { releaseUnusedDocuments() }
        return true
    }

    /// Show/Hide Tools.
    func toggleVisibility(for id: ConversationID) {
        states[id, default: ConversationToolsState()].isShown.toggle()
        focusRequest = nil
    }

    /// Whether `tool`, mounting in `id`'s pane, should take keyboard focus.
    /// Answers yes once.
    func consumeFocus(for tool: ConversationTool, in id: ConversationID) -> Bool {
        guard let focusRequest, focusRequest.conversation == id, focusRequest.tool == tool else { return false }
        self.focusRequest = nil
        return true
    }

    func cancelFocus() {
        focusRequest = nil
    }

    /// Closes terminal tabs whose terminal is gone, as after a Core replacement.
    func pruneTerminals(keeping terminals: Set<TerminalID>) {
        for (id, state) in states {
            for case .terminal(let terminalID) in state.tools where !terminals.contains(terminalID) {
                close(.terminal(terminalID), in: id)
            }
        }
    }

    /// Forgets a deleted conversation's tools, returning them to close.
    func remove(_ id: ConversationID) -> [ConversationTool] {
        if focusRequest?.conversation == id { focusRequest = nil }
        let tools = states.removeValue(forKey: id)?.tools ?? []
        releaseUnusedDocuments()
        return tools
    }

    // MARK: - File documents

    /// One document per open file, shared by every conversation showing it.
    /// A cache, not state, like `reviewModels`.
    func document(for file: MarkdownFileID) -> MarkdownFileDocument {
        if let document = documents[file] { return document }
        let document = MarkdownFileDocument(url: file.url)
        documents[file] = document
        return document
    }

    /// Where the reader was in `id`'s tab for `file`. Each tab keeps its own
    /// place, though tabs for one file share its document.
    func readingPosition(for file: MarkdownFileID, in id: ConversationID) -> ReadingPosition {
        let tab = FileTab(conversation: id, file: file)
        if let position = readingPositions[tab] { return position }
        let position = ReadingPosition()
        readingPositions[tab] = position
        return position
    }

    /// Stops watching every file, as the window closes.
    func releaseDocuments() {
        for document in documents.values { document.stop() }
        documents.removeAll()
        readingPositions.removeAll()
    }

    /// Stops watching files no conversation has a tab for, and forgets
    /// closed tabs' places.
    private func releaseUnusedDocuments() {
        let tabs = Set(states.flatMap { id, state in
            state.tools.compactMap { if case .file(let file) = $0 { FileTab(conversation: id, file: file) } else { nil } }
        })
        readingPositions = readingPositions.filter { tabs.contains($0.key) }
        let open = Set(tabs.map(\.file))
        for (file, document) in documents where !open.contains(file) {
            document.stop()
            documents[file] = nil
        }
    }

    // MARK: - Review models

    /// One review model per project, shared by every conversation in that
    /// project.
    func reviewModel(for project: ResolvedProject) -> DiffReviewModel {
        if let model = reviewModels[project.projectSourceID] { return model }
        let model = makeReviewModel(project)
        model.configure(projectSummary: project)
        reviewModels[project.projectSourceID] = model
        return model
    }

    /// Evicts models for projects that left the workspace.
    func retainReviewModels(for sources: Set<ProjectSourceID>) {
        reviewModels = reviewModels.filter { sources.contains($0.key) }
    }

    /// Reloads every cached model, e.g. after a Core replacement.
    func refreshReviewModels() {
        for model in reviewModels.values {
            model.refresh()
        }
    }
}
