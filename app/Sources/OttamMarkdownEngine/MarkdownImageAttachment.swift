import AppKit

/// Inline image in engine output. Shows a compact placeholder glyph the instant
/// it is styled; `MarkdownImageLoader` swaps in the real
/// bitmap out-of-band. The swap changes only the attachment payload, so a
/// consumer can invalidate its local layout without rebuilding source text.
public final class MarkdownImageAttachment: NSTextAttachment {
    public let url: URL
    public let altText: String
    /// Pixel size of the loaded image; nil while the placeholder shows.
    public private(set) var imageSize: CGSize?
    /// Whether the load failed terminally. `imageSize` stays nil because no
    /// bitmap loaded; instance sizing collapses the provisional image box to a
    /// compact failure glyph.
    public private(set) var loadFailed = false

    public static let placeholderSize = CGSize(width: 20, height: 18)
    public static let failureSize = CGSize(width: 20, height: 18)
    public static let maxDisplayHeight: CGFloat = 320

    public init(url: URL, altText: String = "") {
        self.url = url
        self.altText = altText
        super.init(data: nil, ofType: nil)
        image = NSImage(
            systemSymbolName: "photo",
            accessibilityDescription: altText.isEmpty ? "image" : altText
        )
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    public func finishLoading(image: NSImage, size: CGSize) {
        self.image = image
        imageSize = size
    }

    /// Marks the load terminally failed and swaps in a broken-image glyph.
    /// Returns whether anything changed, so a caller can skip a pointless layout
    /// invalidation.
    @discardableResult
    public func markLoadFailed() -> Bool {
        guard !loadFailed else { return false }
        loadFailed = true
        image = NSImage(
            systemSymbolName: "photo.badge.exclamationmark",
            accessibilityDescription: altText.isEmpty
                ? "image failed to load"
                : "\(altText) — image failed to load"
        )
        return true
    }

    /// Resolves a schemeless destination to a file inside `root`, the
    /// conversation's working root. Links and inline file references use it;
    /// images use `resolveImage`, which is not held to the root.
    ///
    /// Agents write plain filesystem paths — `assets/logo.png`, or an absolute
    /// `/Users/…/logo.png`. `URL(string:)` gives *both* a nil scheme (only a
    /// literal `file://` prefix produces one). A schemeless destination
    /// resolves against `root` and is admitted only if it stays inside it.
    /// Symlinks are resolved on both sides before the containment test, so a
    /// link inside the root cannot point out of it.
    ///
    /// Anything already carrying a scheme passes through untouched. Without a
    /// root, or on an escape, the destination is returned unchanged.
    ///
    /// A root relative to another URL is a folder inside a boundary, as a
    /// file tab passes `docs/specs/` relative to the repository: relative
    /// paths start in the folder, as they do on GitHub, and every read stays
    /// inside the repository. A plain root is both.
    public static func resolve(_ url: URL, relativeTo root: URL?) -> URL {
        guard url.scheme == nil, let root else { return url }
        // `path` percent-decodes, so `assets/my%20logo.png` finds the file.
        let path = url.path
        guard !path.isEmpty else { return url }
        let base = canonicalPath(of: root)
        let candidate = realPath(
            path.hasPrefix("/")
                ? URL(fileURLWithPath: path)
                : URL(fileURLWithPath: path, relativeTo: URL(fileURLWithPath: folder(of: root), isDirectory: true))
        )
        guard contains(candidate.path, base: base) else { return url }
        return candidate
    }

    /// Resolves an image destination. Unlike a link, an image is not held to
    /// `root`: rendering one only draws pixels on this Mac, and an agent that
    /// saves a chart to `/tmp` should see it. `MarkdownImageLoader` guards the
    /// read itself (regular files on a local volume, bounded size).
    ///
    /// Relative paths start in `root`'s folder; absolute and `~/` paths stand
    /// alone. Resolution is lexical: touching the disk here could trigger an
    /// automount from styling, on whatever thread styles.
    public static func resolveImage(_ url: URL, relativeTo root: URL?) -> URL {
        guard url.scheme == nil else { return url }
        let path = url.path
        guard !path.isEmpty else { return url }
        if path.hasPrefix("/") || path.hasPrefix("~/") {
            return URL(fileURLWithPath: (path as NSString).expandingTildeInPath).standardizedFileURL
        }
        guard let root else { return url }
        return URL(fileURLWithPath: path, relativeTo: URL(fileURLWithPath: folder(of: root), isDirectory: true))
            .standardizedFileURL
    }

    /// Where relative paths start: a relative root's folder, or the root.
    private static func folder(of root: URL) -> String {
        root.baseURL == nil ? canonicalPath(of: root) : root.absoluteURL.standardizedFileURL.resolvingSymlinksInPath().path
    }

    /// Whether `url` names a file at or inside `root`, both sides resolved
    /// through their symlinks first. A test on names, true only of the
    /// instant it runs.
    public static func isContained(_ url: URL, in root: URL) -> Bool {
        contains(realPath(url).path, base: canonicalPath(of: root))
    }

    /// The boundary: a relative root's base, or the root itself.
    private static func canonicalPath(of root: URL) -> String {
        (root.baseURL ?? root).standardizedFileURL.resolvingSymlinksInPath().path
    }

    /// Separator-anchored, so `/root-2/x` is not read as living in `/root`.
    private static func contains(_ path: String, base: String) -> Bool {
        path == base || path.hasPrefix(base + "/")
    }

    /// `resolvingSymlinksInPath` on its own is not enough to decide containment:
    /// it leaves a path whose leaf does not exist *entirely* unresolved, so a
    /// link inside the root pointing out of it (`root/link/secret.png`, `link`
    /// → elsewhere) came back still spelled `root/…` and passed the test.
    ///
    /// So resolve the deepest ancestor that does exist — which is where the
    /// links actually live — and re-append what hangs off it. Costs one
    /// `fileExists` per missing component, on a path walked only for schemeless
    /// image destinations.
    private static func realPath(_ url: URL) -> URL {
        let manager = FileManager.default
        var missing: [String] = []
        var existing = url.standardizedFileURL
        while !manager.fileExists(atPath: existing.path) {
            let parent = existing.deletingLastPathComponent().standardizedFileURL
            guard parent.path != existing.path else { return url.standardizedFileURL }
            missing.append(existing.lastPathComponent)
            existing = parent
        }
        var resolved = existing.resolvingSymlinksInPath()
        for component in missing.reversed() { resolved.appendPathComponent(component) }
        return resolved.standardizedFileURL
    }

    /// Aspect-fit into the container width with a display-height cap; unresolved
    /// or degenerate inputs use the compact placeholder footprint. Pure so the
    /// sizing rules are testable without layout.
    public static func displaySize(imageSize: CGSize?, containerWidth: CGFloat) -> CGSize {
        let availableWidth = containerWidth > 0
            ? containerWidth
            : placeholderSize.width
        guard let imageSize, imageSize.width > 0, imageSize.height > 0 else {
            let width = min(placeholderSize.width, availableWidth)
            return CGSize(
                width: width,
                height: width * (placeholderSize.height / placeholderSize.width)
            )
        }
        // Never upscale: small images render at native size.
        var width = min(imageSize.width, availableWidth)
        var height = width * (imageSize.height / imageSize.width)
        if height > maxDisplayHeight {
            height = maxDisplayHeight
            width = height * (imageSize.width / imageSize.height)
        }
        return CGSize(width: width, height: height)
    }

    /// State-aware sizing used by both TextKit and the transcript's Core Text
    /// bridge. Loading and terminal-failure symbols share a compact footprint;
    /// only a successfully decoded image expands into image-sized content.
    public func displaySize(containerWidth: CGFloat) -> CGSize {
        guard loadFailed else {
            return Self.displaySize(imageSize: imageSize, containerWidth: containerWidth)
        }
        let availableWidth = containerWidth > 0 ? containerWidth : Self.failureSize.width
        let width = min(Self.failureSize.width, availableWidth)
        return CGSize(
            width: width,
            height: Self.failureSize.height * (width / Self.failureSize.width)
        )
    }

    public override func attachmentBounds(
        for attributes: [NSAttributedString.Key: Any],
        location: NSTextLocation,
        textContainer: NSTextContainer?,
        proposedLineFragment: CGRect,
        position: CGPoint
    ) -> CGRect {
        CGRect(
            origin: .zero,
            size: displaySize(containerWidth: proposedLineFragment.width)
        )
    }
}

/// Loads attachment images off the main thread and hands each finished
/// attachment back on main for a targeted layout invalidation. Decoded bitmaps
/// are cached by URL so restyled (recreated) attachments fill synchronously.
@MainActor
public final class MarkdownImageLoader {
    private typealias Pending = (
        attachment: MarkdownImageAttachment,
        completion: @MainActor (MarkdownImageAttachment) -> Void
    )

