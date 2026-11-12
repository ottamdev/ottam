import AppKit
import OttamMarkdownEngine
import SwiftUI

/// A reader's place in a document, kept while the document's view comes and
/// goes: switching tabs or conversations unmounts it.
@MainActor
final class ReadingPosition {
    var anchor: TranscriptViewportAnchor?
}

/// Read-only markdown drawn by the transcript renderer. It re-applies only
/// when the text, root, or theme changes, so a reader's scroll position holds
/// across unrelated view updates.
struct MarkdownDocumentView: NSViewControllerRepresentable {
    let id: String
    let markdown: String
    /// Drawn as a property list above `markdown`.
    var frontmatter: MarkdownFrontmatter?
    let accessibilityIdentifier: String
    /// Relative links and images resolve here and must stay inside it; see
    /// `MarkdownImageAttachment.resolve` for a folder inside a root.
    var resourceRoot: URL?
    var openLink: (URL) -> Void = { NSWorkspace.shared.open($0) }
    /// A file's reader keeps their place across edits and returns to it when
    /// the view is rebuilt. Without one, as for a plan, which is replaced by
    /// the next one, new text starts at the top.
    var readingPosition: ReadingPosition?

    @Environment(\.appTypography) private var typography

    @MainActor
    final class Coordinator {
        var applied: (markdown: String, frontmatter: MarkdownFrontmatter?, resourceRoot: URL?, theme: MarkdownTheme)?
        var cards: (store: ChatTranscriptCardStore, theme: MarkdownTheme)?
        var readingPosition: ReadingPosition?
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSViewController(context: Context) -> MacAgentTranscriptViewController {
        let viewController = MacAgentTranscriptViewController()
        viewController.view.setAccessibilityIdentifier(accessibilityIdentifier)
        update(viewController, coordinator: context.coordinator)
        return viewController
    }

    func updateNSViewController(_ viewController: MacAgentTranscriptViewController, context: Context) {
        update(viewController, coordinator: context.coordinator)
    }

    static func dismantleNSViewController(_ viewController: MacAgentTranscriptViewController, coordinator: Coordinator) {
        coordinator.readingPosition?.anchor = viewController.readingPosition()
        viewController.tearDown()
    }

    private func update(_ viewController: MacAgentTranscriptViewController, coordinator: Coordinator) {
        viewController.openLink = openLink
        coordinator.readingPosition = readingPosition
        let theme = MarkdownTheme.ottam(typography)
        if let applied = coordinator.applied, applied.markdown == markdown, applied.frontmatter == frontmatter,
           applied.resourceRoot == resourceRoot, applied.theme == theme { return }
        let isFirst = coordinator.applied == nil
        let scrollsToTop = isFirst || (readingPosition == nil && coordinator.applied?.markdown != markdown)
        let restoring = isFirst ? readingPosition?.anchor : nil
        coordinator.applied = (markdown, frontmatter, resourceRoot, theme)
        // A stable item id lets an edited file keep the blocks before the
        // edit, and with them the reader's place.
        let itemID = readingPosition == nil ? "\(id)/\(markdown.hashValue)" : id
        var plan = [TranscriptItem(id: itemID, content: .markdown(markdown, syntax: .full, container: nil))]
        if let frontmatter {
            plan.insert(TranscriptItem(id: "\(id)/frontmatter", content: .card(.frontmatter(frontmatter))), at: 0)
        }
        if coordinator.cards?.theme != theme {
            let factory = LiveChatTranscriptCardFactory(
                theme: theme,
                environment: HostedCardEnvironment(typography: typography),
                actions: ChatTranscriptActions(),
                onPayloadChanged: { [weak viewController] card in viewController?.cardPayloadDidChange(card) }
            )
            coordinator.cards = (ChatTranscriptCardStore(factory: factory), theme)
        }
        let cards = coordinator.cards?.store.reconcile(plan).cards ?? [:]
        viewController.applyPreparingLargeSettledTranscript(
            plan: plan,
            cards: cards,
            conversationID: nil,
            theme: theme,
            resourceRoot: resourceRoot,
            permitsBackgroundPreparation: readingPosition != nil,
            completion: CanonicalPresentationCompletion { [weak viewController] succeeded in
                guard succeeded, let viewController else { return }
                if let restoring {
                    viewController.restoreReadingPosition(restoring)
                } else if scrollsToTop {
                    viewController.scrollToTop()
                }
            }
        )
    }
}
