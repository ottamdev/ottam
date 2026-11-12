import OttamBackendClient
import OttamPresentationLogic
import SwiftUI

/// One import sheet's source; a new request opens a fresh sheet.
struct McpImportRequest: Identifiable {
    let id = UUID()
    let source: McpImportSource
}

/// Core's preview of the servers a setup or file defines. Each is added
/// with `mcp.add` only once it is ticked and Import is clicked.
struct McpImportSheet: View {
    let source: McpImportSource
    let model: McpServersSettingsModel
    @State private var selection: McpImportSelection?
    @State private var loadError: String?
    @State private var importing = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            Group {
                if let selection {
                    list(selection)
                } else if let loadError {
                    ContentUnavailableView {
                        Label("Nothing to Import", systemImage: "exclamationmark.triangle")
                    } description: {
                        Text(loadError)
                    }
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(maxHeight: .infinity)

            Divider()

            HStack {
                Spacer()
                if importing {
                    ProgressView().controlSize(.small)
                }
                Button(didImport ? "Done" : "Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(importing)
                Button("Import") { importChosen() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(selection?.chosen.isEmpty ?? true || importing)
            }
            .padding(16)
        }
        .frame(width: 560, height: 520)
        .interactiveDismissDisabled(importing)
        .task {
            do {
                selection = try await model.importCandidates(source)
            } catch {
                loadError = error.localizedDescription
            }
        }
    }

    private var didImport: Bool {
        selection?.rows.contains { $0.outcome == .imported } ?? false
    }

    private func list(_ selection: McpImportSelection) -> some View {
        Form {
            Section {
                if selection.rows.isEmpty {
                    Text("No servers are defined there.")
                        .foregroundStyle(.secondary)
                }
                ForEach(selection.rows) { row in
                    McpImportRow(row: row, isSelected: chosen(row.id))
                        .disabled(row.isSettled || importing)
                }
            } header: {
                Text(source.title)
            } footer: {
                Text("Read from \(selection.path). Each server is added as written there; values marked secret go to the Keychain. Edit a server after importing to change it.")
            }

            if !selection.skipped.isEmpty {
                Section("Not Imported") {
                    ForEach(selection.skipped, id: \.name) { skipped in
                        LabeledContent(skipped.name) {
                            Text(skipped.reason)
                                .multilineTextAlignment(.trailing)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
    }

    private func chosen(_ name: String) -> Binding<Bool> {
        Binding(
            get: { selection?.rows.first { $0.id == name }?.isSelected ?? false },
            set: { isSelected in
                guard let index = selection?.rows.firstIndex(where: { $0.id == name }) else { return }
                selection?.rows[index].isSelected = isSelected
            }
        )
    }

    /// Adds the chosen servers one at a time, recording each outcome; the
    /// sheet closes once all are added and stays open to show a failure.
    private func importChosen() {
        guard let configs = selection?.chosen else { return }
        importing = true
        Task {
            defer { importing = false }
            for config in configs {
                do {
                    _ = try await model.add(config)
                    selection?.record(.imported, for: config.name)
                } catch {
                    selection?.record(.failed(error.localizedDescription), for: config.name)
                }
            }
            if selection?.isFinished == true {
                dismiss()
            }
        }
    }
}

private struct McpImportRow: View {
    let row: McpImportSelection.Row
    @Binding var isSelected: Bool

    var body: some View {
        Toggle(isOn: $isSelected) {
            VStack(alignment: .leading, spacing: 2) {
                Text(row.candidate.config.name)
                // In full, wrapped: this is the command being approved.
                ForEach(Array(row.details.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(Array(row.candidate.notes.enumerated()), id: \.offset) { _, note in
                    Label(note, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let status = row.status {
                    Text(status)
                        .font(.caption)
                        .foregroundStyle(statusColor)
                        .textSelection(.enabled)
                }
            }
        }
        .toggleStyle(.checkbox)
    }

    private var statusColor: Color {
        if case .failed = row.outcome { .red } else { .secondary }
    }
}
