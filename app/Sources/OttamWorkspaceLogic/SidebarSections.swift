import OttamBackendClient

/// Why a row's working root is worth calling out, when it is not where the
/// surrounding layout implies the row lives.
///
/// Two cases rather than one flag because they are different facts and deserve
/// different marks: living in the managed files directory is ordinary, while
/// living at a project this Workspace no longer holds is a loose end. A single
/// symbol would blur them.
///
/// The classification lives here; the symbol and wording are the view's, so this
/// stays testable without SwiftUI.
public enum SidebarRootMark: Equatable, Sendable {
    /// Rooted at the Workspace's managed files directory.
    case workspaceFiles
    /// Rooted at a project that is no longer part of this Workspace. Carries the
    /// project's name as the root still records it, since the Workspace can no
    /// longer supply one.
    case detachedProject(name: String)
}

/// A conversation row in the merged sidebar, with its live status resolved at
/// build time so rows stay value-type equatable.
public struct SidebarConversationItem: Identifiable, Equatable {
    public let summary: ConversationSummary
    public let needsAttention: Bool
    public let isRunning: Bool
    /// Set when the row's root is *not* where the surrounding layout implies it
    /// lives, and `nil` when it agrees.
    ///
    /// Resolved here rather than in the view because the trigger differs by
    /// shape: in the collapsed shape any row whose root differs from the implied
    /// root is marked, while in the multi-project shape the per-project sections
    /// already say where their rows live, so only the loose bucket is.
    public let rootMark: SidebarRootMark?
    /// The conversations this one delegated to, nested under its row in
    /// recency order. Empty for a child, which never delegates.
    public let children: [SidebarConversationItem]

    public init(
        summary: ConversationSummary,
        needsAttention: Bool,
        isRunning: Bool,
        rootMark: SidebarRootMark? = nil,
        children: [SidebarConversationItem] = []
    ) {
        self.summary = summary
        self.needsAttention = needsAttention
        self.isRunning = isRunning
        self.rootMark = rootMark
        self.children = children
    }

    public var id: ConversationID { summary.conversationID }

    /// A delegated child waits on the user. Its parent carries this while the
    /// child's own row is out of sight, so a folded or capped parent still
    /// says something needs you.
    public var childNeedsAttention: Bool { children.contains(where: \.needsAttention) }

    /// A delegated child is working.
    public var childIsRunning: Bool { children.contains(where: \.isRunning) }

    /// Rows waiting on the user: this one and its children.
    public var attentionCount: Int {
        (needsAttention ? 1 : 0) + children.count(where: \.needsAttention)
    }

    /// Whether the row stays past a "Show N More…" cap: it is selected,
    /// waiting on the user or working, itself or through a delegated child.
    public func staysPastCap(selected: ConversationID?) -> Bool {
        id == selected || needsAttention || isRunning
            || children.contains { $0.staysPastCap(selected: selected) }
    }
}

/// Section identity for the merged sidebar. Future durable-content sections
/// (specs, tasks) become new cases; the view layer renders whatever the builder
/// emits, so adding a kind is additive.
public enum SidebarSectionKind: Hashable {
    /// The whole history as one unsectioned list — the collapsed shape, where
    /// there is only one home and so nothing to group by.
    case all
    case project(ProjectID)
    case looseChats

    /// Stable string form used to persist per-section collapse state. The
    /// per-project keys stay valid across a collapse, so sections restore their
    /// previous state when a Workspace grows back to two projects.
    public var persistenceKey: String {
        switch self {
        case .all: "all"
        case .project(let id): "project:\(id.rawValue)"
        case .looseChats: "chats"
        }
    }
}

public struct SidebarSection: Identifiable {
    public let kind: SidebarSectionKind
    /// Not rendered for `.all`, whose header the view suppresses.
    public let title: String
    /// Recency order (backend `updated_at DESC` order preserved).
    public let allItems: [SidebarConversationItem]

    public init(kind: SidebarSectionKind, title: String, allItems: [SidebarConversationItem]) {
        self.kind = kind
        self.title = title
        self.allItems = allItems
    }

    public var id: SidebarSectionKind { kind }
}

/// Grouping of the flat conversation list into sidebar sections, by shape.
///
/// In the collapsed shapes (0 or 1 project) there is one home and therefore
/// nothing to group by, so the whole history is a single unsectioned list. In
/// the multi-project shape it is one section per project (in the Workspace's
/// sort order, empty ones skipped) followed by a trailing "Chats" bucket for
/// workspace-files conversations and any conversation whose project is no longer
/// part of the Workspace. Either way nothing disappears.
public enum SidebarSectionBuilder {
    /// Rows shown before the "Show more" fold.
    ///
    /// The flat list is capped higher than a per-project section on purpose: the
    /// collapsed shape replaces what used to be two sections, so reusing the
    /// per-section cap would halve the visible history in the shape this layout
    /// is meant to privilege.
    public static func visibleCap(for kind: SidebarSectionKind) -> Int {
        switch kind {
        case .all: 12
        case .project, .looseChats: 5
        }
    }

