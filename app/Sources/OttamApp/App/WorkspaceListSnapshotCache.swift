import OttamBackendClient
import Foundation
import OSLog

private let cacheLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "WorkspaceListSnapshotCache"
)

/// Local snapshot of the last-known workspace list, so the chooser can paint
/// its final layout (list pane and rows, or the narrow welcome-only pane)
/// synchronously at launch instead of guessing and re-laying-out once Core
/// answers. Core remains the source of truth: the snapshot is overwritten on
/// every successful list fetch and only ever bridges the startup gap.
enum WorkspaceListSnapshotCache {
    private static var fileURL: URL {
        OttamPaths.home().appendingPathComponent("workspace-list.json")
    }

    static func load() -> [WorkspaceListItem] {
        do {
            let data = try Data(contentsOf: fileURL)
            return try JSONDecoder().decode([WorkspaceListItem].self, from: data)
        } catch CocoaError.fileReadNoSuchFile {
            return []
        } catch {
            cacheLogger.error("failed to load workspace list snapshot: \(error.localizedDescription, privacy: .public)")
            return []
        }
    }

    static func save(_ workspaces: [WorkspaceListItem]) {
        do {
            let data = try JSONEncoder().encode(workspaces)
            try FileManager.default.createDirectory(
                at: fileURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try data.write(to: fileURL, options: .atomic)
        } catch {
            cacheLogger.error("failed to save workspace list snapshot: \(error.localizedDescription, privacy: .public)")
        }
    }
}
