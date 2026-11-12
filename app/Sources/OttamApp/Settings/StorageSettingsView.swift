import OttamBackendClient
import SwiftUI

struct StorageSettingsView: View {
    let client: OttamCoreProcessClient
    @State private var status: StorageStatusResult?
    @State private var limit = "5"
    @State private var busy = false
    @State private var error: String?
    @State private var confirmClear = false

    private var parsedLimit: UInt64? {
        guard let value = UInt64(limit), value > 0, value <= UInt64(Int64.max) / 1_000_000_000 else { return nil }
        return value
    }

    var body: some View {
        Form {
            Section("Tool Result Cache") {
                LabeledContent("Cached tool results", value: status.map { bytes($0.cacheBytes) } ?? "Loading…")
                HStack {
                    Text("Cache limit")
                    Spacer()
                    TextField("GB", text: $limit)
                        .labelsHidden()
                        .frame(width: 80)
                        .accessibilityLabel("Cache limit in GB")
                        .disabled(busy)
                    Text("GB").foregroundStyle(.secondary)
                    Button("Apply") {
                        guard let gb = parsedLimit else { return }
                        Task { await perform { try await client.updateStorageLimit(gb: gb) } }
                    }
                    .disabled(parsedLimit == nil || busy)
                }
                Text("Older tool results are removed when the cache fills. Ottam tries to restore them from the saved agent session when you open them. Some results may no longer be available.")
                    .foregroundStyle(.secondary)
                Button("Clear Cache…", role: .destructive) { confirmClear = true }
                    .disabled(busy || status == nil)
                Text("Results in use remain protected and can temporarily exceed the limit. Assistant messages, reasoning, images, and file changes are kept.")
                    .foregroundStyle(.secondary)
            }
            Section("Payload Storage") {
                LabeledContent("Database", value: status.map { bytes($0.databaseBytes) } ?? "—")
                LabeledContent("Write-ahead log", value: status.map { bytes($0.walBytes) } ?? "—")
                Text("Database space includes retained content and reusable pages. Clearing cached results may not reduce the file size immediately.")
                    .foregroundStyle(.secondary)
                Button("Refresh") { Task { await perform { try await client.storageStatus() } } }
                    .disabled(busy)
            }
            if let error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
        }
        .formStyle(.grouped)
        .navigationTitle("Storage")
        .task { await perform { try await client.storageStatus() } }
        .confirmationDialog("Clear cached tool results?", isPresented: $confirmClear) {
            Button("Clear Cache", role: .destructive) {
                Task { await perform { try await client.clearStorageCache() } }
            }
        } message: {
            Text("Ottam will try to restore results when you open them, but results missing from the saved agent session cannot be recovered. Results currently in use will remain cached.")
        }
    }

    private func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .decimal)
    }

    @MainActor private func perform(_ operation: () async throws -> StorageStatusResult) async {
        guard !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let result = try await operation()
            status = result
            limit = String(result.limitGb)
        } catch { self.error = error.localizedDescription }
    }
}
