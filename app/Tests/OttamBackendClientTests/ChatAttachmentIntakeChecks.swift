import CoreGraphics
import OttamCore
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Covers the composer's image intake path. The app target has no test target
/// of its own, which is why this logic lives in `OttamCore` — and the
/// TIFF→PNG conversion below is the piece most likely to regress silently,
/// because ⌘⌃⇧4 puts TIFF on the pasteboard and nothing else in the app looks
/// at pasteboard image types.
struct ChatAttachmentIntakeChecks {
    func run() throws {
        try tiffBecomesPNG()
        try oversizedImageIsDownscaled()
        try acceptableBytesPassThroughUntouched()
        try jpegKeepsItsCompression()
        try nonImageBytesAreRejected()
        try sniffMatchesEveryAcceptedType()
        try temporaryFileCarriesTheSniffedExtension()
        try nonImageFileIsRejectedBeforeReading()
        try bothSourceKindsNormalizeIdentically()
        try oversizedDataIsRejectedBeforeDecode()
    }

    private func oversizedDataIsRejectedBeforeDecode() throws {
        let data = Data(
            repeating: 0,
            count: ChatAttachmentIntake.maximumSourceByteCount + 1
        )
        do {
            _ = try ChatAttachmentIntake.normalize(data)
            throw GateFailure("oversized in-memory image data should be rejected")
        } catch let error as ChatAttachmentIntakeError {
            try check(error == .tooLarge, "expected tooLarge, got \(error)")
        }
    }

    /// The paste path hands a `.data` source across a thread hop while the drop
    /// and picker paths hand a `.file`; both must land on the same payload.
    private func bothSourceKindsNormalizeIdentically() throws {
        let png = try encode(width: 240, height: 160, as: .png)
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-intake-\(UUID().uuidString).png")
        try png.write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }

