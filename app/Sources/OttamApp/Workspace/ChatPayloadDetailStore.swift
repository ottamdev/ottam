import AppKit
import OttamBackendClient
import ImageIO
import Observation
import SwiftUI

/// Owned by the hosted card, never by its transient SwiftUI view. At most one
/// tool is expanded; its input and selected output read independently. Collapse
/// and selection changes invalidate every reply.
@MainActor @Observable
final class ChatPayloadDetailStore {
    typealias PayloadReader = (String, UInt64, Int, UInt64) async throws -> ConversationReadTranscriptPayloadResult
    typealias BodyReader = (String, UInt64, UInt64) async throws -> ConversationReadTranscriptBodyResult
    var expandedID: String?
    var section = "output"
    var part = 0
    private(set) var data = Data()
    private(set) var image: NSImage?
    private(set) var loading = false
    private(set) var failure: String?
    private(set) var unsaved = false
    private(set) var nextOffset: UInt64?
    private(set) var startOffset: UInt64 = 0
    var followsEnd = true
    private(set) var input: ChatPayloadDetailStore?
    @ObservationIgnored private let includesInput: Bool
    @ObservationIgnored var onChange: () -> Void = {}
    @ObservationIgnored var waitForRefresh: () async throws -> Void = { try await Task.sleep(for: .seconds(2)) }
    @ObservationIgnored private var payloadReader: PayloadReader?
    @ObservationIgnored private var bodyReader: BodyReader?
    @ObservationIgnored private var activities: [ChatLiveActivity] = []
    @ObservationIgnored private var request: Task<Void, Never>?
    @ObservationIgnored private var timer: Task<Void, Never>?
    @ObservationIgnored private var epoch = UUID()
    @ObservationIgnored private var loadedRevision: UInt64?
    /// The revision the in-flight read asked for.
    @ObservationIgnored private var requestedRevision: UInt64?
    @ObservationIgnored private var loadedLength: UInt64 = 0
    /// Least time between reads that follow a streaming revision. A reasoning
    /// row bumps its revision per chunk; reading each one re-fetched and
    /// re-laid out the whole text many times a second.
    @ObservationIgnored var catchUpInterval: Duration = .milliseconds(250)
    @ObservationIgnored private var lastLoadStart: ContinuousClock.Instant?
    @ObservationIgnored private var scheduledRefresh: Task<Void, Never>?

    init(includesInput: Bool = true) { self.includesInput = includesInput }
    deinit { request?.cancel(); timer?.cancel(); scheduledRefresh?.cancel() }
    var activity: ChatLiveActivity? { activities.first { $0.id == expandedID } }
    var payload: TranscriptPayload? { activity?.payloads.first { $0.role == section } }
    var text: String {
        // Byte ranges can begin/end within a UTF-8 scalar. Keep the original
        // bytes for the next page; render only the complete boundary scalars.
        let bytes = Array(data)
        var start = 0
        while start < bytes.count, bytes[start] & 0xc0 == 0x80 { start += 1 }
        var end = bytes.count
        if end > start {
            var lead = end - 1
            while lead > start, bytes[lead] & 0xc0 == 0x80 { lead -= 1 }
            let width = bytes[lead] < 0x80 ? 1 : bytes[lead] < 0xe0 ? 2 : bytes[lead] < 0xf0 ? 3 : 4
            if end - lead < width { end = lead }
        }
        return String(decoding: bytes[start..<end], as: UTF8.self)
    }
    var isBody: Bool { section == "body" }
    var hasDiff: Bool {
        guard section == "fileChangeInput", let parts = payload?.parts, parts.count == 2 else { return false }
        return Set(parts.map(\.name)) == ["Before", "After"] && parts.reduce(UInt64(0), { $0 + $1.byteCount }) <= 256 * 1024
    }
    private enum ContentLayoutSignature: Hashable {
        case text(String)
        case capped
    }

