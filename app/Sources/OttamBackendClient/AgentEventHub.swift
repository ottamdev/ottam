import Foundation
import OSLog

public struct AgentEventSubscriptionToken: Hashable, Sendable {
    fileprivate let rawValue: UUID

    public init() {
        rawValue = UUID()
    }
}

public struct AgentEventFilter: Sendable {
    public var methods: Set<String>?
    public var workspaceID: WorkspaceID?
    public var projectID: ProjectID?
    public var terminalSessionID: TerminalSessionID?
    public var agentID: AgentID?
    public var agentSessionID: AgentSessionID?
    public var conversationID: ConversationID?
    public var conversationRunID: ConversationRunID?

    public init(
        methods: Set<String>? = nil,
        workspaceID: WorkspaceID? = nil,
        projectID: ProjectID? = nil,
        terminalSessionID: TerminalSessionID? = nil,
        agentID: AgentID? = nil,
        agentSessionID: AgentSessionID? = nil,
        conversationID: ConversationID? = nil,
        conversationRunID: ConversationRunID? = nil
    ) {
        self.methods = methods
        self.workspaceID = workspaceID
        self.projectID = projectID
        self.terminalSessionID = terminalSessionID
        self.agentID = agentID
        self.agentSessionID = agentSessionID
        self.conversationID = conversationID
        self.conversationRunID = conversationRunID
    }

    public static let all = AgentEventFilter()

    /// Whether any ID field is set, so matching needs the payload's routing IDs.
    fileprivate var readsPayload: Bool {
        workspaceID != nil || projectID != nil || terminalSessionID != nil || agentID != nil
            || agentSessionID != nil || conversationID != nil || conversationRunID != nil
    }

    fileprivate func accepts(_ notification: AgentNotification, routing: EventRouting) -> Bool {
        if let methods, !methods.contains(notification.method) {
            return false
        }
        // One connection-wide recovery marker covers every conversation whose
        // transcript notifications were coalesced under backpressure.
        if notification.method == "conversation.transcriptRecoveryRequired" { return true }
        if let workspaceID, routing.workspaceID != workspaceID.rawValue { return false }
        if let projectID, routing.projectID != projectID.rawValue { return false }
        if let terminalSessionID, routing.sessionID != terminalSessionID.rawValue { return false }
        if let agentID, routing.agentID != agentID.rawValue { return false }
        if let agentSessionID, routing.sessionID != agentSessionID.rawValue { return false }
        if let conversationID, routing.conversationID != conversationID.rawValue { return false }
        if let conversationRunID, routing.runID != conversationRunID.rawValue { return false }
        return true
    }
}

/// The routing IDs a notification's params carry, decoded once per
/// notification rather than once per subscriber and field. Each field decodes
/// on its own, so a malformed one fails only the filters that read it.
private struct EventRouting: Decodable {
    var workspaceID: String?
    var projectID: String?
    var sessionID: String?
    var agentID: String?
    var conversationID: String?
    var runID: String?

    static let none = EventRouting()

    private init() {}

    init(_ notification: AgentNotification, decoder: JSONDecoder) {
        guard !notification.payload.isEmpty,
              let envelope = try? decoder.decode(Envelope.self, from: notification.payload) else {
            self = .none
            return
        }
        self = envelope.params
    }

    private struct Envelope: Decodable {
        var method: String
        var params: EventRouting
    }

    private enum CodingKeys: String, CodingKey {
        case workspaceID, projectID, sessionID, agentID, conversationID, runID
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        workspaceID = try? container.decodeIfPresent(String.self, forKey: .workspaceID)
        projectID = try? container.decodeIfPresent(String.self, forKey: .projectID)
        sessionID = try? container.decodeIfPresent(String.self, forKey: .sessionID)
        agentID = try? container.decodeIfPresent(String.self, forKey: .agentID)
        conversationID = try? container.decodeIfPresent(String.self, forKey: .conversationID)
        runID = try? container.decodeIfPresent(String.self, forKey: .runID)
    }
}

public actor AgentEventHub {
    private static let logger = Logger(
        subsystem: "dev.ottam.Ottam",
        category: "BackendEvents"
    )

    private let decoder = JSONDecoder()
    private var subscriptions: [AgentEventSubscriptionToken: Subscription] = [:]

    public init() {}

    @discardableResult
    public func subscribe(
        filter: AgentEventFilter = .all,
        handler: @escaping @Sendable (AgentNotification) async -> Void
    ) -> AgentEventSubscriptionToken {
        let token = AgentEventSubscriptionToken()
        subscriptions[token] = Subscription(filter: filter, handler: handler)
        Self.logger.debug("backend event subscriber added count=\(self.subscriptions.count, privacy: .public)")
        return token
    }

    public func unsubscribe(_ token: AgentEventSubscriptionToken) {
        subscriptions.removeValue(forKey: token)
        Self.logger.debug("backend event subscriber removed count=\(self.subscriptions.count, privacy: .public)")
    }

    public func publish(_ notification: AgentNotification) async {
        let routing = subscriptions.values.contains { $0.filter.readsPayload }
            ? EventRouting(notification, decoder: decoder)
            : .none
        let handlers = subscriptions.values.compactMap { subscription in
            subscription.filter.accepts(notification, routing: routing) ? subscription.handler : nil
        }
        guard !handlers.isEmpty else {
            Self.logger.debug("backend event had no subscribers method=\(notification.method, privacy: .public)")
            return
        }
        for handler in handlers {
            await handler(notification)
        }
    }
}

private struct Subscription: Sendable {
    var filter: AgentEventFilter
    var handler: @Sendable (AgentNotification) async -> Void
}
