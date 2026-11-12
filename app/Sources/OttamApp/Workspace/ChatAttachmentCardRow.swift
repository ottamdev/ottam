import AppKit
import OttamBackendClient
import ImageIO
import SwiftUI

/// The row of image previews that sits inside the composer card, above the text
/// field, and again (smaller, undeletable) above a user bubble in the transcript.
///
/// Cards are a fixed square and aspect-**fill**: a wide screenshot crops rather
/// than letterboxing into a band of empty space.
struct ChatAttachmentCardRow: View {
    let attachments: [ConversationAttachment]
    var size: CGFloat = 148
    /// Leading in the composer, where cards read as a queue growing rightwards;
    /// trailing in the transcript, so they hang off the same edge as the bubble.
    var alignment: HorizontalAlignment = .leading
    /// The composer scrolls, because its card row is pinned to the field's
    /// width. The transcript doesn't: `scrollEdgeEffectStyle` propagates into
    /// descendant scroll views, and the transcript sets `.soft` on both its top
    /// and bottom — inherited by a nested scroll view, it fades the top and
    /// bottom of every card. Growing leftwards off the bubble's edge is both
    /// the simpler layout and the one that can't inherit an edge effect.
    var scrolls = true
    /// One per row, so Quick Look's arrow keys walk that message's images.
    /// Owned by the caller: in the native transcript the hosted card owns it,
    /// because the row's view is rebuilt from the payload on every change.
    let quickLook: ChatAttachmentQuickLook
    /// Omitted in the transcript, where an already-sent attachment can't be taken back.
    var onRemove: ((ConversationAttachment) -> Void)?

    var body: some View {
        if scrolls {
            ScrollView(.horizontal) {
                cards
                    // A ScrollView parks short content at its leading edge, so a
                    // trailing-aligned frame around the scroll view alone doesn't
                    // move the cards. Matching the container's width and aligning
                    // inside it does, and still scrolls once they outgrow it.
                    .containerRelativeFrame(
                        .horizontal,
                        alignment: Alignment(horizontal: alignment, vertical: .center)
                    )
            }
            .scrollIndicators(.hidden)
        } else {
            cards
        }
    }

    private var cards: some View {
        HStack(alignment: .top, spacing: 10) {
            ForEach(Array(attachments.enumerated()), id: \.element.attachmentID) { index, attachment in
                ChatAttachmentCard(
                    attachment: attachment,
                    size: size,
                    quickLook: quickLook,
                    onOpen: { quickLook.present(attachments, startingAt: index) },
                    onRemove: onRemove.map { remove in { remove(attachment) } }
                )
            }
        }
        .coordinateSpace(.named(chatAttachmentRowSpace))
        .background(QuickLookAnchor(controller: quickLook))
        // The ✕ badge overhangs the card's corner, so the row needs room on
        // those two edges or the scroll view clips it. Without a badge there
        // is nothing to make room for, and the inset would push the cards
        // off the bubble's edge.
        .padding(.top, onRemove == nil ? 0 : 8)
        .padding(.trailing, onRemove == nil ? 0 : 8)
    }
}

private struct ChatAttachmentCard: View {
    let attachment: ConversationAttachment
    let size: CGFloat
    let quickLook: ChatAttachmentQuickLook
    let onOpen: () -> Void
    let onRemove: (() -> Void)?

    @State private var image: NSImage?