    /// The child made by `configureInput` is the command/arguments reader;
    /// the parent presents reasoning or tool output.
    private var maximumLines: Int {
        includesInput ? ChatPayloadTextView.outputMaximumLines : ChatPayloadTextView.inputMaximumLines
    }
    private var presentedText: String {
        includesInput ? text : ChatToolInputPresentation(text: text, kind: activity?.kind).text
    }

    /// Everything `ChatPayloadDetailView` sizes itself by. Text that grows
    /// under the height cap changes it; text past the cap does not. The
    /// spinner and Unsaved label sit inside the header row.
    var layoutSignature: AnyHashable {
        var lines = 0
        if !data.isEmpty {
            lines = 1
            for byte in data where byte == UInt8(ascii: "\n") {
                lines += 1
                if lines > maximumLines { break }
            }
        }
        let content = image == nil && lines <= maximumLines
            ? ContentLayoutSignature.text(presentedText)
            : .capped
        return makeLayoutSignature(content: content, input: input?.layoutSignature)
    }

    /// A width supplied by card measurement can be an upper bound on the
    /// text view's width. Content capped at that width is certainly capped in
    /// the narrower view; otherwise carrying the full text conservatively
    /// invalidates measurement for any wrapping change.
    func layoutSignature(forWidth width: CGFloat, font: NSFont) -> AnyHashable {
        let text = presentedText
        let cap = ChatPayloadTextView.height(forLines: maximumLines, font: font)
        let content = image == nil && !text.isEmpty
            && ChatPayloadTextView.height(for: text, width: width, font: font, maximumLines: maximumLines) >= cap
            ? ContentLayoutSignature.capped
            : .text(text)
        return makeLayoutSignature(content: content, input: input?.layoutSignature(forWidth: width, font: font))
    }

    private func makeLayoutSignature(content: ContentLayoutSignature, input: AnyHashable?) -> AnyHashable {
        AnyHashable([
            AnyHashable(expandedID ?? ""),
            AnyHashable(section),
            AnyHashable(part),
            AnyHashable(sections.count),
            AnyHashable(payload?.parts.count ?? 0),
            AnyHashable(payload?.upstreamTruncated == true),
            AnyHashable(image != nil),
            AnyHashable(failure ?? ""),
            AnyHashable(startOffset > 0),
            AnyHashable(nextOffset != nil),
            AnyHashable(content),
            AnyHashable(input)
        ])
    }
    var sections: [(String, String)] {
        guard let activity else { return [] }
        var result = activity.payloads.map { ($0.role, $0.role == "fileChangeInput" ? "Requested changes" : "Output") }
        if activity.kind == "think" { result = [("body", "Reasoning")] }
        return result
    }

    func configure(_ activities: [ChatLiveActivity], readPayload: PayloadReader?, readBody: BodyReader?) {
        let previous = payload
        let previousActivity = activity
        self.activities = activities
        payloadReader = readPayload
        bodyReader = readBody
        guard expandedID != nil else { return }
        guard activity != nil else { close(); return }
        configureInput()
        // A newer revision waits for the read in flight, which catches up
        // when it lands; restarting it on every streamed chunk starved it.
        // A payload that just stopped pending supersedes that read, though:
        // `catchUp` compares revisions and would not see it.
        if loading {
            if !isBody, previous?.availability == "pending", let payload, payload.availability != "pending" {
                invalidate(); refresh()
            }
            return
        }
        if isBody, previousActivity?.revision != activity?.revision || previousActivity?.bodyByteCount != activity?.bodyByteCount { refreshSoon() }
        if payload?.availability != "pending", previous?.availability == "pending" || previous?.revision != payload?.revision { refresh() }
    }

    func toggle(_ activity: ChatLiveActivity, section: String? = nil) {
        if expandedID == activity.id { close(); return }
        close()
        expandedID = activity.id
        self.section = section ?? (activity.kind == "think" ? "body" : activity.payloads.contains(where: { $0.role == "fileChangeInput" }) ? "fileChangeInput" : "output")
        part = hasDiff ? -1 : 0
        followsEnd = true
        configureInput()
        onChange()
        refresh()
        let waitForRefresh = waitForRefresh
        timer = Task { [weak self] in
            while !Task.isCancelled {
                do { try await waitForRefresh() } catch { return }
                guard let self else { return }
                if self.payload?.availability == "pending" || self.unsaved || self.activity?.kind == "think" { self.refresh() }
            }
        }
    }

