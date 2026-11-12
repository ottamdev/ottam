import AppKit
import OttamBackendClient
import QuickLookUI
import SwiftUI

/// Coordinate space the attachment cards report their frames in, so Quick Look
/// can zoom out of the card the user actually clicked rather than fading in.
let chatAttachmentRowSpace = "chat.attachmentRow"

/// Drives the shared system Quick Look panel for one row of attachments.
///
/// `QLPreviewPanel` is controlled through the responder chain, not by direct
/// ownership: the panel walks up from the key window's first responder looking
/// for something that answers `acceptsPreviewPanelControl`. So the row plants an
/// invisible `NSView` (`QuickLookAnchor` below) in the hierarchy and makes it
/// first responder on click; this object holds the state that view vends.
@MainActor
@Observable
final class ChatAttachmentQuickLook {
    /// Card frames in `chatAttachmentRowSpace`, keyed by attachment ID. Written
    /// by each card as it lays out, read once when the panel opens.
    var cardFrames: [String: CGRect] = [:]

    fileprivate var items: [QuickLookAttachmentItem] = []
    fileprivate var startIndex = 0
    fileprivate weak var anchor: QuickLookAnchorView?

    func present(_ attachments: [ConversationAttachment], startingAt index: Int) {
        // Hand the panel the whole row, not just the clicked card, so the arrow
        // keys move between a message's images the way they do in Finder.
        let boundedIndex = max(0, min(index, attachments.count - 1))
        let requestedID = attachments.indices.contains(boundedIndex)
            ? attachments[boundedIndex].attachmentID.rawValue
            : nil
        let togglesCurrentItem = requestedID.map { anchor?.isPresenting(attachmentID: $0) ?? false }
            ?? false
        items = attachments.map(QuickLookAttachmentItem.init)
        startIndex = boundedIndex
        anchor?.present(toggleCurrentItem: togglesCurrentItem)
    }
}

/// One image, as Quick Look wants it. Attachments are already real files at
/// canonical paths, which is the only thing `QLPreviewItem` needs.
private final class QuickLookAttachmentItem: NSObject, QLPreviewItem {
    let previewItemURL: URL?
    let previewItemTitle: String?
    /// Kept so the anchor can map an item back to its card for the zoom origin.
    let attachmentID: String

    init(_ attachment: ConversationAttachment) {
        previewItemURL = URL(fileURLWithPath: attachment.path)
        previewItemTitle = switch attachment.sourceKind {
        case .pasted: "Pasted image"
        default: (attachment.originPath as NSString?)?.lastPathComponent ?? "Attached image"
        }
        attachmentID = attachment.attachmentID.rawValue
    }
}

/// Zero-size view whose only job is to sit in the responder chain.
///
/// It is installed as the background of the card row, so its bounds share an
/// origin with `chatAttachmentRowSpace` and a card's SwiftUI frame converts
/// straight through `convert(_:to: nil)`.
struct QuickLookAnchor: NSViewRepresentable {
    let controller: ChatAttachmentQuickLook

    func makeNSView(context: Context) -> QuickLookAnchorView {
        let view = QuickLookAnchorView()
        view.controller = controller
        controller.anchor = view
        return view
    }

    func updateNSView(_ view: QuickLookAnchorView, context: Context) {
        view.controller = controller
        controller.anchor = view
    }
}

final class QuickLookAnchorView: NSView, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    weak var controller: ChatAttachmentQuickLook?
    /// Whatever had focus before the panel opened — usually the composer's text
    /// view. Taking first responder is unavoidable; keeping it isn't.
    private weak var previousResponder: NSResponder?

    /// Matches SwiftUI's top-left origin, so a card's reported frame needs no
    /// flipping before `convert(_:to:)`.
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    func isPresenting(attachmentID: String) -> Bool {
        guard let panel = QLPreviewPanel.shared(),
              panel.isVisible,
              panel.dataSource as AnyObject? === self,
              let items = controller?.items,
              items.indices.contains(panel.currentPreviewItemIndex)
        else { return false }
        return items[panel.currentPreviewItemIndex].attachmentID == attachmentID
    }

    func present(toggleCurrentItem: Bool) {
        guard let panel = QLPreviewPanel.shared() else { return }
        // Clicking the card that's already open closes the panel, so the
        // thumbnail toggles the way the space bar does. Identity matters as well
        // as the index: another row's first card is also index 0, and that
        // should switch the preview rather than dismiss it.
        if toggleCurrentItem {
            panel.orderOut(nil)
            return
        }
        if window?.firstResponder !== self {
            previousResponder = window?.firstResponder
            window?.makeFirstResponder(self)
        }
        if panel.isVisible {
            // The key window did not change, so Quick Look does not
            // automatically walk the updated responder chain. Transfer control
            // explicitly before touching panel state.
            panel.updateController()
            guard panel.dataSource as AnyObject? === self else { return }
            loadItems(into: panel)
        } else {
            // Ordering matters. `makeKeyAndOrderFront` is what makes the panel
            // walk the responder chain and adopt us, and on the first call it is
            // also creating the panel — so its `dataSource` is still nil until
            // `beginPreviewPanelControl` lands. Loading here instead of on the
            // next line is the difference between the first click working and
            // silently opening an empty panel.
            panel.makeKeyAndOrderFront(nil)
        }
    }

    private func loadItems(into panel: QLPreviewPanel) {
        panel.reloadData()
        panel.currentPreviewItemIndex = controller?.startIndex ?? 0
    }

    // MARK: - Responder-chain control

    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool { true }

    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.dataSource = self
        panel.delegate = self
        loadItems(into: panel)
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.dataSource = nil
        panel.delegate = nil
        if let previousResponder {
            window?.makeFirstResponder(previousResponder)
            self.previousResponder = nil
        }
    }

    // MARK: - Data source

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        controller?.items.count ?? 0
    }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! {
        guard let items = controller?.items, items.indices.contains(index) else { return nil }
        return items[index]
    }

    // MARK: - Delegate

    /// The on-screen rect the panel zooms out of. Returning `.zero` is legal and
    /// degrades to a fade, so every step here is allowed to fail.
    func previewPanel(
        _ panel: QLPreviewPanel!,
        sourceFrameOnScreenFor item: (any QLPreviewItem)!
    ) -> NSRect {
        guard let item = item as? QuickLookAttachmentItem,
              let frame = controller?.cardFrames[item.attachmentID],
              let window
        else { return .zero }
        return window.convertToScreen(convert(frame, to: nil))
    }
}
