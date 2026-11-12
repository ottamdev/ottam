import AppKit
import Foundation
import Testing
@testable import OttamMarkdownEngine

/// Resolving a markdown image destination against the conversation's working
/// root. Agents write bare filesystem paths, which carry no URL scheme and so
/// were rejected outright before the loader ever looked at the disk.
@Suite("Markdown image resolution")
struct MarkdownImageResolutionTests {
    private static func makeTheme() -> MarkdownTheme {
        MarkdownTheme(
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
            signature: "test-theme"
        )
    }

    /// A real directory, so the symlink resolution both sides run agrees.
    /// `/var` is a symlink to `/private/var` on macOS, and a root built from a
    /// string that has not been through the filesystem would fail containment
    /// against a candidate that has.
    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-resolve-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    @Test("a relative path resolves under the root")
    func relativePath() throws {
        let root = try makeRoot()
        let resolved = MarkdownImageAttachment.resolve(
            URL(string: "assets/brand/png/icon.png")!, relativeTo: root
        )
        #expect(resolved.isFileURL)
        #expect(resolved.path == root.resolvingSymlinksInPath().path + "/assets/brand/png/icon.png")
    }

    /// The exact shape that failed in the field: an agent asked to show a logo
    /// emitted the absolute repo path, which `URL(string:)` parses with a nil
    /// scheme just like a relative one.
    @Test("an absolute path inside the root resolves")
    func absolutePathInsideRoot() throws {
        let root = try makeRoot()
        let inside = root.resolvingSymlinksInPath().path + "/assets/brand/png/ottam-icon-512.png"
        let resolved = MarkdownImageAttachment.resolve(URL(string: inside)!, relativeTo: root)
        #expect(resolved.isFileURL)
        #expect(resolved.path == inside)
    }

    @Test("percent-encoded spaces decode into the file path")
    func percentEncoding() throws {
        let root = try makeRoot()
        let resolved = MarkdownImageAttachment.resolve(
            URL(string: "assets/my%20logo.png")!, relativeTo: root
        )
        #expect(resolved.path == root.resolvingSymlinksInPath().path + "/assets/my logo.png")
    }

    @Test(
        "a path that escapes the root is left unresolved",
        arguments: [
            "../../../etc/passwd",
            "/etc/passwd",
            "assets/../../secrets.png",
            "~/Documents/secret.png",
        ]
    )
    func escapesAreRefused(path: String) throws {
        let root = try makeRoot()
        let url = URL(string: path)!
        // Unchanged, so the loader's scheme allowlist turns it away exactly as
        // it did before there was a root at all: broken glyph, no disk access.
        #expect(MarkdownImageAttachment.resolve(url, relativeTo: root) == url)
    }

    /// A symlink inside the root pointing out of it must not become a way
    /// through: containment is tested after both sides resolve.
    @Test("a symlink out of the root does not escape through it")
    func symlinkEscape() throws {
        let root = try makeRoot()
        let outside = try makeRoot()
        try FileManager.default.createSymbolicLink(
            at: root.appendingPathComponent("link"), withDestinationURL: outside
        )
        let url = URL(string: "link/secret.png")!
        #expect(MarkdownImageAttachment.resolve(url, relativeTo: root) == url)
    }

    @Test(
        "a destination with a scheme passes through untouched",
        arguments: [
            "https://example.com/x.png",
            "http://example.com/x.png",
            "file:///tmp/x.png",
            "data:image/png;base64,iVBORw0KGgo=",
            // Judged by the loader's allowlist, not here.
            "ftp://example.com/x.png",
        ]
    )
    func schemesPassThrough(spelling: String) throws {
        let root = try makeRoot()
        let url = URL(string: spelling)!
        #expect(MarkdownImageAttachment.resolve(url, relativeTo: root) == url)
    }

    @Test("without a root a bare path is left unresolved")
    func noRoot() {
        let url = URL(string: "assets/x.png")!
        #expect(MarkdownImageAttachment.resolve(url, relativeTo: nil) == url)
    }

