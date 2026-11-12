import OttamBackendClient

extension String {
    /// The most of a conversation's title a one-line label shows, such as
    /// a Task card's "Started by" or a row in the agents pill's menu.
    public static let titleCap = 40

    /// The title trimmed and, past `limit` characters, cut to `limit` with
    /// "…". Neither a link button nor a menu item truncates on its own, so
    /// a label cuts the title and shows it whole in its tooltip.
    public func cappedTitle(limit: Int = titleCap) -> String {
        let title = trimmingCharacters(in: .whitespacesAndNewlines)
        guard title.count > limit else { return title }
        return title.prefix(limit - 1).trimmingCharacters(in: .whitespaces) + "…"
    }
}

/// The agents working on one task: a conversation that delegated, and the
/// conversations it delegated to. The window's agents pill names them and
/// opens each one.
public struct ConversationAgentGroup: Equatable, Sendable {
    /// How a member is pictured: its agent's brand tile for the parent,
    /// which is the product itself, or a child's own tile.
    public enum Icon: Equatable, Sendable {
        /// The brand key core sends; nil draws a monogram.
        case brand(String?)
        case avatar(AgentAvatar)
    }

    public struct Member: Equatable, Identifiable, Sendable {
        public var conversationID: ConversationID
        /// The agent, by its display name ("Claude Code") or, for a
        /// child of a named agent, the named agent's ("review-bot").
        public var agentName: String
        /// The conversation's title; a child's task.
        public var title: String
        /// "Working", "Approval required", "Finished", "Failed", "Stopped"
        /// or "Idle".
        public var state: String
        public var isParent: Bool
        public var icon: Icon

        public var id: ConversationID { conversationID }
    }

    /// The parent first, then its children oldest first.
    public var members: [Member]

    /// "Claude Code, review-bot": each agent once, in the members' order.
    public var title: String {
        var names: [String] = []
        for member in members where !names.contains(member.agentName) {
            names.append(member.agentName)
        }
        return names.joined(separator: ", ")
    }

    /// The group `selected` belongs to; nil when it neither delegated nor
    /// was delegated, or its parent is not listed.
    ///
    /// - Parameters:
    ///   - agentNames: Agents by conversation, as far as the app knows
    ///     them; a parent it does not know reads as "Agent".
    ///   - agentBrands: Their brand keys, for the parent's tile.
    public static func of(
        _ selected: ConversationID?,
        conversations: [ConversationSummary],
        agentNames: [ConversationID: String],
        agentBrands: [ConversationID: String] = [:],
        running: Set<ConversationID>,
        attention: Set<ConversationID>
    ) -> Self? {
        guard let selected, let conversation = conversations.first(where: { $0.conversationID == selected }) else {
            return nil
        }
        let parentID = conversation.parentConversationID ?? selected
        guard let parent = conversations.first(where: { $0.conversationID == parentID }) else { return nil }
        let children = conversations
            .filter { $0.parentConversationID == parentID }
            .sorted { ($0.createdAt, $0.conversationID.rawValue) < ($1.createdAt, $1.conversationID.rawValue) }
        guard !children.isEmpty else { return nil }
        func state(_ summary: ConversationSummary) -> String {
            if attention.contains(summary.conversationID) { return "Approval required" }
            if running.contains(summary.conversationID) { return "Working" }
            switch summary.delegationActivity?.latestRunStatus {
            case .queued, .running: return "Working"
            case .completed: return "Finished"
            case .failed: return "Failed"
            case .canceled, .interrupted: return "Stopped"
            case nil: return "Idle"
            }
        }
        let parentMember = Member(
            conversationID: parentID,
            agentName: agentNames[parentID] ?? "Agent",
            title: parent.title,
            state: attention.contains(parentID) ? "Approval required" : running.contains(parentID) ? "Working" : "Idle",
            isParent: true,
            icon: .brand(agentBrands[parentID])
        )
        return ConversationAgentGroup(members: [parentMember] + children.map { child in
            Member(
                conversationID: child.conversationID,
                agentName: child.delegatedAgentName ?? agentNames[child.conversationID] ?? "Agent",
                title: child.taskTitle,
                state: state(child),
                isParent: false,
                icon: child.delegatedAvatar.map(Icon.avatar) ?? .brand(agentBrands[child.conversationID])
            )
        })
    }
}