    private var cache: [URL: (image: NSImage, size: CGSize)] = [:]
    /// Insertion order for eviction; agent-authored markdown can reference an
    /// unbounded number of images, and decoded bitmaps are too big to keep all.
    private var cacheOrder: [URL] = []
    private static let maxCachedImages = 32
    nonisolated private static let maxImageBytes = 32 << 20
    private static let allowedSchemes: Set<String> = ["file", "http", "https", "data"]
    private var pending: [URL: [Pending]] = [:]
    /// URLs whose load failed, for the loader's lifetime: a failed image sits
    /// in a streaming trailing block that restyles every chunk, so without
    /// this the fetch re-fires per chunk forever.
    private var failed: Set<URL> = []

    public init() {}

    /// Starts (or reuses) a load for the attachment; `completion` runs on main
    /// once its image is set — layout invalidation belongs to the caller.
    public func loadIfNeeded(
        _ attachment: MarkdownImageAttachment,
        completion: @escaping @MainActor (MarkdownImageAttachment) -> Void
    ) {
        guard attachment.imageSize == nil, !attachment.loadFailed else { return }
        if let cached = cache[attachment.url] {
            attachment.finishLoading(image: cached.image, size: cached.size)
            completion(attachment)
            return
        }
        let url = attachment.url
        if failed.contains(url) {
            if attachment.markLoadFailed() { completion(attachment) }
            return
        }
        guard Self.allowedSchemes.contains(url.scheme?.lowercased() ?? ""),
              Self.isLocalReadAllowed(url)
        else {
            failed.insert(url)
            if attachment.markLoadFailed() { completion(attachment) }
            return
        }
        let loadAlreadyRunning = pending[url] != nil
        pending[url, default: []].append((attachment, completion))
        guard !loadAlreadyRunning else { return }
        Task.detached(priority: .utility) {
            let loaded: (NSImage, CGSize)?
            if let data = Self.contents(of: url),
               let image = NSImage(data: data) {
                loaded = (image, image.size)
            } else {
                loaded = nil
            }
            await MainActor.run { [weak self] in
                self?.finish(url: url, loaded: loaded)
            }
        }
    }