    @Test("the styler resolves image destinations through its resource root")
    func stylerResolves() throws {
        let root = try makeRoot()
        let styled = MarkdownStyler(theme: Self.makeTheme(), resourceRoot: root)
            .attributedString(from: "![logo](assets/brand/icon.png)")
        var found: MarkdownImageAttachment?
        styled.enumerateAttribute(
            .attachment, in: NSRange(location: 0, length: styled.length)
        ) { value, _, _ in
            if let image = value as? MarkdownImageAttachment { found = image }
        }
        let attachment = try #require(found, "no image attachment was styled")
        #expect(attachment.url.isFileURL)
        #expect(attachment.url.path == root.resolvingSymlinksInPath().path + "/assets/brand/icon.png")
        #expect(attachment.altText == "logo")
    }

    /// End to end: the field failure, through the loader, off real bytes.
    @Test("a bare absolute path now loads its image")
    @MainActor
    func barePathLoads() async throws {
        let root = try makeRoot()
        let file = root.appendingPathComponent("icon.png")
        let bitmap = try #require(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 40, pixelsHigh: 20,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        ))
        try #require(bitmap.representation(using: .png, properties: [:])).write(to: file)

        // Exactly what the agent wrote: an absolute path, no `file://`.
        let bare = try #require(URL(string: file.resolvingSymlinksInPath().path))
        #expect(bare.scheme == nil, "the destination under test must be schemeless")

        let attachment = MarkdownImageAttachment(
            url: MarkdownImageAttachment.resolveImage(bare, relativeTo: root), altText: "icon"
        )
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            MarkdownImageLoader().loadIfNeeded(attachment) { _ in continuation.resume() }
        }
        #expect(!attachment.loadFailed)
        #expect(attachment.imageSize == CGSize(width: 40, height: 20))
    }

    // MARK: - Images are not held to the root

    /// Found in the field: an agent saved a chart to `/tmp` and linked it, and
    /// the transcript showed a broken glyph over a file that was right there.
    @Test("an image outside the root loads")
    @MainActor
    func imageOutsideRootLoads() async throws {
        let root = try makeRoot()
        let file = try makeRoot().appendingPathComponent("chart.png")
        try #require(Self.pngData()).write(to: file)

        let styled = MarkdownStyler(theme: Self.makeTheme(), resourceRoot: root)
            .attributedString(from: "![chart](\(file.path))")
        var found: MarkdownImageAttachment?
        styled.enumerateAttribute(
            .attachment, in: NSRange(location: 0, length: styled.length)
        ) { value, _, _ in
            if let image = value as? MarkdownImageAttachment { found = image }
        }
        let attachment = try #require(found, "no image attachment was styled")
        #expect(attachment.url.isFileURL)
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            MarkdownImageLoader().loadIfNeeded(attachment) { _ in continuation.resume() }
        }
        #expect(!attachment.loadFailed)
        #expect(attachment.imageSize == CGSize(width: 8, height: 8))
    }

    @Test(
        "an image path resolves wherever it points",
        arguments: [
            ("/tmp/spike/bars.png", "/tmp/spike/bars.png"),
            ("../elsewhere/x.png", "/elsewhere/x.png"),
            ("assets/../logo.png", "/repo/logo.png"),
        ]
    )
    func imageResolvesOutsideRoot(spelling: String, expected: String) throws {
        // Relative paths start in the root. `/repo` does not exist, so no
        // symlink resolution moves it.
        let root = URL(fileURLWithPath: "/repo")
        let resolved = MarkdownImageAttachment.resolveImage(URL(string: spelling)!, relativeTo: root)
        #expect(resolved.isFileURL)
        #expect(resolved.path == expected)
    }

    @Test("an image path expands a home-relative spelling")
    func imageExpandsTilde() {
        let resolved = MarkdownImageAttachment.resolveImage(URL(string: "~/Downloads/x.png")!, relativeTo: nil)
        #expect(resolved.path == NSHomeDirectory() + "/Downloads/x.png")
    }

    @Test("without a root an absolute image path still resolves, a relative one does not")
    func imageWithoutRoot() {
        #expect(MarkdownImageAttachment.resolveImage(URL(string: "/tmp/x.png")!, relativeTo: nil).isFileURL)
        let relative = URL(string: "assets/x.png")!
        #expect(MarkdownImageAttachment.resolveImage(relative, relativeTo: nil) == relative)
    }

    // MARK: - The loader's guards

    @Test("a file URL naming a host is refused")
    func fileURLWithHost() throws {
        let remote = try #require(URL(string: "file://example.com/etc/passwd"))
        #expect(!MarkdownImageLoader.isLocalReadAllowed(remote))
        #expect(MarkdownImageLoader.isLocalReadAllowed(try #require(URL(string: "file://localhost/tmp/x.png"))))
        #expect(MarkdownImageLoader.isLocalReadAllowed(try #require(URL(string: "https://example.com/x.png"))))
    }

    @Test("the read returns a regular file's bytes, through a symlink")
    func readsRegularFile() throws {
        let folder = try makeRoot()
        let file = folder.appendingPathComponent("ok.png")
        try Data("mine".utf8).write(to: file)
        try FileManager.default.createSymbolicLink(
            at: folder.appendingPathComponent("link.png"), withDestinationURL: file
        )
        #expect(MarkdownImageLoader.readLocal(file.path, automounts: []) == Data("mine".utf8))
        #expect(MarkdownImageLoader.readLocal(folder.path + "/link.png", automounts: []) == Data("mine".utf8))
    }

    /// Opening a FIFO for reading blocks until a writer arrives; the load task
    /// would hang forever. A device can block or act on open.
    @Test("the read refuses a FIFO, a device and a directory without opening them")
    func refusesSpecialFiles() throws {
        let folder = try makeRoot()
        let fifo = folder.appendingPathComponent("pipe.png").path
        try #require(mkfifo(fifo, 0o600) == 0)
        #expect(MarkdownImageLoader.readLocal(fifo, automounts: []) == nil)
        #expect(MarkdownImageLoader.readLocal("/dev/zero", automounts: []) == nil)
        #expect(MarkdownImageLoader.readLocal(folder.path, automounts: []) == nil)
    }

    @Test("the read refuses a file over the size cap")
    func refusesOversized() throws {
        let file = try makeRoot().appendingPathComponent("huge.png")
        try Data().write(to: file)
        let handle = try FileHandle(forWritingTo: file)
        try handle.truncate(atOffset: UInt64(32 << 20) + 1)
        try handle.close()
        #expect(MarkdownImageLoader.readLocal(file.path, automounts: []) == nil)
    }

    /// Looking up a name inside an automount point can reach a server, so a
    /// path is refused before it enters one, including through a symlink.
    @Test("the read never looks inside an automount point")
    func refusesAutomount() throws {
        let folder = try makeRoot()
        let mount = folder.appendingPathComponent("mount")
        try FileManager.default.createDirectory(at: mount, withIntermediateDirectories: true)
        let file = mount.appendingPathComponent("x.png")
        try Data("remote".utf8).write(to: file)
        try FileManager.default.createSymbolicLink(
            at: folder.appendingPathComponent("link"), withDestinationURL: mount
        )
        // The kernel's name, as getfsstat reports it; Foundation's resolver
        // would strip the `/private` that `/var` leads to.
        let real = try #require(realpath(mount.path, nil).map { path in
            defer { free(path) }
            return String(cString: path)
        })
        #expect(MarkdownImageLoader.readLocal(file.path, automounts: [real]) == nil)
        #expect(MarkdownImageLoader.readLocal(folder.path + "/link/x.png", automounts: [real]) == nil)
        #expect(MarkdownImageLoader.readLocal(file.path, automounts: []) == Data("remote".utf8))
    }

    @Test("this Mac's automount points include the home map under both names")
    func automountPointsListed() {
        let points = MarkdownImageLoader.automountPoints()
        if points.contains("/System/Volumes/Data/home") { #expect(points.contains("/home")) }
    }

    private static func pngData() -> Data? {
        NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 8, pixelsHigh: 8,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        )?.representation(using: .png, properties: [:])
    }
}
