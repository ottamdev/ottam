import OttamBackendClient
import Foundation

/// Where a remote server's latest sign-in stands, as Settings and a chat's
/// Sign in card show it. No phase means none was tried since launch.
public enum McpSignInPhase: Equatable, Sendable {
    /// The browser has the sign-in; core waits for it to come back.
    case waiting
    case signedIn
    case cancelled
    case failed(String)
    /// The server cannot register Ottam itself; core's message says how
    /// to register it by hand.
    case needsClientID(String)
}

/// Every remote server's sign-in, and the chats whose agent called a server
/// that needs one. Core runs one sign-in at a time, so starting one leaves
/// any other waiting one behind.
public struct McpSignInState: Equatable, Sendable {
    public struct Need: Equatable, Sendable {
        public let serverID: McpServerID
        public let serverName: String
    }

    public private(set) var phases: [McpServerID: McpSignInPhase] = [:]
    public private(set) var needs: [ConversationID: [Need]] = [:]
    /// Servers whose cancel this app asked for. Core also finishes a sign-in
    /// as cancelled when a newer one replaces it, which says nothing about
    /// the newer one.
    private var cancelling: Set<McpServerID> = []

    public init() {}

    public func phase(_ serverID: McpServerID) -> McpSignInPhase? {
        phases[serverID]
    }

    public var isWaiting: Bool {
        phases.values.contains(.waiting)
    }

    public mutating func started(_ serverID: McpServerID) {
        for (other, phase) in phases where phase == .waiting && other != serverID {
            phases[other] = nil
        }
        cancelling.remove(serverID)
        phases[serverID] = .waiting
    }

    public mutating func startFailed(_ serverID: McpServerID, message: String) {
        phases[serverID] = .failed(message)
    }

    public mutating func needsClientID(_ serverID: McpServerID, message: String) {
        phases[serverID] = .needsClientID(message)
    }

    public mutating func cancelRequested(_ serverID: McpServerID) {
        cancelling.insert(serverID)
    }

    /// Whether the browser should get the page a started sign-in returned:
    /// not once the user cancelled it, or another took its place.
    public func opensBrowser(_ serverID: McpServerID) -> Bool {
        phases[serverID] == .waiting && !cancelling.contains(serverID)
    }

    public mutating func finished(_ finished: McpSignInFinishedParams) {
        let serverID = finished.serverID
        switch finished.outcome {
        case .signedIn:
            phases[serverID] = .signedIn
        case .cancelled:
            // Replaced by a newer sign-in for the same server: that one
            // still waits.
            guard cancelling.remove(serverID) != nil else { return }
            phases[serverID] = .cancelled
        case .failed:
            phases[serverID] = .failed(finished.error ?? "The sign-in did not finish.")
        }
    }

    /// A chat's call reached a signed-out server. A success from before no
    /// longer holds.
    public mutating func needed(_ needed: McpSignInNeededParams) {
        if phases[needed.serverID] == .signedIn {
            phases[needed.serverID] = nil
        }
        var conversation = needs[needed.conversationID] ?? []
        guard !conversation.contains(where: { $0.serverID == needed.serverID }) else { return }
        conversation.append(Need(serverID: needed.serverID, serverName: needed.serverName))
        needs[needed.conversationID] = conversation
    }

    /// The user wrote again; the cards had their turn. Returns them, for
    /// `restoreNeeds` should the message not go through.
    @discardableResult
    public mutating func clearNeeds(_ conversationID: ConversationID) -> [Need] {
        needs.removeValue(forKey: conversationID) ?? []
    }

    /// Put back the cards a message that failed to send cleared, ahead of
    /// any that came meanwhile.
    public mutating func restoreNeeds(_ restored: [Need], for conversationID: ConversationID) {
        guard !restored.isEmpty else { return }
        let arrived = needs[conversationID] ?? []
        needs[conversationID] = restored + arrived.filter { need in
            !restored.contains { $0.serverID == need.serverID }
        }
    }

    /// A removed server's sign-in and cards go with it.
    public mutating func forget(_ serverID: McpServerID) {
        phases[serverID] = nil
        cancelling.remove(serverID)
        for (conversation, list) in needs {
            let kept = list.filter { $0.serverID != serverID }
            needs[conversation] = kept.isEmpty ? nil : kept
        }
    }

    public func cards(for conversationID: ConversationID) -> [ChatMcpSignIn] {
        (needs[conversationID] ?? []).map { need in
            ChatMcpSignIn(serverID: need.serverID, serverName: need.serverName, phase: phases[need.serverID])
        }
    }
}

/// A chat's Sign in card for one server its agent could not reach.
public struct ChatMcpSignIn: Equatable, Sendable {
    public enum Action: Equatable, Sendable {
        case signIn, tryAgain, cancel
    }

    public let serverID: McpServerID
    public let serverName: String
    public let phase: McpSignInPhase?

    public init(serverID: McpServerID, serverName: String, phase: McpSignInPhase?) {
        self.serverID = serverID
        self.serverName = serverName
        self.phase = phase
    }

    public var title: String {
        switch phase {
        case .waiting: "Signing in to \(serverName)…"
        case .signedIn: "Signed in to \(serverName)"
        case nil, .cancelled, .failed, .needsClientID: "Sign in to \(serverName)"
        }
    }

    public var detail: String {
        switch phase {
        case .waiting: "Finish signing in in your browser."
        case .signedIn: "Ask the agent to try again."
        case nil, .cancelled, .failed, .needsClientID:
            "The agent called the \(serverName) MCP server, which needs you to sign in."
        }
    }

    /// What went wrong with the last try, shown under the detail.
    public var failure: String? {
        switch phase {
        case .failed(let message): message
        case .needsClientID(let message): "\(message) Enter it in Settings › MCP Servers."
        default: nil
        }
    }

    public var action: Action? {
        switch phase {
        case nil, .cancelled: .signIn
        case .failed: .tryAgain
        case .waiting: .cancel
        // Trying again without the client ID fails the same way; the
        // failure says where to enter it.
        case .signedIn, .needsClientID: nil
        }
    }

    public var isSignedIn: Bool { phase == .signedIn }

    public var accessibilityPhase: String {
        switch phase {
        case nil, .cancelled: "signed out"
        case .waiting: "signing in"
        case .signedIn: "signed in"
        case .failed, .needsClientID: "failed"
        }
    }
}

/// The browser page a sign-in opens.
public enum McpSignInPage {
    /// `address` if it is a web page: https, or http on this Mac. A server's
    /// metadata names it, and another scheme would open a file, a share, or
    /// another app. Core checks this too.
    public static func url(_ address: String) -> URL? {
        guard let url = URL(string: address), let host = url.host(percentEncoded: false), !host.isEmpty else {
            return nil
        }
        switch url.scheme?.lowercased() {
        case "https":
            return url
        case "http":
            let loopback = host == "localhost" || host == "::1" || host.hasPrefix("127.")
            return loopback ? url : nil
        default:
            return nil
        }
    }
}
