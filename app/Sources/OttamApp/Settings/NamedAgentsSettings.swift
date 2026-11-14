import OttamBackendClient
import OttamWorkspaceLogic
import Observation
import SwiftUI

/// The named agents a conversation can delegate to through Ottam's
/// `delegate` tool. Owned by the Agents pane and gone with it.
@MainActor
@Observable
final class NamedAgentsSettingsModel {
    /// Posted after a save or delete. A named agent's children draw its
    /// current tile, so open windows re-list their conversations.
    static let changedNotification = Notification.Name("dev.ottam.Ottam.namedAgentsChanged")

    private let client: OttamCoreProcessClient
    private(set) var namedAgents: [NamedAgent] = []
    /// The agents a named agent can run on, with their models and modes, as
    /// a workspace's engine list reports them.
    private(set) var engines: [EngineSummary] = []
    private(set) var didLoadOnce = false
    private(set) var errorText: String?

    init(client: OttamCoreProcessClient) {
        self.client = client
    }

    /// Bumped by each save and delete, so a load that listed before one
    /// landed keeps the newer list instead.
    private var mutations = 0

    func load(workspaceID: WorkspaceID?) async {
        let mutationsAtStart = mutations
        do {
            let listed = try await client.listNamedAgents()
            engines = (try? await client.listEngines(workspaceID: workspaceID).engines) ?? engines
            if mutations == mutationsAtStart {
                namedAgents = listed.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            }
            errorText = nil
            didLoadOnce = true
        } catch {
            errorText = error.localizedDescription
        }
    }

    func engine(_ engineID: EngineID) -> EngineSummary? {
        engines.first { $0.engineID == engineID }
    }

    func save(_ namedAgentID: NamedAgentID?, config: NamedAgentConfig) async throws {
        let saved = if let namedAgentID {
            try await client.updateNamedAgent(namedAgentID: namedAgentID, config: config)
        } else {
            try await client.createNamedAgent(config)
        }
        mutations += 1
        namedAgents.removeAll { $0.namedAgentID == saved.namedAgentID }
        namedAgents.append(saved)
        namedAgents.sort { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        NotificationCenter.default.post(name: Self.changedNotification, object: nil)
    }

    func delete(_ namedAgentID: NamedAgentID) async throws {
        try await client.deleteNamedAgent(namedAgentID: namedAgentID)
        mutations += 1
        namedAgents.removeAll { $0.namedAgentID == namedAgentID }
        NotificationCenter.default.post(name: Self.changedNotification, object: nil)
    }
}

/// What the sheet edits: a new named agent, or the one with `namedAgentID`.
struct NamedAgentDraft: Identifiable {
    var namedAgentID: NamedAgentID?
    var name = ""
    var engineID = ""
    /// Empty is the agent's own default model.
    var modelID = ""
    /// Empty is the agent's read-only mode, which core fills in.
    var modeID = ""
    var instructions = ""
    /// Always set, so a save stores the tile and a rename keeps it: a new
    /// agent starts on a random one, and an agent saved before tiles keeps
    /// the one its name drew. Shuffle picks another.
    var avatarSeed: Int

    var id: String { namedAgentID?.rawValue ?? "new" }

    var avatar: AgentAvatar { AgentAvatar(seed: UInt32(truncatingIfNeeded: avatarSeed)) }

    init(engineID: String = "") {
        self.engineID = engineID
        avatarSeed = Int(AgentAvatar.randomSeed())
    }

    init(_ agent: NamedAgent) {
        namedAgentID = agent.namedAgentID
        name = agent.name
        engineID = agent.engineID.rawValue
        modelID = agent.modelID ?? ""
        modeID = agent.modeID ?? ""
        instructions = agent.instructions
        avatarSeed = Int(agent.avatar.seed)
    }

    var config: NamedAgentConfig {
        NamedAgentConfig(
            name: name.trimmingCharacters(in: .whitespaces),
            engineID: EngineID(engineID),
            modelID: modelID.isEmpty ? nil : modelID,
            modeID: modeID.isEmpty ? nil : modeID,
            instructions: instructions,
            avatarSeed: avatarSeed
        )
    }

    var canSubmit: Bool {
        !name.trimmingCharacters(in: .whitespaces).isEmpty && !engineID.isEmpty
    }
}

/// The Agents pane's Named Agents section: each named agent with its agent,
/// model, and mode, opened for editing in a sheet.
struct NamedAgentsSection: View {
    let model: NamedAgentsSettingsModel
    @Binding var editing: NamedAgentDraft?
    /// The agent Add starts on; nil is the first one listed.
    var defaultEngineID: EngineID?

