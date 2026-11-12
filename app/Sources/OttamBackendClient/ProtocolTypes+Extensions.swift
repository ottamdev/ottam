import Foundation

public struct ResolvedProject: Equatable, Sendable {
    public var project: ProjectSummary
    public var source: ProjectSourceSummary

    public init(project: ProjectSummary, source: ProjectSourceSummary) {
        self.project = project
        self.source = source
    }

    public var projectID: ProjectID { project.projectID }
    public var projectSourceID: ProjectSourceID { source.projectSourceID }
    public var displayName: String { project.displayName }
    public var canonicalRoot: String { source.canonicalPath }
    public var sourceName: String { source.displayName }
    public var sourcePathForDisplay: String { source.canonicalPath }
    public var currentBranch: String? { source.currentBranch }
}

public extension ProjectOpenPathResult {
    var resolvedProject: ResolvedProject {
        ResolvedProject(project: project, source: source)
    }
}

public extension WorkspaceProjectSummary {
    var resolvedProject: ResolvedProject {
        ResolvedProject(project: project, source: source)
    }
}

// Hand-written conveniences layered on the generated wire types
// (ProtocolTypes.generated.swift). Wire shapes live in the Rust protocol crate;
// this file carries only Swift-side ergonomics: Identifiable conformances,
// convenience initializers, and app-side constants.

// MARK: - Identifiable

extension EngineModelOption: Identifiable {
    public var id: String { modelID }
}

extension WorkspaceSummary: Identifiable {
    public var id: WorkspaceID { workspaceID }
}

extension WorkspaceListItem: Identifiable {
    public var id: WorkspaceID { workspace.workspaceID }
}

extension WorkspaceProjectSummary: Identifiable {
    public var id: ProjectID { project.projectID }
}

extension EngineSummary: Identifiable {
    public var id: EngineID { engineID }
}

extension ConversationSummary: Identifiable {
    public var id: ConversationID { conversationID }
}

extension AgentSummary: Identifiable {
    public var id: AgentID { agentID }
}

extension ConversationMessage: Identifiable {
    public var id: ConversationMessageID { messageID }
}

extension ConversationRunSummary: Identifiable {
    public var id: ConversationRunID { runID }
}

// MARK: - Wire-value conveniences

extension AgentPermissionOutcome {
    public var jsonValue: JSONValue {
        switch self {
        case .cancelled:
            return .object(["outcome": .string("cancelled")])
        case .selected(let optionID):
            return .object(["outcome": .string("selected"), "optionID": .string(optionID)])
        }
    }
}

extension AgentQuestionOutcome {
    public var jsonValue: JSONValue {
        switch self {
        case .cancelled:
            return .object(["outcome": .string("cancelled")])
        case .skipped:
            return .object(["outcome": .string("skipped")])
        case .submitted(let answers):
            return .object([
                "outcome": .string("submitted"),
                "answers": .array(answers.map { answer in
                    var fields: [String: JSONValue] = [
                        "questionID": .string(answer.questionID),
                        "values": .array(answer.values.map(JSONValue.string)),
                    ]
                    if let other = answer.other {
                        fields["other"] = .string(other)
                    }
                    return .object(fields)
                }),
            ])
        }
    }
}

// MARK: - App-side constants

extension Capabilities {
    /// The core→client requests the app answers.
    public static let app = Capabilities(
        experimentalMethods: ["agent.requestPermission", "agent.askQuestion"]
    )
}

// MARK: - Phase 4 transitional conveniences

// The Phase 4 protocol reshape moved ConversationDetail to bounded pages
// ({conversation, latestMessagePage, latestRunPage, ...}). These accessors keep
// the pre-reshape call sites compiling until Stage 8 rebuilds the Chat UI on
// the paged shapes directly.
extension ConversationDetail {
    public var summary: ConversationSummary {
        get { conversation }
        set { conversation = newValue }
    }

    /// Messages oldest-first, as the transcript renders them. The wire page is
    /// newest-first.
    public var messages: [ConversationMessage] {
        latestMessagePage.messages.reversed()
    }

    public var runs: [ConversationRunSummary] {
        latestRunPage.runs
    }
}
