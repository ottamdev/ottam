import Foundation

/// Available to the detached process-reaping task even after its actor closes.
final class CoreRefreshProtection: @unchecked Sendable {
    private let lock = NSLock()
    private var paths: [String] = []
    private var hasSnapshot = false
    private var hasSharedHomes = true

    func update(paths: [String], hasSharedHomes: Bool? = nil) {
        lock.withLock {
            self.paths = paths
            self.hasSharedHomes = hasSharedHomes ?? !paths.isEmpty
            self.hasSnapshot = true
        }
    }

    var canForceTerminate: Bool { lock.withLock { hasSnapshot && !hasSharedHomes } }

    var isRefreshing: Bool {
        guard let paths = lock.withLock({ hasSnapshot ? self.paths : nil }) else { return true }
        return paths.contains { path in
            do {
                _ = try FileManager.default.attributesOfItem(atPath: path)
                return true
            } catch let error as NSError {
                // Unreadable is not proof of absence.
                return error.domain != NSCocoaErrorDomain || error.code != NSFileReadNoSuchFileError
            }
        }
    }
}
