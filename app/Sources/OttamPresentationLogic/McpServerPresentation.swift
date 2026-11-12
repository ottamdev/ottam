import OttamBackendClient
import Foundation

/// A server's state as Settings shows it.
public struct McpServerStatePresentation: Equatable, Sendable {
    public enum Tone: Equatable, Sendable {
        case idle, busy, good, problem
    }

    public let text: String
    public let tone: Tone
    /// What went wrong, as core reported it.
    public let detail: String?

    public init(state: McpServerState) {
        (text, tone) = switch state.status {
        case .notStarted: ("Not started", .idle)
        case .starting: ("Starting", .busy)
        case .running: ("Running", .good)
        case .failed: ("Failed", .problem)
        case .signedOut: ("Signed out", .problem)
        }
        detail = state.error
    }
}

public extension McpServerConfig {
    var isLocal: Bool {
        if case .stdio = transport { true } else { false }
    }

    /// The command line or URL, for a row's subtitle. Arguments with spaces
    /// are quoted so the line reads as it would be typed.
    var connectionText: String {
        switch transport {
        case .stdio(let command, let args, _):
            ([command] + args)
                .map { $0.contains(" ") || $0.isEmpty ? "\"\($0)\"" : $0 }
                .joined(separator: " ")
        case .http(let url, _):
            url
        }
    }

    /// Environment variable or header names, a secret's marked as such.
    var valueNames: [String] {
        let values = switch transport {
        case .stdio(_, _, let env): env
        case .http(_, let headers): headers
        }
        return values.map { $0.secret ? "\($0.name) (secret)" : $0.name }
    }
}

/// The add and edit sheet's fields. Core validates what it is sent; the
/// sheet only waits for the fields every server needs.
public struct McpServerDraft: Equatable, Sendable {
    public enum Kind: String, CaseIterable, Identifiable, Sendable {
        case local, remote

        public var id: Self { self }

        public var title: String {
            switch self {
            case .local: "Local"
            case .remote: "Remote"
            }
        }
    }

    /// An environment variable or header.
    public struct Value: Equatable, Identifiable, Sendable {
        public let id: UUID
        public var name: String
        public var value: String
        public var secret: Bool
        /// The name a saved secret is stored under. Core never sends its
        /// value back, and keeps it while this row is left empty.
        public let storedSecret: String?

        public init(name: String = "", value: String = "", secret: Bool = false, storedSecret: String? = nil) {
            id = UUID()
            self.name = name
            self.value = value
            self.secret = secret
            self.storedSecret = storedSecret
        }

        public var trimmedName: String { name.trimmingCharacters(in: .whitespaces) }

        /// Whether the saved secret stays as it is.
        public var keepsStoredSecret: Bool {
            secret && value.isEmpty && storedSecret == trimmedName
        }

        var isBlank: Bool { trimmedName.isEmpty && value.isEmpty }

        var config: McpConfigValue {
            McpConfigValue(name: trimmedName, value: keepsStoredSecret ? nil : value, secret: secret)
        }
    }

    public struct Argument: Equatable, Identifiable, Sendable {
        public let id: UUID
        public var text: String

        public init(_ text: String = "") {
            id = UUID()
            self.text = text
        }
    }

    public var name = ""
    public var kind = Kind.local
    public var command = ""
    public var arguments: [Argument] = []
    public var environment: [Value] = []
    public var url = ""
    public var headers: [Value] = []

    public init() {}

    public init(config: McpServerConfig) {
        name = config.name
        switch config.transport {
        case .stdio(let command, let args, let env):
            kind = .local
            self.command = command
            arguments = args.map(Argument.init)
            environment = env.map(Self.value)
        case .http(let url, let headers):
            kind = .remote
            self.url = url
            self.headers = headers.map(Self.value)
        }
    }

    private static func value(_ value: McpConfigValue) -> Value {
        Value(
            name: value.name,
            value: value.value ?? "",
            secret: value.secret,
            storedSecret: value.secret && value.value == nil ? value.name : nil
        )
    }

    private var trimmedName: String { name.trimmingCharacters(in: .whitespaces) }

    public var canSubmit: Bool {
        guard !trimmedName.isEmpty else { return false }
        return switch kind {
        case .local: !command.trimmingCharacters(in: .whitespaces).isEmpty
        case .remote: !url.trimmingCharacters(in: .whitespaces).isEmpty
        }
    }

    /// What core is sent. Rows left blank are dropped; the other kind's
    /// fields are kept in the draft, so switching back loses nothing.
    public var config: McpServerConfig {
        let transport: McpServerTransport = switch kind {
        case .local:
            .stdio(
                command: command.trimmingCharacters(in: .whitespaces),
                args: arguments.map(\.text).filter { !$0.isEmpty },
                env: environment.filter { !$0.isBlank }.map(\.config)
            )
        case .remote:
            .http(
                url: url.trimmingCharacters(in: .whitespaces),
                headers: headers.filter { !$0.isBlank }.map(\.config)
            )
        }
        return McpServerConfig(name: trimmedName, transport: transport)
    }
}

public extension McpImportSource {
    /// The import sheet's title and the menu item that opens it.
    var title: String {
        switch self {
        case .setup(let harnessID):
            switch harnessID {
            case "claude": "Import from Claude Code"
            case "codex": "Import from Codex"
            default: "Import from \(harnessID)"
            }
        case .file: "Import from .mcp.json"
        }
    }
}

/// The import sheet's choices: core's preview, each server ticked unless
/// one already has its name or its source turned it off, and how each
/// import went.
public struct McpImportSelection: Equatable, Sendable {
    public enum Outcome: Equatable, Sendable {
        case imported
        case failed(String)
    }

    public struct Row: Identifiable, Equatable, Sendable {
        public var id: String { candidate.config.name }
        public let candidate: McpImportCandidate
        public var isSelected: Bool
        public var outcome: Outcome?

        /// Already added, now or by this sheet, so it cannot be chosen.
        public var isSettled: Bool {
            candidate.existingServerID != nil || outcome == .imported
        }

        /// The row's subtitle lines: what it runs or reaches, its values,
        /// and its name in the file when that had to change.
        public var details: [String] {
            let config = candidate.config
            var lines = [config.connectionText]
            if !config.valueNames.isEmpty {
                lines.append("\(config.isLocal ? "Environment" : "Headers"): \(config.valueNames.joined(separator: ", "))")
            }
            if let sourceName = candidate.sourceName {
                lines.append("Named “\(sourceName)” there")
            }
            return lines
        }

        public var status: String? {
            switch outcome {
            case .imported: "Imported"
            case .failed(let error): error
            case nil: candidate.existingServerID == nil ? nil : "Already added"
            }
        }
    }

    public let path: String
    public var rows: [Row]
    public let skipped: [McpImportSkipped]

    public init(_ result: McpImportCandidatesResult) {
        path = result.path
        rows = result.candidates.map {
            Row(candidate: $0, isSelected: $0.existingServerID == nil && !$0.disabledInSource, outcome: nil)
        }
        skipped = result.skipped
    }

    /// What Import adds: chosen, and neither present nor added already.
    public var chosen: [McpServerConfig] {
        rows.filter { $0.isSelected && !$0.isSettled }.map(\.candidate.config)
    }

    public var isFinished: Bool {
        !rows.isEmpty && rows.allSatisfy { !$0.isSelected || $0.isSettled }
    }

    public mutating func record(_ outcome: Outcome, for name: String) {
        guard let index = rows.firstIndex(where: { $0.id == name }) else { return }
        rows[index].outcome = outcome
    }
}