    /// A file URL naming a host is a network read, and is refused.
    nonisolated static func isLocalReadAllowed(_ url: URL) -> Bool {
        guard url.isFileURL, let host = url.host, !host.isEmpty else { return true }
        return host == "localhost"
    }

    /// Off-main fetch. Remote and `data:` destinations read directly; a local
    /// one goes through `readLocal`.
    nonisolated private static func contents(of url: URL) -> Data? {
        if url.isFileURL { return readLocal(url.path, automounts: automountPoints()) }
        guard let data = try? Data(contentsOf: url, options: .mappedIfSafe),
              data.count <= maxImageBytes
        else { return nil }
        return data
    }

    /// Reads a local image file from anywhere on this Mac's own disks.
    ///
    /// The path is agent-authored, but what it reads is only drawn on screen,
    /// and the agent can already read the same files itself. The guards are
    /// for what *opening* a path can do:
    /// - looking up a name inside an automount point (`/home`, an NFS or SMB
    ///   map) asks the automounter, which can reach a server. `resolvedPath`
    ///   follows links a component at a time and stops before entering one;
    /// - opening a device or FIFO can block or act, so only a regular file is
    ///   opened, nonblocking and without following a link swapped in since;
    /// - a network volume that is already mounted is refused by its flags;
    /// - the read stops at the size cap, however the file grows.
    nonisolated static func readLocal(_ path: String, automounts: Set<String>) -> Data? {
        guard let resolved = resolvedPath(path, automounts: automounts) else { return nil }
        var info = stat()
        guard lstat(resolved, &info) == 0, info.st_mode & S_IFMT == S_IFREG else { return nil }
        let descriptor = open(resolved, O_RDONLY | O_CLOEXEC | O_NONBLOCK | O_NOFOLLOW)
        guard descriptor >= 0 else { return nil }
        defer { close(descriptor) }
        var volume = statfs()
        guard fstat(descriptor, &info) == 0,
              info.st_mode & S_IFMT == S_IFREG,
              info.st_size <= off_t(maxImageBytes),
              fstatfs(descriptor, &volume) == 0,
              volume.f_flags & UInt32(MNT_LOCAL) != 0,
              let data = try? FileHandle(fileDescriptor: descriptor).read(upToCount: maxImageBytes + 1),
              data.count <= maxImageBytes
        else { return nil }
        return data
    }

