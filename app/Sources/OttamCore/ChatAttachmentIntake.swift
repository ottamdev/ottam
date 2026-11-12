import AppKit
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// A normalized image ready to hand to Core: the exact bytes that will be
/// stored, the type Core will re-verify against their magic bytes, and the
/// pixel dimensions Core records as presentation hints.
public struct ChatAttachmentPayload: Equatable, Sendable {
    public let data: Data
    public let mimeType: String
    public let pixelWidth: Int
    public let pixelHeight: Int

    public init(data: Data, mimeType: String, pixelWidth: Int, pixelHeight: Int) {
        self.data = data
        self.mimeType = mimeType
        self.pixelWidth = pixelWidth
        self.pixelHeight = pixelHeight
    }
}

public enum ChatAttachmentIntakeError: LocalizedError, Equatable {
    case unsupportedType
    case decodeFailed
    case encodeFailed
    case tooLarge

    public var errorDescription: String? {
        switch self {
        case .unsupportedType:
            return "That file isn’t an image Ottam can attach."
        case .decodeFailed:
            return "That image couldn’t be read."
        case .encodeFailed:
            return "That image couldn’t be prepared for sending."
        case .tooLarge:
            return "That image is too large to attach."
        }
    }
}

/// Turns whatever the user pasted, dropped, or picked into bytes Core accepts.
///
/// Deliberately free of app state so it is testable without the app (see
/// `ChatAttachmentIntakeChecks`); the TIFF→PNG path below is the piece most
/// likely to break silently.
public enum ChatAttachmentIntake {
    /// Long-edge ceiling. Above this, detail stops helping the model and the
    /// bytes start costing real money on every retry.
    public static let maximumLongEdge = 1568
    /// Mirrors Core's per-attachment cap and the DB CHECK behind it.
    public static let maximumByteCount = 3 * 1024 * 1024
    /// Mirrors the cap `AttachmentRepository::claim` enforces. Checking it in
    /// the composer turns a rejected message into a refused seventh card.
    public static let maximumAttachmentsPerMessage = 6
    /// Mirrors the total `AttachmentRepository::claim` allows across one
    /// Message. Six images are each allowed 3 MiB, so the count cap alone does
    /// not keep a message under this.
    public static let maximumByteCountPerMessage = 4 * 1024 * 1024
    /// Ceiling on what we're willing to read off disk to *produce* an
    /// attachment. The stored result is capped at `maximumByteCount`; this caps
    /// the input, so a huge source is refused instead of loaded.
    public static let maximumSourceByteCount = 64 * 1024 * 1024

    public static let acceptedMIMETypes: Set<String> = [
        "image/png", "image/jpeg", "image/gif", "image/webp"
    ]

    /// Content types offered in the file picker and accepted from a drop.
    public static var acceptedContentTypes: [UTType] {
        [.png, .jpeg, .gif, .webP]
    }

    // MARK: - Sources

    /// Where an incoming image's bytes are, before anything decodes them.
    ///
    /// Separating "is there an image here" from "turn it into a payload" keeps
    /// the expensive half — decode, downscale, re-encode — off the main thread.
    /// `NSPasteboard` can only be read from the main thread; ImageIO can run
    /// anywhere.
    public enum ChatAttachmentSource: Sendable {
        case data(Data)
        case file(URL)
    }

    /// Pull an image reference off the pasteboard without decoding it. Returns
    /// nil when the pasteboard holds no image.
    ///
    /// A ⌘⌃⇧4 screenshot arrives as **TIFF**, not PNG — which is why the
    /// conversion in `normalize` is not optional.
    public static func imageSource(from pasteboard: NSPasteboard) -> ChatAttachmentSource? {
        // A file the user copied in Finder is the most faithful source: it has
        // real bytes and a real type, so prefer it over a rendered rendition.
        if let urls = pasteboard.readObjects(forClasses: [NSURL.self]) as? [URL],
           let url = urls.first(where: { isProbablyImage($0) }) {
            return .file(url)
        }
        for type in [NSPasteboard.PasteboardType.png, .tiff] {
            if let data = pasteboard.data(forType: type) {
                return .data(data)
            }
        }
        return nil
    }

    /// Decode, downscale, and re-encode. Safe to call off the main thread, and
    /// on a large screenshot it should be: this is tens of milliseconds of work.
    public static func payload(for source: ChatAttachmentSource) throws -> ChatAttachmentPayload {
        switch source {
        case .data(let data):
            return try normalize(data)
        case .file(let url):
            return try payload(fromFileAt: url)
        }
    }

    /// True when the pasteboard's most specific entry is plain text, meaning a
    /// paste should stay a paste. Copying a filename must not silently become
    /// an attachment.
    public static func pasteboardPrefersText(_ pasteboard: NSPasteboard) -> Bool {
        guard let types = pasteboard.types else { return false }
        let imageTypes: Set<NSPasteboard.PasteboardType> = [.png, .tiff, .fileURL]
        return types.contains(.string) && !types.contains(where: { imageTypes.contains($0) })
    }

