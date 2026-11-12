import AppKit
import OttamMarkdownEngine

/// Agents name files in code spans (`docs/a.md`, `docs/a.md:12`) more often
/// than they link them. A span that names an existing markdown file inside
/// the resource root becomes a link to it, so it opens in a file tab. In a
/// file tab the span is looked up from the file's folder, then from the root,
/// since docs name files both ways.
///
/// Text runs through here on every measure and streamed chunk, so a span
/// must pass a test on its text alone before the disk is asked, and answers
/// are cached for `recheckInterval`. A render keeps the answers it used
/// (`Answers`), so a cached render is rebuilt once a file it named appears
/// or goes away: agents often name a file just before writing it.
final class InlineCodeFileLinks: @unchecked Sendable {
    static let shared = InlineCodeFileLinks()
    static let recheckInterval: TimeInterval = 2
    private static let capacity = 512
    private static let longestPath = 512

    /// The answers a render used, one per candidate span.
    struct Answers: Equatable {
        fileprivate var names: [String] = []
        fileprivate var files: [URL?] = []

        /// Whether the disk still gives these answers.
        func areCurrent(in resourceRoot: URL?, now: Date = Date()) -> Bool {
            guard let resourceRoot, !names.isEmpty else { return true }
            return zip(names, files).allSatisfy {
                InlineCodeFileLinks.shared.file(named: $0, in: resourceRoot, now: now) == $1
            }
        }
    }

    private let lock = NSLock()
    private var answers: [String: (url: URL?, at: Date)] = [:]

    /// `value` with a link on each code span that names a markdown file
    /// under `resourceRoot`. Spans that already link keep their link.
    static func applying(
        to value: NSAttributedString,
        resourceRoot: URL?,
        now: Date = Date()
    ) -> NSAttributedString {
        guard let resourceRoot else { return value }
        let links = candidates(in: value).compactMap { range, name -> (NSRange, URL)? in
            guard value.attribute(.link, at: range.location, effectiveRange: nil) == nil,
                  let url = shared.file(named: name, in: resourceRoot, now: now)
            else { return nil }
            return (range, url)
        }
        guard !links.isEmpty else { return value }
        let result = NSMutableAttributedString(attributedString: value)
        for (range, url) in links {
            result.addAttribute(.link, value: url as NSURL, range: range)
        }
        return result
    }

    /// What each candidate span in `value` names now.
    static func answers(in value: NSAttributedString, resourceRoot: URL?, now: Date = Date()) -> Answers {
        guard let resourceRoot else { return Answers() }
        var answers = Answers()
        for (_, name) in candidates(in: value) {
            answers.names.append(name)
            answers.files.append(shared.file(named: name, in: resourceRoot, now: now))
        }
        return answers
    }

    private static func candidates(in value: NSAttributedString) -> [(NSRange, String)] {
        let string = value.string as NSString
        return TranscriptTextMeasurement.inlineCodeRanges(in: value).compactMap { range in
            let text = string.substring(with: range)
            return candidatePath(text) == nil ? nil : (range, text)
        }
    }

    /// The path a span names when it looks like a markdown file, judged on
    /// the text alone: one word, no scheme, a markdown extension once any
    /// line suffix is off.
    static func candidatePath(_ text: String) -> String? {
        guard !text.isEmpty, text.utf16.count <= longestPath,
              !text.contains(where: \.isWhitespace), !text.contains("://")
        else { return nil }
        let path = FileReference.path(text)
        let pathExtension = (path as NSString).pathExtension.lowercased()
        return MarkdownFileID.extensions.contains(pathExtension) ? path : nil
    }

    func file(named text: String, in root: URL, now: Date = Date()) -> URL? {
        guard let path = Self.candidatePath(text) else { return nil }
        let key = "\(root.absoluteURL.path)\n\(root.baseURL?.path ?? "")\n\(path)"
        lock.lock()
        if let answer = answers[key], now.timeIntervalSince(answer.at) < Self.recheckInterval {
            lock.unlock()
            return answer.url
        }
        lock.unlock()
        let url = Self.existingFile(path, in: root)
        lock.lock()
        if answers.count >= Self.capacity { answers.removeAll(keepingCapacity: true) }
        answers[key] = (url, now)
        lock.unlock()
        return url
    }

    private static func existingFile(_ path: String, in root: URL) -> URL? {
        guard let encoded = path.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed),
              let destination = URL(string: encoded)
        else { return nil }
        // From the folder first, then from the root it sits in.
        for base in [root, root.baseURL].compactMap({ $0 }) {
            let url = MarkdownImageAttachment.resolve(destination, relativeTo: base)
            var isDirectory: ObjCBool = false
            if url.isFileURL, FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory),
               !isDirectory.boolValue {
                return url
            }
        }
        return nil
    }
}