    /// `path` with every symlink followed by `lstat` and `readlink`, one
    /// component at a time, so no name is ever looked up inside an automount
    /// point. Nil if the path would enter one, a component is missing, or
    /// links loop.
    nonisolated static func resolvedPath(_ path: String, automounts: Set<String>) -> String? {
        var remaining = Array(path.split(separator: "/").map(String.init).reversed())
        var current = ""
        var links = 0
        while let name = remaining.popLast() {
            if automounts.contains(current.isEmpty ? "/" : current) { return nil }
            if name == "." { continue }
            if name == ".." {
                current = (current as NSString).deletingLastPathComponent
                if current == "/" { current = "" }
                continue
            }
            let next = current + "/" + name
            var info = stat()
            guard lstat(next, &info) == 0 else { return nil }
            guard info.st_mode & S_IFMT == S_IFLNK else {
                current = next
                continue
            }
            links += 1
            guard links <= 32,
                  let target = try? FileManager.default.destinationOfSymbolicLink(atPath: next)
            else { return nil }
            if target.hasPrefix("/") { current = "" }
            remaining.append(contentsOf: target.split(separator: "/").map(String.init).reversed())
        }
        return current.isEmpty ? "/" : current
    }

    /// Where automounted maps are mounted, also under the firmlinked name a
    /// path would use (`/System/Volumes/Data/x` is reached as `/x`).
    nonisolated static func automountPoints() -> Set<String> {
        let count = getfsstat(nil, 0, MNT_NOWAIT)
        guard count > 0 else { return [] }
        var mounts: [statfs] = Array(repeating: statfs(), count: Int(count))
        let size = MemoryLayout.stride(ofValue: mounts[0]) * mounts.count
        let filled = mounts.withUnsafeMutableBufferPointer { buffer in
            getfsstat(buffer.baseAddress, Int32(size), MNT_NOWAIT)
        }
        var points: Set<String> = []
        for mount in mounts.prefix(Int(max(filled, 0))) {
            guard cString(mount.f_fstypename) == "autofs" else { continue }
            let point = cString(mount.f_mntonname)
            points.insert(point)
            let data = "/System/Volumes/Data"
            if point.hasPrefix(data + "/") { points.insert(String(point.dropFirst(data.count))) }
        }
        return points
    }

    nonisolated private static func cString<T>(_ tuple: T) -> String {
        withUnsafeBytes(of: tuple) { String(decoding: $0.prefix { $0 != 0 }, as: UTF8.self) }
    }

    private func finish(url: URL, loaded: (NSImage, CGSize)?) {
        let waiters = pending.removeValue(forKey: url) ?? []
        guard let (image, size) = loaded else {
            // Waiters used to be dropped silently here, so nothing ever redrew
            // and every later append re-kicked the fetch.
            failed.insert(url)
            for (attachment, completion) in waiters where attachment.markLoadFailed() {
                completion(attachment)
            }
            return
        }
        if cache[url] == nil {
            cacheOrder.append(url)
            if cacheOrder.count > Self.maxCachedImages {
                cache.removeValue(forKey: cacheOrder.removeFirst())
            }
        }
        cache[url] = (image, size)
        for (attachment, completion) in waiters {
            if attachment.imageSize == nil {
                attachment.finishLoading(image: image, size: size)
            }
            completion(attachment)
        }
    }
}
