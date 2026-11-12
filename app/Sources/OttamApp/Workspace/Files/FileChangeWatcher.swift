import Foundation

/// Calls `onChange` when a file changes on disk, coalescing bursts. Agents and
/// editors save by writing a new file and renaming it over the old one, so the
/// watcher follows the path, not the first file it opened: it watches the
/// file for writes and its folder for entries coming and going, and reopens
/// either when the path names a new one. `NSFilePresenter` would only hear
/// coordinated writes, which agents don't make.
@MainActor
final class FileChangeWatcher {
    private struct Watch {
        let source: DispatchSourceFileSystemObject
        let status: FileStatus
    }

    private let url: URL
    private let onChange: @MainActor () -> Void
    private var file: Watch?
    private var folder: Watch?
    /// The file as of the last callback, nil while it is missing.
    private var status: FileStatus?
    private var pending: DispatchWorkItem?
    private var isCancelled = false

    static let coalescing: DispatchTimeInterval = .milliseconds(100)

    init(url: URL, onChange: @escaping @MainActor () -> Void) {
        self.url = url
        self.onChange = onChange
        status = FileStatus(path: url.path)
        rewatch()
    }

    isolated deinit {
        cancel()
    }

    func cancel() {
        isCancelled = true
        pending?.cancel()
        pending = nil
        file?.source.cancel()
        file = nil
        folder?.source.cancel()
        folder = nil
    }

    /// Watches the file if it exists, and the nearest folder above it that
    /// does: a folder created later is noticed from its parent, and one
    /// replaced at the same path is reopened.
    private func rewatch() {
        let fileStatus = FileStatus(path: url.path)
        if Self.needsReopen(file, for: fileStatus) {
            file?.source.cancel()
            file = fileStatus.flatMap {
                watch(url, status: $0, events: [.write, .extend, .attrib, .delete, .rename, .revoke])
            }
        }
        var folderURL = url.deletingLastPathComponent()
        var folderStatus = FileStatus(path: folderURL.path)
        while folderStatus == nil, folderURL.path != "/" {
            folderURL = folderURL.deletingLastPathComponent()
            folderStatus = FileStatus(path: folderURL.path)
        }
        if Self.needsReopen(folder, for: folderStatus) {
            folder?.source.cancel()
            folder = folderStatus.flatMap { watch(folderURL, status: $0, events: [.write, .delete, .rename, .revoke]) }
        }
    }

    /// Whether `watch` no longer holds the file `status` names.
    private static func needsReopen(_ watch: Watch?, for status: FileStatus?) -> Bool {
        guard let status else { return watch != nil }
        return !status.isSameFile(as: watch?.status)
    }

    /// A burst calls back once, `coalescing` after its first event, so a
    /// steady stream of writes still shows every `coalescing`.
    private func changed() {
        guard pending == nil else { return }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.isCancelled else { return }
                self.pending = nil
                self.rewatch()
                // The folder also reports its other entries; only this file's
                // changes call back.
                let status = FileStatus(path: self.url.path)
                guard status != self.status else { return }
                self.status = status
                self.onChange()
            }
        }
        pending = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.coalescing, execute: work)
    }

    /// Nil when the path can't be opened; the folder's source notices when it
    /// can.
    private func watch(_ url: URL, status: FileStatus, events: DispatchSource.FileSystemEvent) -> Watch? {
        let descriptor = open(url.path, O_EVTONLY)
        guard descriptor >= 0 else { return nil }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: descriptor, eventMask: events, queue: .main)
        source.setEventHandler { [weak self] in MainActor.assumeIsolated { self?.changed() } }
        source.setCancelHandler { close(descriptor) }
        source.resume()
        return Watch(source: source, status: status)
    }
}

/// What `stat` says about a path: enough to tell a file from its replacement,
/// and an edit from no change.
private struct FileStatus: Equatable {
    let device: Int64
    let inode: UInt64
    let size: Int64
    let modifiedSeconds: Int
    let modifiedNanoseconds: Int

    init?(path: String) {
        var info = stat()
        guard stat(path, &info) == 0 else { return nil }
        device = Int64(info.st_dev)
        inode = UInt64(info.st_ino)
        size = Int64(info.st_size)
        modifiedSeconds = info.st_mtimespec.tv_sec
        modifiedNanoseconds = info.st_mtimespec.tv_nsec
    }

    func isSameFile(as other: FileStatus?) -> Bool {
        guard let other else { return false }
        return device == other.device && inode == other.inode
    }
}