    func select(_ section: String, part: Int? = nil) {
        self.section = section
        self.part = part ?? (hasDiff ? -1 : 0)
        invalidate()
        loadedRevision = nil
        data = Data()
        image = nil
        refresh()
        onChange()
    }

    func close() {
        input?.close(); input = nil
        invalidate()
        timer?.cancel(); timer = nil
        expandedID = nil
        data = Data(); image = nil; loadedRevision = nil; loadedLength = 0
        nextOffset = nil; startOffset = 0
        onChange()
    }

    private func configureInput() {
        guard includesInput else { return }
        guard let activity, activity.kind != "think", activity.bodyByteCount > 0 else {
            input?.close(); input = nil
            return
        }
        if input == nil {
            input = ChatPayloadDetailStore(includesInput: false)
            input?.catchUpInterval = catchUpInterval
            input?.onChange = { [weak self] in self?.onChange() }
        }
        input?.configure(activities, readPayload: payloadReader, readBody: bodyReader)
        if input?.expandedID != activity.id { input?.toggle(activity, section: "body") }
    }

    /// Copy the complete selected text, not just the bounded on-screen page.
    /// Copying a live result takes a snapshot of its readable length.
    func textForCopy() async throws -> String {
        guard let activity, image == nil, failure == nil else { throw CopyError.unavailable }
        let epoch = epoch
        let payload = payload
        let part = part
        let isBody = isBody
        var bytes = Data()
        if startOffset == 0, nextOffset == nil, !loading {
            bytes = data
        } else {
            var offset: UInt64 = 0
            var length: UInt64?
            repeat {
                let chunk: Data
                let next: UInt64?
                if isBody, let bodyReader {
                    let page = try await bodyReader(activity.id, activity.revision, offset)
                    chunk = Data(page.text.utf8); next = page.nextOffset
                    length = activity.bodyByteCount
                } else if let payload, let id = payload.payloadID, let payloadReader, part >= 0 {
                    let page = try await payloadReader(id, payload.revision, part, offset)
                    guard !["missing", "failed", "unavailable"].contains(page.availability),
                          let decoded = Data(base64Encoded: page.data) else { throw CopyError.unavailable }
                    chunk = decoded; next = page.nextOffset
                    if length == nil { length = page.readableLength }
                } else { throw CopyError.unavailable }
                try Task.checkCancellation()
                guard self.epoch == epoch else { throw CancellationError() }
                guard let length, length <= 16 * 1024 * 1024 else { throw CopyError.tooLarge }
                bytes.append(chunk.prefix(Int(length - min(length, UInt64(bytes.count)))))
                if UInt64(bytes.count) >= length { break }
                guard let next, next > offset, !chunk.isEmpty else { throw CopyError.unavailable }
                offset = next
            } while true
        }
        try Task.checkCancellation()
        guard self.epoch == epoch else { throw CancellationError() }
        let text = String(decoding: bytes, as: UTF8.self)
        return isBody && activity.kind != "think" ? ChatToolInputPresentation(text: text, kind: activity.kind).text : text
    }

    private enum CopyError: LocalizedError {
        case unavailable, tooLarge
        var errorDescription: String? {
            switch self {
            case .unavailable: "The complete text is unavailable. Try again after it finishes loading."
            case .tooLarge: "This content exceeds the 16 MB copy limit. Select a smaller range to copy."
            }
        }
    }

    private func invalidate() {
        epoch = UUID(); request?.cancel(); request = nil; loading = false
        scheduledRefresh?.cancel(); scheduledRefresh = nil
    }

