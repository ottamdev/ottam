import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamWorkspaceLogic
import OSLog
import SwiftUI

private let sidebarNavigationLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "WorkspaceNavigation"
)

/// The merged workspace sidebar: an identity block naming the Workspace, the New
/// Chat row, then the conversation history.
///
/// History shape follows the Workspace: one flat unsectioned list where there is
/// a single home (0 or 1 project), and collapsible per-project sections plus a
/// trailing "Chats" bucket once there are two or more. Selection unifies New
/// Chat and conversations via `SidebarSelection`; the system sidebar material is
/// left untouched.
struct SidebarView: View {
    /// Restored with the window; see `WorkspaceRestorationState`.
    @Binding var collapsedSectionsRaw: String
    @Bindable var workspace: WorkspaceStore
    @Bindable var chatWorkspace: ChatWorkspaceModel
    @Bindable var backend: DiffReviewModel
    let addProject: () -> Void

    /// Sections whose "Show more" fold has been opened. Deliberately not
    /// persisted — a fresh window starts capped.
    @State private var uncappedSections: Set<SidebarSectionKind> = []
    @State private var deletionTarget: ConversationSummary?
    /// Parents whose delegated children are folded away. Not persisted: a
    /// fresh window shows every child.
    @State private var foldedParents: Set<ConversationID> = []
    /// Parents whose children past the cap have been shown. Not persisted,
    /// like the sections' fold.
    @State private var uncappedParents: Set<ConversationID> = []

