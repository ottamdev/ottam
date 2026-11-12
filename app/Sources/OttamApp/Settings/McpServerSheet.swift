import OttamBackendClient
import OttamPresentationLogic
import SwiftUI

/// The add and edit sheet. Core validates the server and its error is shown
/// here; a saved secret's value is never sent back, so its row stays empty
/// and keeps it unless something is typed.
struct McpServerSheet: View {
    let title: String
    let actionTitle: String
    let submit: (McpServerConfig) async throws -> Void
    @State private var draft: McpServerDraft
    @State private var submitting = false
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    init(
        title: String,
        actionTitle: String,
        draft: McpServerDraft,
        submit: @escaping (McpServerConfig) async throws -> Void
    ) {
        self.title = title
        self.actionTitle = actionTitle
        self.submit = submit
        _draft = State(initialValue: draft)
    }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    TextField("Name", text: $draft.name, prompt: Text("docs"))
                        .accessibilityIdentifier("mcp.server.name")
                    Picker("Type", selection: $draft.kind) {
                        ForEach(McpServerDraft.Kind.allCases) { kind in
                            Text(kind.title).tag(kind)
                        }
                    }
                    .pickerStyle(.segmented)
                } header: {
                    Text(title)
                } footer: {
                    Text("Agents see the server and its tools under this name: letters, digits, hyphens, and underscores.")
                }

                switch draft.kind {
                case .local:
                    Section {
                        TextField("Command", text: $draft.command, prompt: Text("npx"))
                            .accessibilityIdentifier("mcp.server.command")
                    }
                    argumentsSection
                    McpValuesSection(title: "Environment", noun: "Variable", values: $draft.environment)
                case .remote:
                    Section {
                        TextField("URL", text: $draft.url, prompt: Text("https://example.com/mcp"))
                            .accessibilityIdentifier("mcp.server.url")
                    }
                    McpValuesSection(title: "Headers", noun: "Header", values: $draft.headers)
                }
            }
            .formStyle(.grouped)

            Divider()

            HStack(alignment: .firstTextBaseline) {
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
                Button(actionTitle) {
                    submitting = true
                    error = nil
                    Task {
                        defer { submitting = false }
                        do {
                            try await submit(draft.config)
                            dismiss()
                        } catch {
                            self.error = error.localizedDescription
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!draft.canSubmit || submitting)
            }
            .padding(16)
        }
        .frame(width: 560, height: 580)
        .interactiveDismissDisabled(submitting)
    }

    private var argumentsSection: some View {
        Section {
            ForEach($draft.arguments) { $argument in
                HStack {
                    TextField("Argument", text: $argument.text)
                        .labelsHidden()
                    removeButton("Remove argument") {
                        draft.arguments.removeAll { $0.id == argument.id }
                    }
                }
            }
            Button("Add Argument") { draft.arguments.append(.init()) }
        } header: {
            Text("Arguments")
        } footer: {
            Text("One per row, passed as typed, without a shell.")
        }
    }
}

/// Environment variables or headers: a name, a value, and whether the value
/// is a secret kept in the Keychain.
private struct McpValuesSection: View {
    let title: String
    let noun: String
    @Binding var values: [McpServerDraft.Value]

    var body: some View {
        Section {
            ForEach($values) { $value in
                HStack {
                    TextField("Name", text: $value.name, prompt: Text("Name"))
                        .labelsHidden()
                        .accessibilityLabel("\(noun) name")
                        .frame(width: 160)
                    Group {
                        if value.secret {
                            SecureField(
                                "Value",
                                text: $value.value,
                                prompt: Text(value.keepsStoredSecret ? "Saved" : "Value")
                            )
                        } else {
                            TextField("Value", text: $value.value, prompt: Text("Value"))
                        }
                    }
                    .labelsHidden()
                    .accessibilityLabel("\(value.trimmedName.isEmpty ? noun : value.trimmedName) value")
                    Toggle("Secret", isOn: $value.secret)
                        .toggleStyle(.checkbox)
                    removeButton("Remove \(value.trimmedName.isEmpty ? noun.lowercased() : value.trimmedName)") {
                        values.removeAll { $0.id == value.id }
                    }
                }
            }
            Button("Add \(noun)") { values.append(.init()) }
        } header: {
            Text(title)
        } footer: {
            Text("A secret is kept in the Keychain and never shown again. Leave a saved secret empty to keep it.")
        }
    }
}

private func removeButton(_ label: String, action: @escaping () -> Void) -> some View {
    Button(action: action) {
        Image(systemName: "minus.circle")
    }
    .buttonStyle(.borderless)
    .accessibilityLabel(label)
}