    func refresh() {
        guard !loading, let activity else { return }
        let revision = isBody ? activity.revision : payload?.revision
        let length = isBody ? activity.bodyByteCount : payload?.parts[safe: part]?.byteCount ?? 0
        if loadedRevision == revision, loadedLength == length, failure == nil, !unsaved { return }
        let offset: UInt64
        if loadedRevision != revision { offset = 0 }
        else if followsEnd, unsaved { offset = length > 65536 ? length - 65536 : 0 }
        else { offset = startOffset }
        load(offset: offset, append: false)
    }

    private var currentRevision: UInt64? { isBody ? activity?.revision : payload?.revision }
    /// One follow-up read for a revision that arrived while a read was out.
    private func catchUp() { if currentRevision != loadedRevision { refreshSoon() } }

    private func refreshSoon() {
        guard scheduledRefresh == nil else { return }
        let elapsed = lastLoadStart.map { ContinuousClock.now - $0 } ?? catchUpInterval
        guard elapsed < catchUpInterval else { refresh(); return }
        let epoch = epoch
        let wait = catchUpInterval - elapsed
        scheduledRefresh = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled, let self, self.epoch == epoch else { return }
            self.scheduledRefresh = nil
            self.refresh()
        }
    }

    func nextPage() { guard let nextOffset, !loading else { return }; load(offset: nextOffset, append: true) }
    func previousPage() { guard startOffset > 0, !loading else { return }; load(offset: startOffset > 65536 ? startOffset - 65536 : 0, append: false) }
    func retry() { loadedRevision = nil; refresh() }

    private func load(offset: UInt64, append: Bool) {
        guard let activity else { return }
        let epoch = epoch
        let section = section
        let part = part
        let payload = payload
        let bodyReader = bodyReader
        let payloadReader = payloadReader
        loading = true; failure = nil
        lastLoadStart = .now
        requestedRevision = section == "body" ? activity.revision : payload?.revision
        onChange()
        request = Task { [weak self] in
            do {
                let bytes: Data
                let next: UInt64?
                let unsaved: Bool
                let length: UInt64
                if part == -1, let payload, let id = payload.payloadID, let payloadReader {
                    var values: [String] = []
                    for index in payload.parts.indices.sorted(by: { payload.parts[$0].name == "Before" && payload.parts[$1].name != "Before" }) {
                        var offset: UInt64 = 0
                        var data = Data()
                        repeat {
                            let page = try await payloadReader(id, payload.revision, index, offset)
                            try Task.checkCancellation()
                            guard !["missing", "failed", "unavailable"].contains(page.availability), let chunk = Data(base64Encoded: page.data) else {
                                self?.failed(page.reason ?? "Requested changes are unavailable.", epoch: epoch); return
                            }
                            data.append(chunk)
                            guard data.count <= 256 * 1024 else { throw CancellationError() }
                            guard let next = page.nextOffset, next > offset else { break }
                            offset = next
                        } while true
                        values.append(String(decoding: data, as: UTF8.self))
                    }
                    guard values.count == 2 else { return }
                    bytes = Data(Self.requestedDiff(before: values[0], after: values[1]).utf8)
                    next = nil; unsaved = payload.availability == "pending"; length = payload.byteCount ?? 0
                } else if section == "body" {
                    guard let bodyReader else { throw CancellationError() }
                    let page = try await bodyReader(activity.id, activity.revision, offset)
                    bytes = Data(page.text.utf8); next = page.nextOffset; unsaved = false; length = activity.bodyByteCount
                } else {
                    guard let payload, let id = payload.payloadID, let payloadReader else {
                        self?.failed("This detail was not captured.", epoch: epoch); return
                    }
                    guard !payload.parts.isEmpty else {
                        self?.failed(payload.availability == "pending" ? "Waiting for output…" : "No output was supplied.", epoch: epoch); return
                    }
                    if payload.parts[safe: part]?.format == "unavailable" {
                        self?.failed(Self.reason(payload.reason), epoch: epoch); return
                    }
                    let page = try await payloadReader(id, payload.revision, part, offset)
                    guard !["missing", "failed", "unavailable"].contains(page.availability) else {
                        self?.failed(page.reason ?? "This detail is unavailable.", epoch: epoch); return
                    }
                    bytes = Data(base64Encoded: page.data) ?? Data()
                    next = page.nextOffset; unsaved = page.unsaved; length = page.readableLength
                }
                guard !Task.isCancelled, let self, self.epoch == epoch else { return }
                self.loading = false
                self.unsaved = unsaved
                self.loadedRevision = section == "body" ? activity.revision : payload?.revision
                self.loadedLength = length
                self.nextOffset = next
                if append { self.data.append(bytes) } else { self.data = bytes; self.startOffset = offset }
                // Text is a bounded sliding window. Image reads are bounded by
                // the admission limit and decoded only into a small thumbnail.
                if payload?.parts[safe: part]?.format.hasPrefix("image/") == true {
                    if self.data.count > 16 * 1024 * 1024 { self.failed("Image exceeds the capture limit.", epoch: epoch); return }
                    if next != nil { self.nextPage(); return }
                    let imageData = self.data
                    let thumbnail = await Task.detached(priority: .userInitiated) { Self.thumbnail(imageData) }.value
                    guard !Task.isCancelled, self.epoch == epoch else { return }
                    self.image = thumbnail.map { NSImage(cgImage: $0, size: NSSize(width: $0.width, height: $0.height)) }
                    if self.image == nil { self.failed("The captured image could not be decoded.", epoch: epoch) }
                } else if self.data.count > 512 * 1024 {
                    let removed = self.data.count - 512 * 1024
                    self.data.removeFirst(removed); self.startOffset += UInt64(removed)
                }
                self.onChange()
                self.catchUp()
            } catch {
                guard !Task.isCancelled else { return }
                // Core answers a read of a superseded revision as stale.
                if let self, self.epoch == epoch, self.currentRevision != self.requestedRevision {
                    self.loading = false
                    self.refreshSoon()
                    return
                }
                self?.failed(error.localizedDescription, epoch: epoch)
            }
        }
    }

    private func failed(_ message: String, epoch: UUID) {
        guard self.epoch == epoch else { return }
        loading = false; failure = message; onChange()
    }

    static func reason(_ reason: String?) -> String {
        ChatLiveActivity.captureReason(reason)
    }

    static func requestedDiff(before: String, after: String) -> String {
        let old = before.components(separatedBy: "\n")
        let new = after.components(separatedBy: "\n")
        // Bound diff computation independently of capture size. Large line
        // counts remain inspectable as explicit before/after replacements.
        guard old.count + new.count <= 4000 else {
            return "--- Requested before\n+++ Requested after\n" + old.map { "-" + $0 }.joined(separator: "\n") + "\n" + new.map { "+" + $0 }.joined(separator: "\n")
        }
        let changes = new.difference(from: old)
        var removed = Set<Int>(), inserted = Set<Int>()
        for change in changes {
            switch change {
            case .remove(let offset, _, _): removed.insert(offset)
            case .insert(let offset, _, _): inserted.insert(offset)
            }
        }
        var lines = ["--- Requested before", "+++ Requested after"]
        var left = 0, right = 0
        while left < old.count || right < new.count {
            if left < old.count, removed.contains(left) { lines.append("-" + old[left]); left += 1 }
            else if right < new.count, inserted.contains(right) { lines.append("+" + new[right]); right += 1 }
            else if left < old.count, right < new.count { lines.append(" " + old[left]); left += 1; right += 1 }
            else { break }
        }
        return lines.joined(separator: "\n")
    }

    private nonisolated static func thumbnail(_ data: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              width > 0, height > 0, width <= 32768, height <= 32768,
              Int64(width) * Int64(height) <= 64_000_000,
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceThumbnailMaxPixelSize: 2048,
                kCGImageSourceCreateThumbnailWithTransform: true
              ] as CFDictionary) else { return nil }
        return image
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
