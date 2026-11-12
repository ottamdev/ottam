import Foundation
import Observation

/// A file's text, split off the main thread into its frontmatter and body.
struct MarkdownFileText: Equatable, Sendable {
    let frontmatter: MarkdownFrontmatter?
    let body: String

    init(_ text: String) {
        (frontmatter, body) = MarkdownFrontmatter.split(text)
    }
}

/// What a file tab shows.
enum MarkdownFileContent: Equatable {
    case loading
    case loaded(MarkdownFileText)
    case missing
    case unreadable
    case tooLarge
    case notText
}

/// A markdown file read off the main thread and read again whenever it
/// changes on disk, for as long as a tab shows it.
@MainActor @Observable
final class MarkdownFileDocument {
    /// Larger files are refused rather than rendered.
    nonisolated static let sizeLimit = 2 * 1024 * 1024

    let url: URL
    private(set) var content: MarkdownFileContent = .loading

    @ObservationIgnored private var watcher: FileChangeWatcher?
    @ObservationIgnored private var isReading = false
    @ObservationIgnored private var needsReread = false

    init(url: URL) {
        self.url = url
        watcher = FileChangeWatcher(url: url) { [weak self] in self?.reload() }
        reload()
    }

    /// Reads the file again; a change during a read reads once more after it.
    func reload() {
        guard watcher != nil else { return }
        guard !isReading else {
            needsReread = true
            return
        }
        isReading = true
        let url = url
        Task { [weak self] in
            let content = await Task.detached(priority: .userInitiated) { Self.read(url) }.value
            guard let self else { return }
            isReading = false
            if self.content != content { self.content = content }
            if needsReread {
                needsReread = false
                reload()
            }
        }
    }

    func stop() {
        watcher?.cancel()
        watcher = nil
    }

    nonisolated static func read(_ url: URL) -> MarkdownFileContent {
        guard let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey]) else { return .missing }
        guard values.isRegularFile == true else { return .unreadable }
        if let size = values.fileSize, size > sizeLimit { return .tooLarge }
        guard let data = try? Data(contentsOf: url) else {
            return FileManager.default.fileExists(atPath: url.path) ? .unreadable : .missing
        }
        guard let text = String(data: data, encoding: .utf8) else { return .notText }
        return .loaded(MarkdownFileText(text))
    }
}