    var body: some View {
        List(selection: selection) {
            Section {
                Label("New Chat", systemImage: "square.and.pencil")
                    .accessibilityIdentifier(AccessibilityID.routeChat)
                    .accessibilityLabel("New Chat")
                    .tag(SidebarSelection.newChat)
            }

            ForEach(sections) { section in
                if section.kind == .all {
                    // Collapsed shape: one list, labelled by kind rather than by
                    // project. A project-named header would repeat the identity
                    // block below, but an unlabelled section contributes only
                    // whitespace, which reads as the route rows continuing rather
                    // than as a second group.
                    //
                    // No `.sectionActions` — with a single implied root the
                    // top-level New Chat row already does what the per-section
                    // affordance did.
                    Section(isExpanded: isExpandedBinding(section.kind)) {
                        rows(in: section)
                    } header: {
                        sectionHeader(section)
                    }
                } else {
                    Section(isExpanded: isExpandedBinding(section.kind)) {
                        rows(in: section)
                    } header: {
                        sectionHeader(section)
                    }
                    // System-owned hover affordance next to the collapse chevron
                    // (macOS shows section actions on header hover).
                    .sectionActions {
                        Button("New Chat in \(section.title)", systemImage: "plus.bubble") {
                            beginNewChat(in: section.kind)
                        }
                        .imageScale(.large)
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .onKeyPress(keys: [.leftArrow, .rightArrow]) { press in
            foldFromKeyboard(press.key)
        }
        .accessibilityIdentifier(AccessibilityID.workspaceSidebar)
        // Mounted as a bar rather than a row so it cannot enter
        // `SidebarSelection`, take row metrics, or pick up the selection pill
        // that would make it read as a fourth navigation item. `safeAreaBar`
        // rather than `safeAreaInset` so it participates in the scroll edge
        // effect — see HeaderScrollEdge.swift.
        //
        // At the bottom rather than the top: the sidebar's titlebar already owns
        // the top edge, so a bar there is a second participant a few points from
        // the first, and rows scrolling between them read through anything
        // translucent. The bottom edge has no competing system chrome, which is
        // why a sidebar footer is the shape Finder, Mail and Xcode all use.
        //
        // `.hard`, not the house-default `.soft`. `.soft` is a progressive frost
        // — it fades content approaching the edge, by design only partially — so
        // rows stayed legible inside the band. `.hard` puts a full material
        // across it instead. The other way to settle this is an opaque fill
        // under the bar, the way the composer does it (ChatWorkspaceView.swift);
        // that was tried here and reads as a flat patch in a column that is
        // otherwise vibrant over the desktop.
        .scrollEdgeEffectStyle(.hard, for: .bottom)
        .safeAreaBar(edge: .bottom, spacing: 0) {
            WorkspaceIdentityBlock(
                name: workspace.workspaceSummary?.displayName ?? "Workspace",
                detail: identityDetail,
                projects: workspace.projects,
                workspaceFiles: workspace.workspaceFiles,
                workspaceID: workspace.workspaceSummary?.workspaceID,
                addProject: addProject
            )
            // macOS 27 draws the `.hard` edge's own boundary; 26 draws none, so
            // the bar needs a hairline there. Overlaid rather than stacked so it
            // takes no height from the bar and the name doesn't shift.
            .overlay(alignment: .top) {
                if #unavailable(macOS 27) { Divider() }
            }
        }
        .alert(
            "Delete Conversation?",
            isPresented: deleteConfirmationPresented,
            presenting: deletionTarget
        ) { conversation in
            if delegatedChildren(of: conversation).isEmpty {
                Button("Delete", role: .destructive) {
                    Task {
                        await chatWorkspace.delete(conversation, backend: backend)
                    }
                }
            } else {
                Button("Delete All", role: .destructive) {
                    Task {
                        await chatWorkspace.delete(conversation, children: .delete, backend: backend)
                    }
                }
                Button(delegatedChildren(of: conversation).count == 1
                    ? "Keep Delegated Conversation" : "Keep Delegated Conversations") {
                    Task {
                        await chatWorkspace.delete(conversation, children: .keep, backend: backend)
                    }
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: { conversation in
            Text(deleteMessage(for: conversation))
        }
    }

    // MARK: - Sections

    /// "History" and "Chats" are the two section titles that are stable across
    /// Workspaces — the only headers a scenario can name directly, since every
    /// project section's title is the project's own display name.
    @ViewBuilder
    private func sectionHeader(_ section: SidebarSection) -> some View {
        let text = Text(section.title)
            .font(.chrome(.body, weight: .semibold))
            .badge(collapsedAttentionBadge(for: section))
        switch section.kind {
        case .all:
            text.accessibilityIdentifier(AccessibilityID.sidebarHistorySection)
        case .looseChats:
            text.accessibilityIdentifier(AccessibilityID.sidebarChatsSection)
        case .project:
            text
        }
    }

    @ViewBuilder
    private func rows(in section: SidebarSection) -> some View {
        // Children are rows of their own under their parent rather than a
        // DisclosureGroup: the outline's leading triangle pushed parents'
        // titles out of line with every other row. The parent's toggle
        // follows its title instead, and the list's arrow keys fold it
        // (`foldFromKeyboard`).
        ForEach(visibleItems(in: section)) { item in
            if item.children.isEmpty {
                row(item)
            } else {
                let unfolded = unfoldedBinding(item)
                row(item, nesting: .parent(unfolded: unfolded))
                if unfolded.wrappedValue {
                    let children = visibleChildren(of: item)
                    ForEach(children) { child in
                        row(child, nesting: .child(parentTitle: item.summary.title))
                    }
                    if children.count < item.children.count {
                        Button("Show \(item.children.count - children.count) More…") {
                            uncappedParents.insert(item.id)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(
                            "Show \(item.children.count - children.count) more delegated conversations of \(item.summary.title)"
                        )
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .padding(.leading, SidebarConversationRow.titleInset + SidebarConversationRow.childIndent)
                    }
                }
            }
        }
        if let hidden = hiddenCount(in: section) {
            Button("Show \(hidden) More…") {
                uncappedSections.insert(section.kind)
            }
            .buttonStyle(.plain)
            .font(.callout)
            .foregroundStyle(.secondary)
            .padding(.leading, SidebarConversationRow.titleInset)
        }
    }

    private func row(
        _ item: SidebarConversationItem,
        nesting: SidebarConversationRow.Nesting = .none
    ) -> some View {
        SidebarConversationRow(
            item: item,
            nesting: nesting,
            deleteDisabled: chatWorkspace.isMutating,
            deleteAction: { deletionTarget = item.summary }
        )
        .tag(SidebarSelection.conversation(item.id))
    }

    /// A parent's first few children, plus any past them that are selected
    /// or have something to say, the way a section's cap keeps them.
    private func visibleChildren(of item: SidebarConversationItem) -> [SidebarConversationItem] {
        guard !uncappedParents.contains(item.id) else { return item.children }
        let selected = chatWorkspace.selectedConversationID
        return item.children.enumerated().compactMap { index, child in
            index < Self.childCap || child.staysPastCap(selected: selected) ? child : nil
        }
    }

    /// Children shown under a parent before "Show N More…".
    private static let childCap = 3

    private func unfoldedBinding(_ parent: SidebarConversationItem) -> Binding<Bool> {
        Binding(
            get: { !foldedParents.contains(parent.id) },
            set: { unfolded in
                if unfolded {
                    foldedParents.remove(parent.id)
                } else {
                    foldedParents.insert(parent.id)
                    // As the system outline does: folding away the selected
                    // child selects its parent, so the selection stays in sight.
                    if parent.children.contains(where: { $0.id == chatWorkspace.selectedConversationID }) {
                        selection.wrappedValue = .conversation(parent.id)
                    }
                }
            }
        )
    }

    /// What the outline's arrow keys did, kept for these rows: Left folds the
    /// selected parent, or moves from a child to its parent; Right unfolds.
    private func foldFromKeyboard(_ key: KeyEquivalent) -> KeyPress.Result {
        let items = sections.flatMap(\.allItems)
        guard let move = SidebarKeyboardFold.move(
            key == .rightArrow ? .right : .left,
            selected: chatWorkspace.selectedConversationID,
            items: items,
            folded: foldedParents
        ) else { return .ignored }
        switch move {
        case .fold(let id), .unfold(let id):
            guard let parent = items.first(where: { $0.id == id }) else { return .ignored }
            unfoldedBinding(parent).wrappedValue = move == .unfold(id)
        case .selectParent(let id):
            selection.wrappedValue = .conversation(id)
        }
        return .handled
    }

    private var sections: [SidebarSection] {
        SidebarSectionBuilder.sections(
            projects: workspace.projects,
            selectedProject: workspace.projectSummary,
            conversations: chatWorkspace.conversations,
            attentionConversationIDs: chatWorkspace.attentionConversationIDs,
            runningConversationIDs: chatWorkspace.runningConversationIDs
        )
    }

    /// The identity block's tooltip and accessibility value, which adapts to the
    /// shape for the same reason a single path would be wrong above a
    /// multi-project Workspace.
    private var identityDetail: String {
        workspaceFolderSummary(projectPaths: workspace.projects.map(\.source.canonicalPath))
    }

    private func visibleItems(in section: SidebarSection) -> [SidebarConversationItem] {
        guard !uncappedSections.contains(section.kind) else { return section.allItems }
        let cap = SidebarSectionBuilder.visibleCap(for: section.kind)
        let selected = chatWorkspace.selectedConversationID
        // A parent whose child is selected, waiting or working stays too, or
        // that child would vanish with it.
        return section.allItems.enumerated().compactMap { index, item in
            index < cap || item.staysPastCap(selected: selected) ? item : nil
        }
    }

    private func hiddenCount(in section: SidebarSection) -> Int? {
        guard !uncappedSections.contains(section.kind) else { return nil }
        let hidden = section.allItems.count - visibleItems(in: section).count
        return hidden > 0 ? hidden : nil
    }

    /// Pending-approval count surfaced on the header only while the section is
    /// collapsed; expanded sections show the per-row glyphs instead.
    private func collapsedAttentionBadge(for section: SidebarSection) -> Int {
        guard !isExpandedBinding(section.kind).wrappedValue else { return 0 }
        return section.allItems.reduce(0) { $0 + $1.attentionCount }
    }

    /// Starts a new conversation whose draft working root is the section's home:
    /// the project for project sections, workspace files for the Chats bucket,
    /// and the implied root in the collapsed shape.
    private func beginNewChat(in kind: SidebarSectionKind) {
        switch kind {
        case .all:
            chatWorkspace.draftWorkingRoot = impliedWorkingRoot(
                projects: workspace.projects,
                selectedProject: workspace.projectSummary
            )
        case .project(let projectID):
            if let project = workspace.projects.first(where: {
                $0.project.projectID == projectID
            }) {
                chatWorkspace.draftWorkingRoot = .project(
                    projectID: project.project.projectID,
                    sourceID: project.source.projectSourceID
                )
            }
        case .looseChats:
            chatWorkspace.draftWorkingRoot = .workspaceFiles
        }
        chatWorkspace.beginNewConversation(backend: backend)
    }

    // MARK: - Collapse persistence

    private var collapsedSet: Set<String> {
        Set(collapsedSectionsRaw.split(separator: ",").map(String.init))
    }

    private func isExpandedBinding(_ kind: SidebarSectionKind) -> Binding<Bool> {
        Binding {
            !collapsedSet.contains(kind.persistenceKey)
        } set: { expanded in
            var set = collapsedSet
            if expanded {
                set.remove(kind.persistenceKey)
            } else {
                set.insert(kind.persistenceKey)
            }
            collapsedSectionsRaw = set.sorted().joined(separator: ",")
        }
    }

    // MARK: - Selection

    /// Derived from `selectedConversationID`; writes map back onto it.
    private var selection: Binding<SidebarSelection?> {
        Binding {
            chatWorkspace.selectedConversationID.map(SidebarSelection.conversation) ?? .newChat
        } set: { newValue in
            switch newValue ?? .newChat {
            case .newChat:
                sidebarNavigationLogger.info("selected sidebar New Chat")
                chatWorkspace.beginNewConversation(backend: backend)
            case .conversation(let id):
                sidebarNavigationLogger.info(
                    "selected sidebar conversation id=\(id.rawValue, privacy: .public)"
                )
                chatWorkspace.select(id, backend: backend)
            }
        }
    }

    private func delegatedChildren(of conversation: ConversationSummary) -> [ConversationSummary] {
        chatWorkspace.conversations.filter { $0.parentConversationID == conversation.conversationID }
    }

    /// The alert's text. A parent's names what it delegated to, and what
    /// keeping them does.
    private func deleteMessage(for conversation: ConversationSummary) -> String {
        let removes = "This removes the conversation and its local history. "
            + "Associated harness sessions will also be removed where supported."
        let children = delegatedChildren(of: conversation)
        guard !children.isEmpty else { return removes }
        let names = children.map { $0.delegatedAgentName ?? $0.taskTitle }
        let delegated = children.count == 1
            ? "It delegated a task to \(names[0]). Delete that conversation too, or keep it: "
                + "a kept conversation stops working and stands on its own in the sidebar."
            : "It delegated \(children.count) tasks, to \(ListFormatter.localizedString(byJoining: Array(Set(names)).sorted())). "
                + "Delete those conversations too, or keep them: kept conversations stop working and stand on their own in the sidebar."
        return removes + "\n\n" + delegated
    }

    private var deleteConfirmationPresented: Binding<Bool> {
        Binding {
            deletionTarget != nil
        } set: { presented in
            if !presented {
                deletionTarget = nil
            }
        }
    }
}

// MARK: - Rows

/// A single-line row: the title, then marks only when there is something to
/// say. Colors carry meaning only — orange = approval required, accent dot =
/// agent running; everything else inherits the system sidebar treatment
/// (accent-tinted selection).
private struct SidebarConversationRow: View {
    /// Where a row sits among delegated conversations.
    enum Nesting {
        case none
        /// A parent, with its children shown under it or folded away.
        case parent(unfolded: Binding<Bool>)
        /// A delegated child, named by its task: the parent above says where
        /// it came from, and the agent's name would repeat down every row.
        /// VoiceOver, which has no indent to go by, hears the parent's title.
        case child(parentTitle: String)

        var isChild: Bool {
            if case .child = self { true } else { false }
        }
    }

    let item: SidebarConversationItem
    var nesting: Nesting = .none
    let deleteDisabled: Bool
    let deleteAction: () -> Void

    /// How far a title sits in from the row's content edge. The sidebar
    /// puts that edge where a Label's icon would start, 6pt inside the
    /// selection, so an icon-less title hugged the selection's edge; this
    /// brings it to about where the New Chat row's icon starts.
    static let titleInset: CGFloat = 6
    /// How far a child's title sits in from its parent's: indentation alone
    /// carries the nesting, as in the system outline, with no tree line.
    static let childIndent: CGFloat = 14

    private var unfolded: Binding<Bool>? {
        if case .parent(let unfolded) = nesting { unfolded } else { nil }
    }

    private var title: String {
        if case .child = nesting { item.summary.taskTitle } else { item.summary.title }
    }

    /// A folded parent wears its children's marks: their rows are out of
    /// sight, and an approval they wait on must not be.
    private var foldedChildNeedsAttention: Bool {
        unfolded?.wrappedValue == false && item.childNeedsAttention
    }

    private var foldedChildIsRunning: Bool {
        unfolded?.wrappedValue == false && item.childIsRunning
    }

    /// Rows carry no icon: every row being a chat, a glyph repeated down
    /// the list said nothing. Marks trail the title only when there is
    /// something to say.
    var body: some View {
        // A `Label`, though there is no icon: the sidebar sets a selected
        // Label's title in bold, as Mail and Notes do, and a bare stack
        // gets no such treatment. The empty icon takes no width.
        Label {
            HStack(spacing: 6) {
                Text(title)
                    .lineLimit(1)
                    .truncationMode(.tail)
                if let unfolded {
                    Button {
                        withAnimation(.snappy(duration: 0.2)) { unfolded.wrappedValue.toggle() }
                    } label: {
                        // The glyph swaps at once; only the rows animate.
                        Image(systemName: unfolded.wrappedValue ? "chevron.down" : "chevron.right")
                            .font(.system(size: 9, weight: .semibold))
                            .foregroundStyle(.secondary)
                            .contentTransition(.identity)
                            .frame(width: 14, height: 14)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help(unfolded.wrappedValue ? "Hide delegated conversations" : "Show delegated conversations")
                    // The row's own action carries it for VoiceOver.
                    .accessibilityHidden(true)
                }
                if item.rootMark != nil || item.needsAttention || item.isRunning
                    || foldedChildNeedsAttention || foldedChildIsRunning
                {
                    Spacer(minLength: 0)
                }
                // Visible without hover, and announced: a chat must never
                // silently claim a root it does not have. A glyph rather than
                // the root's name, which repeated down a whole bucket read as
                // noise and stole width from the titles it was meant to
                // qualify. One symbol per fact, not one for both — the tooltip
                // and the accessibility label carry the wording.
                if let mark = item.rootMark {
                    Image(systemName: symbolName(for: mark))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                }
                statusMark
            }
        } icon: {
            EmptyView()
        }
        .padding(.leading, Self.titleInset + (nesting.isChild ? Self.childIndent : 0))
        .help(tooltip)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityDescription)
        .accessibilityActions {
            if let unfolded {
                Button(unfolded.wrappedValue ? "Hide delegated conversations" : "Show delegated conversations") {
                    unfolded.wrappedValue.toggle()
                }
            }
        }
        .contextMenu {
            Button("Copy Title") { WorkspacePasteboard.copy(title) }
            // The core's handle for `conversation.*` inspect calls when debugging.
            Button("Copy Conversation ID") {
                WorkspacePasteboard.copy(item.summary.conversationID.rawValue)
            }
            Divider()
            Button("Delete Conversation…", role: .destructive, action: deleteAction)
                .disabled(deleteDisabled)
        }
    }

    /// Waiting on the user, or working, itself or through a folded child;
    /// nothing when idle.
    @ViewBuilder
    private var statusMark: some View {
        if item.needsAttention || foldedChildNeedsAttention {
            Image(systemName: "exclamationmark.circle.fill")
                .font(.caption)
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
        } else if item.isRunning || foldedChildIsRunning {
            Image(systemName: "circle.fill")
                .font(.system(size: 7))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
        }
    }

    /// The glyph the mark renders as. Two symbols, because the two marks are two
    /// facts — but one family, so they read as two answers to the same question
    /// rather than as unrelated pictograms.
    ///
    /// A plain folder for the ordinary case: Workspace files really is one
    /// directory, so the glyph asserts nothing false, and it stays quiet because
    /// rooting a chat there is normal rather than a problem. `questionmark.folder`
    /// over `folder.badge.questionmark` on legibility — at 11pt the corner badge
    /// muddies into the folder outline, while the full-body question mark is
    /// still distinguishable from a plain folder at a glance. Neither an
    /// `exclamationmark.triangle` nor a `.badge.minus`: a project leaving the
    /// Workspace is a loose end, not an error or a deletion.
    private func symbolName(for mark: SidebarRootMark) -> String {
        switch mark {
        case .workspaceFiles: "folder"
        case .detachedProject: "questionmark.folder"
        }
    }

    /// What the glyph means, in words. This is the whole reason a symbol is
    /// affordable — it says nothing on its own, so the hover has to say it.
    private func rootExplanation(for mark: SidebarRootMark) -> String {
        switch mark {
        case .workspaceFiles:
            "Rooted at Workspace files, not a project"
        case .detachedProject(let name):
            "Rooted at \(name), which is no longer in this Workspace"
        }
    }

    private var tooltip: String {
        var parts: [String] = []
        if case .child = nesting, let agent = item.summary.delegatedAgentName { parts.append(agent) }
        if item.needsAttention { parts.append("Approval required") }
        if item.isRunning { parts.append("Agent running") }
        if foldedChildNeedsAttention { parts.append("A delegated conversation needs approval") }
        if foldedChildIsRunning { parts.append("Delegated agent running") }
        if let mark = item.rootMark { parts.append(rootExplanation(for: mark)) }
        if let relative = chatRelativeTime(item.summary.updatedAt) {
            parts.append("Updated \(relative)")
        }
        return parts.joined(separator: " · ")
    }

    private var accessibilityDescription: String {
        var parts = [title]
        // A child's row reads as part of its parent, as its indent shows.
        if case .child(let parentTitle) = nesting {
            parts.append(
                [item.summary.delegatedAgentName, "delegated from \(parentTitle)"].compactMap { $0 }.joined(separator: ", ")
            )
        }
        if item.needsAttention { parts.append("approval required") }
        if item.isRunning { parts.append("agent running") }
        if foldedChildNeedsAttention { parts.append("a delegated conversation needs approval") }
        if foldedChildIsRunning { parts.append("delegated agent running") }
        if let unfolded {
            let count = item.children.count
            parts.append("\(count) delegated conversation\(count == 1 ? "" : "s"), \(unfolded.wrappedValue ? "expanded" : "collapsed")")
        }
        // The mark is useless to VoiceOver if it lives only in the tooltip, and
        // more so now that the visible form is a wordless glyph.
        if let mark = item.rootMark {
            parts.append(rootExplanation(for: mark).lowercasedFirst)
        }
        return parts.joined(separator: ", ")
    }
}

extension String {
    /// Lowercases only the first character, so a sentence-case string can be
    /// dropped mid-phrase without mangling names inside it.
    fileprivate var lowercasedFirst: String {
        guard let first else { return self }
        return first.lowercased() + dropFirst()
    }
}

// MARK: - Identity

/// The tint a Workspace monogram can take: a 5×5 grid, five hue families
/// across and five tones down.
///
/// Generated from HSB rather than written out as 25 literals, so the grid is
/// even by construction — a hand-picked list of this size drifts, and one
/// swatch a shade off is visible when they sit in a row together.
///
/// A closed set rather than `ColorPicker`: 25 presets cover the range people
/// reach for, and every one is a fill `monogramGlyph` is known to keep the
/// letter legible on.
struct WorkspaceMonogramTint: Identifiable, Hashable, Sendable {
    /// `row-column`. Positional rather than named because the names would be
    /// invented — "Deep Teal" is not a thing anyone types — and because it keeps
    /// the stored value stable if a hue is ever re-tuned in place. Retuning is
    /// the only migration this can need: change the grid's *shape* and old picks
    /// resolve to the name-derived tint, which is the right amount of ceremony
    /// for a swatch preference.
    let id: String
    let label: String
    let color: Color

    private static let hues: [(name: String, hue: Double)] = [
        ("Red", 0.99),
        ("Orange", 0.08),
        ("Green", 0.35),
        ("Blue", 0.58),
        ("Purple", 0.78),
    ]

    /// Saturation climbs as brightness falls, which is what keeps the dark end
    /// from going muddy and the light end from going chalky.
    private static let tones: [(name: String, saturation: Double, brightness: Double)] = [
        ("Pale", 0.26, 1.00),
        ("Light", 0.46, 0.96),
        ("", 0.72, 0.88),
        ("Deep", 0.85, 0.68),
        ("Dark", 0.90, 0.46),
    ]

    static let columnCount = hues.count

    static let all: [WorkspaceMonogramTint] = tones.enumerated().flatMap { row, tone in
        hues.enumerated().map { column, hue in
            WorkspaceMonogramTint(
                id: "\(row)-\(column)",
                label: tone.name.isEmpty ? hue.name : "\(tone.name) \(hue.name)",
                color: Color(
                    hue: hue.hue,
                    saturation: tone.saturation,
                    brightness: tone.brightness
                )
            )
        }
    }

    static func stored(_ id: String) -> WorkspaceMonogramTint? {
        all.first { $0.id == id }
    }

    /// Where a Workspace's picked tint lives in user defaults.
    static func storageKey(for workspaceID: WorkspaceID?) -> String {
        "workspace.monogramTint.\(workspaceID?.rawValue ?? "unassigned")"
    }

    /// The tint a Workspace shows until somebody picks one.
    ///
    /// Derived from the name rather than stored, so an unchosen disc needs no
    /// storage and changes on rename — the honest behaviour, since until it is
    /// chosen the disc identifies the name, not the Workspace.
    ///
    /// FNV-1a rather than `hashValue`. Swift seeds `Hasher` randomly per
    /// process, so a stored-property hash would repaint the disc on every
    /// launch.
    static func derived(fromName name: String) -> Self {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in name.lowercased().utf8 {
            hash ^= UInt64(byte)
            hash &*= 0x100_0000_01b3
        }
        return all[Int(hash % UInt64(all.count))]
    }
}

extension Color {
    /// Black or white, whichever the monogram glyph reads better in: a white
    /// glyph vanishes on the pale row. WCAG relative luminance, comparing the
    /// contrast ratio each choice would produce and taking the larger.
    ///
    /// sRGB explicitly — `NSColor(someSwiftUIColor)` can land in a device or
    /// catalog space, where the components mean something else.
    var monogramGlyph: Color {
        guard let srgb = NSColor(self).usingColorSpace(.sRGB) else { return .white }
        func linear(_ channel: CGFloat) -> Double {
            let channel = Double(max(0, min(1, channel)))
            return channel <= 0.04045
                ? channel / 12.92
                : pow((channel + 0.055) / 1.055, 2.4)
        }
        let luminance = 0.2126 * linear(srgb.redComponent)
            + 0.7152 * linear(srgb.greenComponent)
            + 0.0722 * linear(srgb.blueComponent)
        let againstWhite = 1.05 / (luminance + 0.05)
        let againstBlack = (luminance + 0.05) / 0.05
        return againstWhite >= againstBlack ? .white : .black
    }
}

/// The Workspace's first letter on a tinted disc, as a second cue beside the
/// name. The tint is the one picked from the palette, or derived from the name
/// until somebody picks.
struct WorkspaceMonogram: View {
    let name: String
    let color: Color
    /// 20pt in the identity bar; the chooser's two-line rows take a larger one.
    var diameter: CGFloat = 20
    var glyph: AppTypography.Scale = .label

    /// First letter of the name, uppercased. Grapheme-cluster based so an emoji
    /// or an accented character survives as one glyph.
    private var initial: String {
        guard let first = name.first(where: { !$0.isWhitespace }) else { return "?" }
        return String(first).uppercased()
    }

    var body: some View {
        Text(initial)
            .font(.chrome(glyph, weight: .bold))
            // Not hardcoded white: the pale tints swallow a white glyph, so the
            // glyph follows the fill.
            .foregroundStyle(color.monogramGlyph)
            .frame(width: diameter, height: diameter)
            // Opaque fill first, glass layered over it — the order is the whole
            // trick. `.glassEffect(.regular.tint(color))` in place of the fill
            // renders indistinguishably from a flat disc at this size: the bar
            // sits on the `.hard` edge material, so there is nothing behind the
            // disc to lens and the material has no signal to build a rim from.
            // Giving the glass an opaque disc of its own to sit on is what
            // produces the specular edge. Measured on real pixels, both ways.
            //
            // Keeping the fill opaque also keeps `monogramGlyph` honest: it
            // picks black-or-white from the nominal tint, which a translucent
            // fill would have quietly invalidated.
            .background(color, in: .circle)
            // Explicit shape: `glassEffect()` defaults to a capsule, which is a
            // circle only for as long as this frame stays square.
            .glassEffect(in: .circle)
            .accessibilityHidden(true)
    }
}

/// States which Workspace this window is: the monogram disc and the name, each
/// opening its own popover (the palette, and the paths plus Add Project).
///
/// One line, not three: the block's position at the foot of the list already
/// says it is the Workspace, so a "Workspace" caption spent a line on a word
/// nobody reads twice, and the paths belong behind the chevron where they are
/// also actionable.
///
/// Deliberately not a Workspace *switcher*: one Workspace per native window means
/// switching is activating another window, which macOS already provides through
/// the Window menu, Mission Control, and ⌘`.
private struct WorkspaceIdentityBlock: View {
    let name: String
    /// Shape-adaptive summary — a path, `No projects`, or `N projects`. No longer
    /// rendered inline; kept as the tooltip and the accessibility value so the
    /// fact survives for anyone who never opens the menu.
    let detail: String
    let projects: [WorkspaceProjectSummary]
    let workspaceFiles: WorkingRootSummary?
    /// Keys the stored tint. `nil` until the Workspace summary arrives, which is
    /// why the colour section only mounts once there is something stable to key.
    let workspaceID: WorkspaceID?
    let addProject: () -> Void

    /// App-local rather than a field on `WorkspaceDetail`: the Workspace model is
    /// generated from the Rust protocol, and a swatch preference does not earn a
    /// schema change plus a migration. The cost is that the choice stays on this
    /// machine — revisit if Workspaces ever sync.
    ///
    /// Empty means unchosen, which falls back to the name-derived tint rather
    /// than baking a default into storage.
    @AppStorage private var storedTint: String

    @State private var isPickingColor = false
    @State private var hoveredTint: WorkspaceMonogramTint?
    @State private var isShowingIdentityMenu = false
    /// Keyed by row label, which is unique within this popover — the paths are
    /// distinct and `Add Project…` is not a path.
    @State private var hoveredRow: String?

    init(
        name: String,
        detail: String,
        projects: [WorkspaceProjectSummary],
        workspaceFiles: WorkingRootSummary?,
        workspaceID: WorkspaceID?,
        addProject: @escaping () -> Void
    ) {
        self.name = name
        self.detail = detail
        self.projects = projects
        self.workspaceFiles = workspaceFiles
        self.workspaceID = workspaceID
        self.addProject = addProject
        _storedTint = AppStorage(
            wrappedValue: "",
            WorkspaceMonogramTint.storageKey(for: workspaceID)
        )
    }

    /// Empty storage means unchosen, so the name-derived tint stands in. An
    /// unrecognised value resolves the same way rather than erroring, which is
    /// all the handling a preference this small needs.
    private var selectedSwatch: WorkspaceMonogramTint {
        WorkspaceMonogramTint.stored(storedTint) ?? .derived(fromName: name)
    }

    var body: some View {
        HStack(spacing: 6) {
            // Outside the `Menu`, not inside its label: a macOS menu label is
            // AppKit-backed and accepts text plus a template image only — a
            // filled `Shape` is dropped entirely and the label flattens to the
            // first `Text` it finds. Sitting here it renders, and now carries its
            // own control rather than being inert decoration beside the menu.
            monogramButton

            // A `Button` presenting a popover rather than a `Menu`, to match the
            // palette hanging off the disc beside it — two controls a few points
            // apart opening two different kinds of surface read as an accident.
            //
            // The cost is real and worth stating: an `NSMenu` brings keyboard
            // navigation, type-select and the system highlight for free, and a
            // popover of buttons brings none of it. Acceptable for two or three
            // reveal rows; if this ever grows into a menu proper, that trade
            // should be revisited rather than reimplemented.
            Button {
                isShowingIdentityMenu = true
            } label: {
                HStack(spacing: 3) {
                    Text(name)
                        .font(.chrome(.headline, weight: .medium))
                    // Drawn by hand now that `.borderlessButton` is not supplying
                    // one, at the weight that keeps it from reading heavier than
                    // the name.
                    //
                    // Up, not the down a pull-down button conventionally shows.
                    // That convention holds because a menu's direction is not
                    // knowable in advance; here it is — the bar is pinned to the
                    // sidebar's bottom edge, so the popover always rises. A
                    // chevron pointing away from the thing it opens is the part
                    // that reads wrong.
                    Image(systemName: "chevron.up")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.secondary)
                }
                .contentShape(.rect)
            }
            // No button chrome. The bar already separates itself from the list
            // with a divider and the `.hard` edge material, so a capsule around
            // the name was a second treatment doing the same job — and a
            // translucent one, which is what let rows read through it. Bare text
            // plus a chevron is the shape Finder's path bar and Xcode's jump bar
            // use in this position.
            .buttonStyle(.plain)
            // Neither control carries button chrome, so the pointer is the only
            // thing that says they are controls at all before you click. Strict
            // HIG reserves this for links; `.plain` styling is what makes it
            // earn its keep here.
            .pointerStyle(.link)
            .help(detail)
            .accessibilityIdentifier(AccessibilityID.sidebarIdentityBlock)
            .accessibilityLabel("Workspace \(name)")
            .accessibilityValue(detail)
            .popover(isPresented: $isShowingIdentityMenu, arrowEdge: .top) {
                identityMenu
            }

            Spacer(minLength: 0)
        }
        // Sitting outside the List means owning this inset by hand: no API
        // reports the sidebar's effective row inset (`listSectionMargins` is
        // unavailable on macOS, and `listRowInsets` only sets one), so this is
        // measured from a real capture rather than derived.
        //
        // Target is the row *icon* column at 30pt from the sidebar's content
        // edge — the list's true left edge, which the bar hangs on whether it
        // sits above or below; the label column at 54.5pt would read as
        // The disc hangs on the window's leading edge rather than on the list's
        // row grid: 11pt here puts it at the same absolute x as the traffic
        // lights at the top of the same window. Deliberately not the row *icon*
        // column, which would need 23 — the bar is chrome bracketing the window,
        // and reads better squared against the other chrome than indented to
        // match rows it is not part of. Note the bar's own origin sits 8pt
        // inside the window border, so this constant is not the visual inset.
        .padding(.leading, 10)
        .padding(.trailing, 12)
        // Even, measured from the divider above to the window's content edge
        // below. Weighting it toward the list was tried and reads as a misplaced
        // line rather than as emphasis.
        .padding(.vertical, 10)
    }

    /// The disc, as the affordance for its own colour. It reads as the thing the
    /// colour belongs to, so clicking it is where people reach first — leaving it
    /// inert while the setting hid one level into the name's menu was the part
    /// that felt wrong.
    ///
    /// Only becomes a control once there is a `WorkspaceID`: without one there is
    /// no stable key, and a pick would be written somewhere it could not be read
    /// back from. Unkeyed, it stays the plain disc it was.
    @ViewBuilder
    private var monogramButton: some View {
        if workspaceID == nil {
            WorkspaceMonogram(name: name, color: selectedSwatch.color)
        } else {
            Button {
                isPickingColor = true
            } label: {
                WorkspaceMonogram(name: name, color: selectedSwatch.color)
                    // The disc stays 20pt; the *target* is 28, which is what
                    // macOS wants for a hit area and what made a 20pt disc a
                    // poor place to put a control before. Padding rather than a
                    // frame so the extra area is centred on the glyph, and
                    // `contentShape` so the corners of that box are not live.
                    .padding(4)
                    .contentShape(.circle)
            }
            .buttonStyle(.plain)
            .pointerStyle(.link)
            .help("Workspace color")
            .accessibilityLabel("Workspace color")
            .accessibilityValue(selectedSwatch.label)
            // Matches the padding above so the disc keeps its measured position
            // in the bar — the target grows inward, not outward.
            .padding(-4)
            // `.top`: the identity bar sits at the sidebar's bottom edge, so the
            // palette has to open upward into the list rather than off-window.
            .popover(isPresented: $isPickingColor, arrowEdge: .top) {
                colorPalette
            }
        }
    }

    /// The palette as a grid, which is the shape macOS uses for picking one of a
    /// fixed set — Finder tags, Reminders lists, Calendar calendars.
    ///
    /// Drawn by hand rather than with `.pickerStyle(.palette)`, which is
    /// menu-only in practice: outside a menu it degrades to a segmented control
    /// of text labels and drops the per-item `.tint` entirely (verified in a
    /// capture). Inside a menu it renders correctly but hovers each swatch with
    /// the standard rounded-rect menu-row highlight, so a circle sits in a box.
    private var colorPalette: some View {
        LazyVGrid(
            columns: Array(
                repeating: GridItem(.fixed(18), spacing: 10),
                count: WorkspaceMonogramTint.columnCount
            ),
            spacing: 10
        ) {
            ForEach(WorkspaceMonogramTint.all) { swatch in
                swatchButton(swatch)
            }
        }
        .padding(14)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.sidebarIdentityColorPicker)
        .accessibilityLabel("Workspace color")
    }

    private func swatchButton(_ swatch: WorkspaceMonogramTint) -> some View {
        Button {
            storedTint = swatch.id
            isPickingColor = false
        } label: {
            Circle()
                .fill(swatch.color)
                .frame(width: 18, height: 18)
                // Ring outside the swatch rather than a fill change, so the
                // colour being chosen is never altered by the act of selecting
                // it. Concentric with the swatch, which is the part the menu's
                // rounded-rect hover got wrong.
                .overlay {
                    Circle()
                        .strokeBorder(.primary, lineWidth: 2)
                        .padding(-3.5)
                        .opacity(swatch == selectedSwatch ? 1 : (hoveredTint == swatch ? 0.35 : 0))
                }
                .padding(4)
                .contentShape(.circle)
        }
        .buttonStyle(.plain)
        .onHover { hoveredTint = $0 ? swatch : (hoveredTint == swatch ? nil : hoveredTint) }
        .help(swatch.label)
        .accessibilityLabel(swatch.label)
        .accessibilityAddTraits(swatch == selectedSwatch ? [.isSelected] : [])
    }

    /// The paths, as the thing you act on rather than a line of text. Labelled by
    /// path instead of by project name because the name is already the block's
    /// title in the single-project shape, and the path is the fact the inline
    /// second line used to carry.
    @ViewBuilder
    private var identityMenu: some View {
        VStack(alignment: .leading, spacing: 1) {
            revealSection
            Divider()
                .padding(.vertical, 5)
            menuRow("Add Project…", systemImage: "plus", action: addProject)
        }
        // Wider than it is tall, and deliberately: the rows carry their own
        // 8pt inset, so this is what sets the gap from the popover's edge to
        // the text — 20pt, close to the 14 in the palette beside it once its
        // swatches' own padding is counted. Tighter read as squished.
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        // Floors the width so a Workspace with one short path does not open a
        // popover narrower than its own title, and caps it so a deep path
        // truncates instead of stretching the popover across the window.
        .frame(minWidth: 200, maxWidth: 340, alignment: .leading)
    }

    @ViewBuilder
    private var revealSection: some View {
        if projects.isEmpty {
            if let workspaceFiles {
                menuHeader("Workspace Files")
                revealRow(workspaceFiles.canonicalPath)
            } else {
                menuHeader("Projects")
                // Not a `menuRow`: nothing happens on click, so it should not
                // take a hover highlight that implies otherwise.
                Text("No projects")
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
            }
        } else {
            menuHeader(projects.count == 1 ? "Project" : "Projects")
            ForEach(projects) { member in
                revealRow(member.source.canonicalPath)
            }
        }
    }

    private func menuHeader(_ title: String) -> some View {
        Text(title)
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.bottom, 2)
    }

    private func revealRow(_ path: String) -> some View {
        let label = abbreviated(path)
        return menuRow(label, help: "Reveal \(label) in Finder") {
            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
        }
    }

    /// A menu row rebuilt by hand, since a popover has none of the row chrome an
    /// `NSMenu` supplies. Rounded-rect highlight here rather than the concentric
    /// ring the swatches use: these rows *are* rectangles, and the ring only won
    /// there because a box around a circle is the mismatch.
    private func menuRow(
        _ label: String,
        systemImage: String? = nil,
        help: String? = nil,
        action: @escaping () -> Void
    ) -> some View {
        Button {
            isShowingIdentityMenu = false
            action()
        } label: {
            HStack(spacing: 6) {
                if let systemImage {
                    Image(systemName: systemImage)
                        .frame(width: 13)
                        .foregroundStyle(.secondary)
                }
                Text(label)
                    .lineLimit(1)
                    // Middle, not tail: the meaningful end of a path is the
                    // last component.
                    .truncationMode(.middle)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(.primary.opacity(hoveredRow == label ? 0.09 : 0))
            )
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .onHover { hoveredRow = $0 ? label : (hoveredRow == label ? nil : hoveredRow) }
        .help(help ?? label)
    }

    /// Component-aware abbreviation; a raw `hasPrefix` check would turn a sibling
    /// like /Users/meg into ~g for home /Users/me.
    private func abbreviated(_ path: String) -> String {
        (path as NSString).abbreviatingWithTildeInPath
    }
}