        let fromData = try ChatAttachmentIntake.payload(for: .data(png))
        let fromFile = try ChatAttachmentIntake.payload(for: .file(url))
        try check(fromData == fromFile, "a pasted and a picked copy of one image should agree")
        try check(fromFile.pixelWidth == 240, "dimensions should survive the file path")
    }

    private func tiffBecomesPNG() throws {
        let tiff = try encode(width: 120, height: 90, as: .tiff)
        try check(ChatAttachmentIntake.sniffMIMEType(tiff) == nil, "TIFF must not sniff as an accepted type")
        let payload = try ChatAttachmentIntake.normalize(tiff)
        try check(payload.mimeType == "image/png", "TIFF should normalize to PNG, got \(payload.mimeType)")
        try check(
            ChatAttachmentIntake.sniffMIMEType(payload.data) == "image/png",
            "normalized TIFF bytes should carry a PNG signature"
        )
        try check(
            payload.pixelWidth == 120 && payload.pixelHeight == 90,
            "TIFF conversion should preserve dimensions, got \(payload.pixelWidth)x\(payload.pixelHeight)"
        )
    }

    private func oversizedImageIsDownscaled() throws {
        let png = try encode(width: 3000, height: 1500, as: .png)
        let payload = try ChatAttachmentIntake.normalize(png)
        try check(payload.mimeType == "image/png", "downscale should stay PNG")
        try check(
            payload.pixelWidth == ChatAttachmentIntake.maximumLongEdge,
            "long edge should clamp to \(ChatAttachmentIntake.maximumLongEdge), got \(payload.pixelWidth)"
        )
        try check(
            payload.pixelHeight == ChatAttachmentIntake.maximumLongEdge / 2,
            "aspect ratio should survive downscaling, got \(payload.pixelHeight)"
        )
    }

    private func acceptableBytesPassThroughUntouched() throws {
        let png = try encode(width: 200, height: 200, as: .png)
        let payload = try ChatAttachmentIntake.normalize(png)
        try check(payload.data == png, "an already-acceptable PNG should not be re-encoded")
        try check(payload.pixelWidth == 200 && payload.pixelHeight == 200, "dimensions should be read from the source")
    }

    private func jpegKeepsItsCompression() throws {
        let jpeg = try encode(width: 400, height: 300, as: .jpeg)
        let payload = try ChatAttachmentIntake.normalize(jpeg)
        try check(payload.mimeType == "image/jpeg", "a small JPEG should stay JPEG, got \(payload.mimeType)")
        try check(payload.data == jpeg, "a small JPEG should pass through byte for byte")
    }

    private func nonImageBytesAreRejected() throws {
        let text = Data("not an image, just some bytes".utf8)
        do {
            _ = try ChatAttachmentIntake.normalize(text)
            throw GateFailure("normalize should reject non-image bytes")
        } catch let error as ChatAttachmentIntakeError {
            try check(error == .decodeFailed, "expected decodeFailed, got \(error)")
        }
    }

    private func sniffMatchesEveryAcceptedType() throws {
        let cases: [(String, [UInt8])] = [
            ("image/png", [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            ("image/jpeg", [0xFF, 0xD8, 0xFF, 0xE0]),
            ("image/gif", Array("GIF89a".utf8)),
            ("image/webp", Array("RIFF".utf8) + [0, 0, 0, 0] + Array("WEBP".utf8))
        ]
        for (expected, bytes) in cases {
            let sniffed = ChatAttachmentIntake.sniffMIMEType(Data(bytes + [UInt8](repeating: 0, count: 8)))
            try check(sniffed == expected, "expected \(expected), sniffed \(sniffed ?? "nil")")
            try check(
                ChatAttachmentIntake.acceptedMIMETypes.contains(expected),
                "\(expected) should be an accepted type"
            )
        }
        try check(ChatAttachmentIntake.sniffMIMEType(Data([0x00, 0x01, 0x02])) == nil, "garbage should not sniff")
        try check(ChatAttachmentIntake.sniffMIMEType(Data()) == nil, "empty data should not sniff")
    }

    private func temporaryFileCarriesTheSniffedExtension() throws {
        let jpeg = try encode(width: 32, height: 32, as: .jpeg)
        let payload = try ChatAttachmentIntake.normalize(jpeg)
        let url = try ChatAttachmentIntake.writeTemporaryFile(payload)
        defer { try? FileManager.default.removeItem(at: url) }
        try check(url.pathExtension == "jpg", "JPEG should land as .jpg, got .\(url.pathExtension)")
        try check(
            (try? Data(contentsOf: url)) == payload.data,
            "the temp file should hold exactly the payload bytes Core will copy"
        )
    }

    private func nonImageFileIsRejectedBeforeReading() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-intake-\(UUID().uuidString).txt")
        try Data("hello".utf8).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        do {
            _ = try ChatAttachmentIntake.payload(fromFileAt: url)
            throw GateFailure("a .txt file should not be attachable")
        } catch let error as ChatAttachmentIntakeError {
            try check(error == .unsupportedType, "expected unsupportedType, got \(error)")
        }
    }

    // MARK: - Helpers

    /// Build a real encoded image so the checks exercise ImageIO the same way
    /// the pasteboard path does, rather than asserting against handcrafted
    /// headers.
    private func encode(width: Int, height: Int, as type: UTType) throws -> Data {
        guard let context = CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else {
            throw GateFailure("could not create a \(width)x\(height) bitmap context")
        }
        context.setFillColor(CGColor(red: 0.2, green: 0.5, blue: 0.9, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        guard let image = context.makeImage() else {
            throw GateFailure("could not render the test bitmap")
        }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            data as CFMutableData,
            type.identifier as CFString,
            1,
            nil
        ) else {
            throw GateFailure("could not create a \(type.identifier) destination")
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw GateFailure("could not encode \(type.identifier)")
        }
        return data as Data
    }

    private func check(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        guard condition() else {
            throw GateFailure(message)
        }
    }
}

private struct GateFailure: Error, CustomStringConvertible {
    var description: String

    init(_ description: String) {
        self.description = description
    }
}