    var body: some View {
        ZStack(alignment: .topTrailing) {
            Button(action: onOpen) {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(.quaternary)
                    .frame(width: size, height: size)
                    .overlay {
                        if let image {
                            Image(nsImage: image)
                                .resizable()
                                .aspectRatio(contentMode: .fill)
                        } else {
                            Image(systemName: "photo")
                                .font(.system(size: size * 0.2))
                                .foregroundStyle(.tertiary)
                        }
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    // `clipShape` clips drawing, not hit testing: an aspect-fill
                    // image is wider than the card it fills — a 3.5:1 terminal
                    // capture overflows a 108 pt card by 133 pt each side — and
                    // those invisible flanks still claim clicks, over the top of
                    // whichever cards precede it in the stack.
                    .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .stroke(ReviewColors.cardBorder, lineWidth: 1)
                    )
                    // Report where this card sits so Quick Look zooms out of it.
                    // The row's anchor view shares this space's origin.
                    .background {
                        GeometryReader { geometry in
                            Color.clear
                                .onChange(
                                    of: geometry.frame(in: .named(chatAttachmentRowSpace)),
                                    initial: true
                                ) { _, frame in
                                    quickLook.cardFrames[attachment.attachmentID.rawValue] = frame
                                }
                        }
                    }
            }
            .buttonStyle(.plain)

            if let onRemove {
                Button(action: onRemove) {
                    ZStack {
                        Circle()
                            .fill(.regularMaterial)
                            .frame(width: 22, height: 22)
                        Image(systemName: "xmark")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(.primary)
                    }
                }
                .buttonStyle(.plain)
                // Material rather than a flat fill so the glyph stays legible
                // over both a bright screenshot and a dark one.
                .offset(x: 8, y: -8)
                .help("Remove attachment")
                .accessibilityLabel("Remove attachment")
            }
        }
        // The composer card claims the I-beam for the whole surface; a thumbnail
        // is not a typing target.
        .pointerStyle(.default)
        .help("\(Self.describe(attachment)) — click to preview")
        .accessibilityLabel(Self.describe(attachment))
        .accessibilityHint("Opens a full-size preview")
        .task(id: attachment.attachmentID) {
            image = await ChatAttachmentImageCache.shared.image(for: attachment)
        }
    }

    private static func describe(_ attachment: ConversationAttachment) -> String {
        switch attachment.sourceKind {
        case .pasted:
            return "Pasted image"
        default:
            guard let origin = attachment.originPath else { return "Attached image" }
            return (origin as NSString).lastPathComponent
        }
    }
}

/// Small LRU of attachment thumbnails, keyed by attachment ID.
///
/// Scrolling a long transcript would otherwise re-read and re-decode the same
/// files on every `.task`, and attachment bytes are immutable once stored — a
/// hit can never be stale.
///
/// Thumbnails, not full images: a stored attachment is up to 1568 px on its long
/// edge, which is ~10 MB once decoded, and a cache of those would run to
/// hundreds of megabytes. The cards are 148 pt, so anything past
/// `thumbnailPixelSize` is pixels nobody sees.
@MainActor
final class ChatAttachmentImageCache {
    static let shared = ChatAttachmentImageCache()

    /// Comfortably covers the 148 pt composer card on a 2× display.
    private static let thumbnailPixelSize = 320

    private var entries: [String: NSImage] = [:]
    private var order: [String] = []
    private let limit = 64

    func image(for attachment: ConversationAttachment) async -> NSImage? {
        let key = attachment.attachmentID.rawValue
        if let hit = entries[key] {
            touch(key)
            return hit
        }
        let path = attachment.path
        let size = Self.thumbnailPixelSize
        // Decode off the main actor. `CGImage` is immutable and safe to hand
        // back; the `NSImage` wrapper is built here, on the main actor.
        let thumbnail = await Task.detached(priority: .userInitiated) {
            Self.decodeThumbnail(atPath: path, maxPixelSize: size)
        }.value
        guard let thumbnail else { return nil }
        let image = NSImage(
            cgImage: thumbnail,
            size: NSSize(width: thumbnail.width, height: thumbnail.height)
        )
        entries[key] = image
        order.append(key)
        if order.count > limit {
            let evicted = order.removeFirst()
            entries[evicted] = nil
        }
        return image
    }

    /// `CGImageSourceCreateThumbnailAtIndex` decodes straight to the target
    /// size — the full-resolution bitmap is never materialized.
    private nonisolated static func decodeThumbnail(
        atPath path: String,
        maxPixelSize: Int
    ) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(
            URL(fileURLWithPath: path) as CFURL,
            [kCGImageSourceShouldCache: false] as CFDictionary
        ) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    private func touch(_ key: String) {
        guard let index = order.firstIndex(of: key) else { return }
        order.remove(at: index)
        order.append(key)
    }
}