    public static func sections(
        projects: [WorkspaceProjectSummary],
        selectedProject: ResolvedProject?,
        conversations: [ConversationSummary],
        attentionConversationIDs: Set<ConversationID>,
        runningConversationIDs: Set<ConversationID>
    ) -> [SidebarSection] {
        let shape = WorkspaceShape.of(projects: projects)
        let implied = impliedWorkingRoot(projects: projects, selectedProject: selectedProject)

        // A delegated child sits under its parent, wherever its own root is;
        // one whose parent is not listed stays at the top level.
        let listedIDs = Set(conversations.map(\.conversationID))
        var childrenByParent: [ConversationID: [ConversationSummary]] = [:]
        var topLevel: [ConversationSummary] = []
        for conversation in conversations {
            if let parentID = conversation.parentConversationID, listedIDs.contains(parentID) {
                childrenByParent[parentID, default: []].append(conversation)
            } else {
                topLevel.append(conversation)
            }
        }
        let conversations = topLevel

        func item(
            _ conversation: ConversationSummary,
            mismatched: Bool
        ) -> SidebarConversationItem {
            SidebarConversationItem(
                summary: conversation,
                needsAttention: attentionConversationIDs.contains(conversation.conversationID),
                isRunning: runningConversationIDs.contains(conversation.conversationID),
                rootMark: mismatched ? mark(for: conversation.workingRoot) : nil,
                children: (childrenByParent[conversation.conversationID] ?? []).map { child in
                    SidebarConversationItem(
                        summary: child,
                        needsAttention: attentionConversationIDs.contains(child.conversationID),
                        isRunning: runningConversationIDs.contains(child.conversationID)
                    )
                }
            )
        }

        guard shape == .multiple else {
            // Collapsed: one list, recency order preserved, every row that does
            // not live at the implied root marked.
            guard !conversations.isEmpty else { return [] }
            return [
                SidebarSection(
                    kind: .all,
                    title: "History",
                    allItems: conversations.map {
                        item($0, mismatched: !$0.workingRoot.matches(implied))
                    }
                )
            ]
        }

        let knownProjectIDs = Set(projects.map(\.project.projectID))
        var byProject: [ProjectID: [SidebarConversationItem]] = [:]
        var loose: [SidebarConversationItem] = []

        for conversation in conversations {
            if conversation.workingRoot.kind == .project,
                let projectID = conversation.workingRoot.projectID,
                knownProjectIDs.contains(projectID)
            {
                byProject[projectID, default: []].append(item(conversation, mismatched: false))
            } else {
                // The section header names the project for project rows; the
                // loose bucket does not, so those rows carry their own root.
                loose.append(item(conversation, mismatched: true))
            }
        }

        var sections: [SidebarSection] = []
        for workspaceProject in projects {
            let projectID = workspaceProject.project.projectID
            guard let items = byProject[projectID], !items.isEmpty else { continue }
            sections.append(
                SidebarSection(
                    kind: .project(projectID),
                    title: workspaceProject.project.displayName,
                    allItems: items
                )
            )
        }
        if !loose.isEmpty {
            sections.append(
                SidebarSection(kind: .looseChats, title: "Chats", allItems: loose)
            )
        }
        return sections
    }
}

/// Classifies a root that has already been judged out of place.
///
/// A marked project root is by construction one this Workspace does not hold: in
/// the collapsed shape the only member project *is* the implied root, and in the
/// multi-project shape only non-member roots reach the loose bucket. So there is
/// no "some other member project" case to distinguish.
private func mark(for root: WorkingRootSummary) -> SidebarRootMark {
    switch root.kind {
    case .workspaceFiles:
        return .workspaceFiles
    default:
        return .detachedProject(name: root.displayName)
    }
}

extension WorkingRootSummary {
    /// Whether this stored root is the same place as `selection`.
    ///
    /// Compared by identity, not availability: a root whose project has left the
    /// Workspace still names the same directory, which is what lets the sidebar
    /// keep showing the row and mark it rather than dropping it.
    public func matches(_ selection: WorkingRootSelection) -> Bool {
        switch selection {
        case .workspaceFiles:
            return kind == .workspaceFiles
        case .project(let projectID, let sourceID):
            return kind == .project && self.projectID == projectID && self.sourceID == sourceID
        }
    }
}

extension ConversationSummary {
    /// A delegated child's task, which names its row: core keeps it apart
    /// from the `"<agent>: "` title it gave the child. Any other
    /// conversation's title as it is.
    public var taskTitle: String {
        delegatedAgentName == nil ? title : delegatedTaskTitle ?? title
    }
}

/// What the outline's arrow keys did, kept for the sidebar's nested rows,
/// which are not a system outline: Left folds the selected parent, or moves
/// from a child to its parent; Right unfolds a parent.
public enum SidebarKeyboardFold: Equatable {
    case fold(ConversationID)
    case unfold(ConversationID)
    case selectParent(ConversationID)

    public enum Key: Sendable {
        case left
        case right
    }

    /// The move for `key` with `selected` selected; nil when the key does
    /// nothing here, so the list keeps it.
    public static func move(
        _ key: Key,
        selected: ConversationID?,
        items: [SidebarConversationItem],
        folded: Set<ConversationID>
    ) -> Self? {
        guard let selected else { return nil }
        for item in items {
            if item.id == selected, !item.children.isEmpty {
                let isFolded = folded.contains(item.id)
                switch key {
                case .left: return isFolded ? nil : .fold(item.id)
                case .right: return isFolded ? .unfold(item.id) : nil
                }
            }
            if key == .left, item.children.contains(where: { $0.id == selected }) {
                return .selectParent(item.id)
            }
        }
        return nil
    }
}