    public static func payload(fromFileAt url: URL) throws -> ChatAttachmentPayload {
        guard isProbablyImage(url) else {
            throw ChatAttachmentIntakeError.unsupportedType
        }
        // Check the size before reading. Nothing stops someone dropping a
        // gigapixel TIFF, and `Data(contentsOf:)` would pull all of it into
        // memory before we ever got to reject it.
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        guard size <= maximumSourceByteCount else {
            throw ChatAttachmentIntakeError.tooLarge
        }
        let data: Data
        do {
            data = try Data(contentsOf: url, options: .mappedIfSafe)
        } catch {
            throw ChatAttachmentIntakeError.decodeFailed
        }
        return try normalize(data)
    }

    // MARK: - Normalization

    /// Decide whether the bytes can go as-is, and re-encode them when they
    /// can't.
    ///
    /// Passing acceptable bytes through untouched matters: re-encoding a 300 KB
    /// JPEG screenshot as PNG can quadruple it for no visible gain.
    public static func normalize(_ data: Data) throws -> ChatAttachmentPayload {
        guard data.count <= maximumSourceByteCount else {
            throw ChatAttachmentIntakeError.tooLarge
        }
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              CGImageSourceGetCount(source) > 0
        else {
            throw ChatAttachmentIntakeError.decodeFailed
        }
        let dimensions = try pixelDimensions(of: source)
        let sniffed = sniffMIMEType(data)

        let withinLongEdge = max(dimensions.width, dimensions.height) <= maximumLongEdge
        if let sniffed, acceptedMIMETypes.contains(sniffed), withinLongEdge,
           data.count <= maximumByteCount {
            return ChatAttachmentPayload(
                data: data,
                mimeType: sniffed,
                pixelWidth: dimensions.width,
                pixelHeight: dimensions.height
            )
        }

        // Everything else — TIFF off the pasteboard, an oversized screenshot, a
        // format Core won't take — becomes a downscaled PNG. An animated GIF
        // that lands here loses its animation; a still frame the model can
        // actually see beats a rejection.
        return try encodePNG(from: source)
    }

    /// Re-encode at or below the long-edge ceiling. ImageIO does the resampling
    /// and never allocates the full-size bitmap when the source is larger.
    private static func encodePNG(from source: CGImageSource) throws -> ChatAttachmentPayload {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maximumLongEdge
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
        else {
            throw ChatAttachmentIntakeError.decodeFailed
        }
        let encoded = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            encoded as CFMutableData,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else {
            throw ChatAttachmentIntakeError.encodeFailed
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw ChatAttachmentIntakeError.encodeFailed
        }
        let data = encoded as Data
        guard data.count <= maximumByteCount else {
            throw ChatAttachmentIntakeError.tooLarge
        }
        return ChatAttachmentPayload(
            data: data,
            mimeType: "image/png",
            pixelWidth: image.width,
            pixelHeight: image.height
        )
    }

    private static func pixelDimensions(
        of source: CGImageSource
    ) throws -> (width: Int, height: Int) {
        guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil)
            as? [CFString: Any],
            let width = properties[kCGImagePropertyPixelWidth] as? Int,
            let height = properties[kCGImagePropertyPixelHeight] as? Int,
            width > 0, height > 0
        else {
            throw ChatAttachmentIntakeError.decodeFailed
        }
        return (width, height)
    }

    /// Identify an image by its magic bytes, matching Core's sniffer exactly.
    /// Disagreeing with Core here would mean an upload Core then rejects.
    public static func sniffMIMEType(_ data: Data) -> String? {
        let bytes = [UInt8](data.prefix(12))
        if bytes.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
            return "image/png"
        }
        if bytes.starts(with: [0xFF, 0xD8, 0xFF]) {
            return "image/jpeg"
        }
        if bytes.starts(with: Array("GIF87a".utf8)) || bytes.starts(with: Array("GIF89a".utf8)) {
            return "image/gif"
        }
        if bytes.count >= 12,
           Array(bytes[0..<4]) == Array("RIFF".utf8),
           Array(bytes[8..<12]) == Array("WEBP".utf8) {
            return "image/webp"
        }
        return nil
    }

    private static func isProbablyImage(_ url: URL) -> Bool {
        guard let type = UTType(filenameExtension: url.pathExtension.lowercased()) else {
            return false
        }
        return acceptedContentTypes.contains { type.conforms(to: $0) }
    }

    // MARK: - Handoff

    /// Write the payload where Core can copy it from. Image bytes cannot cross
    /// JSONL — the frame cap is 1 MiB on both sides — so the RPC carries a path
    /// and Core reads the file itself.
    ///
    /// The caller deletes this in a `defer`; Core never touches it after
    /// copying.
    public static func writeTemporaryFile(_ payload: ChatAttachmentPayload) throws -> URL {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
            .appendingPathComponent("ottam-attachments", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        let url = directory.appendingPathComponent(
            "\(UUID().uuidString).\(fileExtension(for: payload.mimeType))"
        )
        try payload.data.write(to: url, options: .atomic)
        return url
    }

    private static func fileExtension(for mimeType: String) -> String {
        switch mimeType {
        case "image/jpeg": return "jpg"
        case "image/gif": return "gif"
        case "image/webp": return "webp"
        default: return "png"
        }
    }
}