    var body: some View {
        Section {
            if let errorText = model.errorText {
                Label {
                    Text("Could not read named agents")
                    Text(errorText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } icon: {
                    Image(systemName: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                }
            } else if model.didLoadOnce && model.namedAgents.isEmpty {
                Text("No named agents yet.")
                    .foregroundStyle(.secondary)
            }
            ForEach(model.namedAgents, id: \.namedAgentID) { agent in
                Button {
                    editing = NamedAgentDraft(agent)
                } label: {
                    HStack(spacing: 10) {
                        AgentAvatarView(avatar: agent.avatar, size: 28)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(agent.name)
                            Text(detail(agent))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("namedAgent.row.\(agent.name)")
            }
        } header: {
            HStack {
                Text("Named Agents")
                Spacer()
                Button("Add Named Agent…") {
                    editing = NamedAgentDraft(engineID: (defaultEngineID ?? model.engines.first?.engineID)?.rawValue ?? "")
                }
                .disabled(!model.didLoadOnce)
                .accessibilityIdentifier("namedAgent.add")
            }
        } footer: {
            Text("A conversation can hand a task to a named agent. Ottam opens a conversation with it, under the one that asked, and brings its report back when it finishes.")
        }
    }

    private func detail(_ agent: NamedAgent) -> String {
        let engine = model.engine(agent.engineID)
        let agentName = engine?.displayName ?? agent.engineID.rawValue
        // No model set runs the agent's default, so name the one it runs
        // now; omitted until the agent's catalog lists it.
        let model = (agent.modelID ?? engine?.defaultModelID).map { modelID in
            engine?.models.first { $0.modelID == modelID }?.displayName ?? modelID
        }
        let mode = agent.modeID.map { modeID in
            engine?.modes.first { $0.id == modeID }?.displayName ?? modeID
        }
        return [agentName, model, mode].compactMap(\.self).joined(separator: " · ")
    }
}

/// Add or edit a named agent. Core checks the name and agent; its error is
/// shown here.
struct NamedAgentSheet: View {
    let model: NamedAgentsSettingsModel
    @State var draft: NamedAgentDraft
    @State private var submitting = false
    @State private var confirmingDelete = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    LabeledContent("Tile") {
                        HStack(spacing: 10) {
                            Button("Shuffle") { draft.avatarSeed = Int(AgentAvatar.randomSeed()) }
                                .help("Pick another tile for this agent")
                                .accessibilityIdentifier("namedAgent.shuffle")
                            AgentAvatarView(
                                avatar: draft.avatar, size: 32,
                                label: draft.name.isEmpty ? "Tile" : "\(draft.name)’s tile"
                            )
                            .accessibilityIdentifier("namedAgent.avatar")
                        }
                    }
                    TextField("Name", text: $draft.name, prompt: Text("review-bot"))
                        .accessibilityIdentifier("namedAgent.name")
                    Picker("Agent", selection: $draft.engineID) {
                        if draft.engineID.isEmpty {
                            Text("Choose…").tag("")
                        }
                        ForEach(engineOptions, id: \.id) { option in
                            Text(option.displayName).tag(option.id)
                        }
                    }
                    .accessibilityIdentifier("namedAgent.agent")
                    Picker("Model", selection: $draft.modelID) {
                        Text("Agent’s default").tag("")
                        ForEach(engine?.models ?? [], id: \.modelID) { model in
                            Text(model.displayName).tag(model.modelID)
                        }
                        if !draft.modelID.isEmpty, engine?.models.contains(where: { $0.modelID == draft.modelID }) != true {
                            Text(draft.modelID).tag(draft.modelID)
                        }
                    }
                    Picker("Mode", selection: $draft.modeID) {
                        Text("Read-only").tag("")
                        ForEach(engine?.modes ?? [], id: \.id) { mode in
                            Text(mode.displayName).tag(mode.id)
                        }
                        if !draft.modeID.isEmpty, engine?.modes.contains(where: { $0.id == draft.modeID }) != true {
                            Text(draft.modeID).tag(draft.modeID)
                        }
                    }
                    .accessibilityIdentifier("namedAgent.mode")
                } header: {
                    Text(draft.namedAgentID == nil ? "Add Named Agent" : "Edit Named Agent")
                } footer: {
                    Text("Agents delegate by this name: letters, digits, hyphens, and underscores. Its conversations start in this mode; read-only unless you choose another.")
                }
                Section {
                    TextEditor(text: $draft.instructions)
                        .font(.body)
                        .frame(minHeight: 120)
                        .accessibilityLabel("Instructions")
                        .accessibilityIdentifier("namedAgent.instructions")
                } header: {
                    Text("Instructions")
                } footer: {
                    Text("Sent ahead of each task it is given.")
                }
            }
            .formStyle(.grouped)

            Divider()

            HStack(alignment: .firstTextBaseline) {
                if draft.namedAgentID != nil {
                    Button("Delete…", role: .destructive) { confirmingDelete = true }
                        .disabled(submitting)
                }
                if let error {
                    Text(error)
                        .foregroundStyle(.red)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                Spacer()
                if submitting {
                    ProgressView().controlSize(.small)
                }
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(submitting)
                Button(draft.namedAgentID == nil ? "Add" : "Save") {
                    run { try await model.save(draft.namedAgentID, config: draft.config) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!draft.canSubmit || submitting)
                .accessibilityIdentifier("namedAgent.save")
            }
            .padding(16)
        }
        .frame(width: 520, height: 560)
        .interactiveDismissDisabled(submitting)
        .onChange(of: draft.engineID) {
            draft.modelID = ""
            draft.modeID = ""
        }
        .confirmationDialog(
            "Delete \(draft.name)?",
            isPresented: $confirmingDelete
        ) {
            Button("Delete", role: .destructive) {
                guard let namedAgentID = draft.namedAgentID else { return }
                run { try await model.delete(namedAgentID) }
            }
        } message: {
            Text("Conversations it already has stay.")
        }
    }

    private var engine: EngineSummary? {
        model.engine(EngineID(draft.engineID))
    }

    private var engineOptions: [EngineSelectOption] {
        var options = model.engines.map { EngineSelectOption(id: $0.engineID.rawValue, displayName: $0.displayName) }
        if !draft.engineID.isEmpty, !options.contains(where: { $0.id == draft.engineID }) {
            options.append(EngineSelectOption(id: draft.engineID, displayName: draft.engineID))
        }
        return options
    }

    private func run(_ action: @escaping () async throws -> Void) {
        submitting = true
        error = nil
        Task {
            defer { submitting = false }
            do {
                try await action()
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
