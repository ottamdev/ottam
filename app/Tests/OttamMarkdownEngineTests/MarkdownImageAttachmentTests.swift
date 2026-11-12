import AppKit
import Testing
@testable import OttamMarkdownEngine

@Suite("MarkdownImageAttachment")
struct MarkdownImageAttachmentTests {
    @Test("wide images aspect-fit the container width")
    func aspectFitsWidth() {
        let size = MarkdownImageAttachment.displaySize(
            imageSize: CGSize(width: 1200, height: 600),
            containerWidth: 400
        )
        #expect(size.width == 400)
        #expect(size.height == 200)
    }

    @Test("tall images cap at the display height and shrink width to match")
    func capsHeight() {
        let size = MarkdownImageAttachment.displaySize(
            imageSize: CGSize(width: 800, height: 1600),
            containerWidth: 700
        )
        #expect(size.height == MarkdownImageAttachment.maxDisplayHeight)
        #expect(size.width == MarkdownImageAttachment.maxDisplayHeight / 2)
    }

    @Test("small images render at native size, never upscaled")
    func neverUpscales() {
        let size = MarkdownImageAttachment.displaySize(
            imageSize: CGSize(width: 90, height: 60),
            containerWidth: 700
        )
        #expect(size == CGSize(width: 90, height: 60))
    }

    @Test("unknown image size falls back to the placeholder footprint")
    func placeholderFootprint() {
        let size = MarkdownImageAttachment.displaySize(imageSize: nil, containerWidth: 700)
        #expect(size == MarkdownImageAttachment.placeholderSize)
    }

    @Test("placeholder stays compact and shrinks into narrower containers")
    func placeholderShrinks() {
        let ordinary = MarkdownImageAttachment.displaySize(imageSize: nil, containerWidth: 110)
        #expect(ordinary == MarkdownImageAttachment.placeholderSize)

        let narrow = MarkdownImageAttachment.displaySize(imageSize: nil, containerWidth: 10)
        #expect(narrow.width == 10)
        #expect(narrow.height == 9)
    }

    @Test("degenerate inputs fall back instead of dividing by zero")
    func degenerateInputs() {
        let zeroImage = MarkdownImageAttachment.displaySize(
            imageSize: .zero,
            containerWidth: 400
        )
        #expect(zeroImage.width > 0 && zeroImage.height > 0)

        let zeroContainer = MarkdownImageAttachment.displaySize(
            imageSize: CGSize(width: 100, height: 50),
            containerWidth: 0
        )
        #expect(zeroContainer.width > 0 && zeroContainer.height > 0)
    }

    @Test("styler turns image runs into placeholder attachments")
    func stylerEmitsAttachments() {
        let theme = MarkdownTheme(
            body: NSFont.systemFont(ofSize: 13),
            code: NSFont.monospacedSystemFont(ofSize: 12, weight: .regular),
            heading1: NSFont.boldSystemFont(ofSize: 20),
            heading2: NSFont.boldSystemFont(ofSize: 17),
            heading3: NSFont.boldSystemFont(ofSize: 13),
            heading4: NSFont.boldSystemFont(ofSize: 12),
            textColor: .labelColor,
            secondaryTextColor: .secondaryLabelColor,
            inlineCodeBackground: NSColor.labelColor.withAlphaComponent(0.07),
            codeBackground: .textBackgroundColor,
            codeBorder: .separatorColor,
            quoteBar: .tertiaryLabelColor,
            ruleColor: .separatorColor,
            syntaxColors: [:],
            signature: "image-test-theme"
        )
        let styled = MarkdownStyler(theme: theme).attributedString(
            from: "before ![alt text](https://example.com/i.png) after"
        )

        var attachments: [MarkdownImageAttachment] = []
        styled.enumerateAttribute(
            .attachment,
            in: NSRange(location: 0, length: styled.length)
        ) { value, _, _ in
            if let attachment = value as? MarkdownImageAttachment {
                attachments.append(attachment)
            }
        }
        #expect(attachments.count == 1)
        #expect(attachments.first?.url == URL(string: "https://example.com/i.png"))
        #expect(attachments.first?.altText == "alt text")
        #expect(attachments.first?.imageSize == nil)
        #expect(styled.string.contains("before "))
        #expect(styled.string.contains(" after"))
    }

