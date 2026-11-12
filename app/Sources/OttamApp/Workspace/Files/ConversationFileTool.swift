import AppKit
import OttamAccessibility
import SwiftUI

/// A file tab: one markdown file, read-only, following edits on disk.
struct ConversationFileTool: View {
    let file: MarkdownFileID
    let document: MarkdownFileDocument
    /// This tab's place in the file; another conversation's tab keeps its own.
    let readingPosition: ReadingPosition
    /// The conversation's root.
    let root: URL
    let openLink: (URL) -> Void

    private var folder: URL { file.url.deletingLastPathComponent() }

    /// The file's folder, relative to the root when inside it: relative links,
    /// images, and code-span paths start in the folder and stay inside the
    /// root. A file outside it, such as an agent's plan, is rooted at its own
    /// folder. `file` is already resolved, so a name test is enough.
    private var resourceRoot: URL {
        let root = root.resolvingSymlinksInPath().path
        let folder = folder.path
        guard folder.hasPrefix(root + "/") else { return folder == root ? self.root : self.folder }
        return URL(
            fileURLWithPath: String(folder.dropFirst(root.count + 1)),
            isDirectory: true,
            relativeTo: URL(fileURLWithPath: root, isDirectory: true)
        )
    }

    var body: some View {
        switch document.content {
        case .loading:
            Color.clear
        case .loaded(let text):
            MarkdownDocumentView(
                id: "file:\(file.path)",
                markdown: text.body,
                frontmatter: text.frontmatter,
                accessibilityIdentifier: AccessibilityID.chatFileTab,
                resourceRoot: resourceRoot,
                openLink: openLink,
                readingPosition: readingPosition
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .missing:
            unavailable("File Not Found", systemImage: "doc.questionmark", "\(file.path) doesn’t exist.", canOpen: false)
        case .unreadable:
            unavailable("Can’t Open File", systemImage: "exclamationmark.triangle", "\(file.path) can’t be read.", canOpen: true)
        case .tooLarge:
            unavailable("File Too Large", systemImage: "doc.text", "\(file.name) is too large to show here.", canOpen: true)
        case .notText:
            unavailable("Not a Text File", systemImage: "doc", "\(file.name) isn’t UTF-8 text.", canOpen: true)
        }
    }

    private func unavailable(_ title: String, systemImage: String, _ description: String, canOpen: Bool) -> some View {
        ContentUnavailableView {
            Label(title, systemImage: systemImage)
        } description: {
            Text(description)
        } actions: {
            if canOpen {
                Button("Open with Default App") { NSWorkspace.shared.open(file.url) }
            }
        }
        .accessibilityIdentifier(AccessibilityID.chatFileTabUnavailable)
    }
}
