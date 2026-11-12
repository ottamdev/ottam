import SwiftUI

/// Native sheets shared by profile creation and rename. Identity never comes
/// from this editable text; core owns allocation and validation.
struct AgentProfileLabelSheet: View {
    let title: String
    let actionTitle: String
    let initialLabel: String
    let existingLabels: [String]
    let submit: (String) async throws -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var label = ""
    @State private var submitting = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var trimmed: String { label.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var valid: Bool {
        !trimmed.isEmpty && trimmed.count <= 80
            && !trimmed.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
            && !existingLabels.contains { $0.lowercased() == trimmed.lowercased() }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(title).font(.headline)
            TextField("Profile name", text: $label).focused($focused)
                .accessibilityIdentifier("agent.profile.label")
            if let error { Text(error).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true) }
            if actionTitle == "Create" {
                Text("Creates an empty Ottam profile with its own sign-in. Configuration and credentials are not copied from another profile.")
                    .font(.callout).foregroundStyle(.secondary)
            }
            HStack {
                if submitting { ProgressView().controlSize(.small) }
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction).disabled(submitting)
                Button(actionTitle) {
                    submitting = true
                    Task {
                        defer { submitting = false }
                        do { try await submit(trimmed); dismiss() }
                        catch { self.error = error.localizedDescription }
                    }
                }.keyboardShortcut(.defaultAction).disabled(!valid || submitting)
            }
        }
        .padding(24).frame(width: 420)
        .interactiveDismissDisabled(submitting)
        .onAppear { label = initialLabel; focused = true }
    }
}

struct AgentProfileDeleteSheet: View {
    let label: String
    let managed: Bool
    let submit: (Bool) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var purge = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Delete \(label)?").font(.headline)
            Text("Conversations remain readable, but this profile can no longer continue them. Other profiles and the shared provider installation are preserved.")
            if managed {
                Toggle("Also remove this profile’s home", isOn: $purge)
                Text("Removes this profile’s credentials, configuration and vendor sessions. Vendor keychain entries are preserved.")
                    .font(.callout).foregroundStyle(.secondary)
            } else {
                Text("Your CLI home will be preserved.").foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Delete Profile", role: .destructive) { dismiss(); submit(purge) }
            }
        }.padding(24).frame(width: 440)
    }
}