    @Test("loader swaps the payload and reports back on main")
    @MainActor
    func loaderSwapsPayload() async throws {
        let image = NSImage(size: CGSize(width: 8, height: 4), flipped: false) { rect in
            NSColor.systemBlue.setFill()
            rect.fill()
            return true
        }
        let tiff = image.tiffRepresentation!
        let png = NSBitmapImageRep(data: tiff)!.representation(using: .png, properties: [:])!
        let fileURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-engine-image-test-\(UUID().uuidString).png")
        try png.write(to: fileURL)
        defer { try? FileManager.default.removeItem(at: fileURL) }

        let attachment = MarkdownImageAttachment(url: fileURL, altText: "tiny")
        let loader = MarkdownImageLoader()
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            loader.loadIfNeeded(attachment) { _ in continuation.resume() }
        }
        #expect(attachment.imageSize == CGSize(width: 8, height: 4))

        // Second attachment for the same URL fills synchronously from cache.
        let cached = MarkdownImageAttachment(url: fileURL)
        var filledSynchronously = false
        loader.loadIfNeeded(cached) { _ in filledSynchronously = true }
        #expect(filledSynchronously)
        #expect(cached.imageSize == CGSize(width: 8, height: 4))
    }

    @Test("a failed load is not retried")
    @MainActor
    func failedLoadIsNotRetried() async throws {
        // A failed image sits in a block that restyles on every streamed chunk,
        // and the styler mints a fresh attachment each time. Without a failed
        // set that is one fetch per chunk, forever.
        let missing = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-engine-missing-\(UUID().uuidString).png")
        let loader = MarkdownImageLoader()

        let first = MarkdownImageAttachment(url: missing, altText: "gone")
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            loader.loadIfNeeded(first) { _ in continuation.resume() }
        }
        #expect(first.loadFailed)

        // A fresh attachment for the same URL resolves synchronously off the
        // failed set — the async fetch path could never do that.
        let second = MarkdownImageAttachment(url: missing)
        var resolvedSynchronously = false
        loader.loadIfNeeded(second) { _ in resolvedSynchronously = true }
        #expect(resolvedSynchronously)
        #expect(second.loadFailed)
    }

    @Test("repeated loads on a failed attachment report once")
    @MainActor
    func repeatedLoadIfNeededOnAFailedAttachmentFiresOnce() async throws {
        let missing = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-engine-missing-\(UUID().uuidString).png")
        let loader = MarkdownImageLoader()
        let attachment = MarkdownImageAttachment(url: missing)

        var completions = 0
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            loader.loadIfNeeded(attachment) { _ in
                completions += 1
                continuation.resume()
            }
        }
        loader.loadIfNeeded(attachment) { _ in completions += 1 }
        loader.loadIfNeeded(attachment) { _ in completions += 1 }
        #expect(completions == 1, "a settled attachment re-reported \(completions) times")
    }

    @Test("markLoadFailed reports a change only once")
    func markLoadFailedReportsChangeOnlyOnce() {
        // Callers can skip a layout invalidation that would change nothing.
        let attachment = MarkdownImageAttachment(url: URL(string: "https://example.com/x.png")!)
        #expect(attachment.markLoadFailed())
        #expect(!attachment.markLoadFailed())
        #expect(attachment.loadFailed)
    }

    @Test("a failed load keeps the compact placeholder footprint")
    func failedLoadKeepsCompactPlaceholderFootprint() {
        let attachment = MarkdownImageAttachment(url: URL(string: "https://example.com/x.png")!)
        let placeholder = attachment.displaySize(containerWidth: 600)
        attachment.markLoadFailed()
        #expect(attachment.imageSize == nil)
        #expect(attachment.displaySize(containerWidth: 600) == MarkdownImageAttachment.failureSize)
        #expect(attachment.displaySize(containerWidth: 600) == placeholder)
    }

    @Test("a failed glyph shrinks into a narrower container")
    func failedGlyphShrinks() {
        let attachment = MarkdownImageAttachment(url: URL(string: "https://example.com/x.png")!)
        attachment.markLoadFailed()
        let size = attachment.displaySize(containerWidth: 10)
        #expect(size.width == 10)
        #expect(size.height == 9)
    }
}
