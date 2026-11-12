import OttamBackendClient
import OttamGhostty
import Foundation
import Observation
import OSLog

struct TerminalOutputSinkToken: Equatable {
    let rawValue: UInt64
}

private struct ByteCappedDataBuffer {
    private var chunks: [Data] = []
    private var startIndex = 0
    private(set) var byteCount = 0

    var isEmpty: Bool {
        startIndex >= chunks.count
    }

    mutating func append(_ data: Data, maxBytes: Int) {
        guard !data.isEmpty else { return }
        chunks.append(data)
        byteCount += data.count
        trim(to: maxBytes)
    }

    func replaySnapshot(_ body: (Data) -> Void) {
        guard !isEmpty else { return }
        for data in chunks[startIndex...] {
            body(data)
        }
    }

    mutating func removeAll(keepingCapacity: Bool = false) {
        chunks.removeAll(keepingCapacity: keepingCapacity)
        startIndex = 0
        byteCount = 0
    }

    private mutating func trim(to maxBytes: Int) {
        while byteCount > maxBytes, startIndex < chunks.count {
            byteCount -= chunks[startIndex].count
            startIndex += 1
        }
        compactIfNeeded()
    }

    private mutating func compactIfNeeded() {
        guard startIndex > 64, startIndex * 2 > chunks.count else { return }
        chunks.removeSubrange(0..<startIndex)
        startIndex = 0
    }
}

@MainActor
@Observable
final class TerminalSessionModel: TerminalSurfaceBacking {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "Terminal"
    )
    private static let outputResyncReplayMaxBytes: UInt64 = 2 * 1024 * 1024
    private static let maxPendingInputBytes = 4 * 1024 * 1024

    /// Carries this terminal's output from core; one per window, not per terminal.
    private weak var events: TerminalWorkspaceModel?
    private weak var backend: DiffReviewModel?
    private var outputSink: ((Data) -> Void)?
    private var outputSinkToken: TerminalOutputSinkToken?
    private var nextOutputSinkToken: UInt64 = 0
    private var outputReplayBuffer = ByteCappedDataBuffer()
    private var pendingOutputBySession: [TerminalSessionID: [TerminalOutputDeltaParams]] = [:]
    private var pendingExitBySession: [TerminalSessionID: TerminalExitedParams] = [:]
    private var pendingResize: (sessionID: TerminalSessionID, rows: UInt16, cols: UInt16)?
    private var resizeTask: Task<Void, Never>?
    private var lastSentResize: (sessionID: TerminalSessionID, rows: UInt16, cols: UInt16)?
    private var pendingInput = Data()
    private var inputFlushTask: Task<Void, Never>?
    private var isFlushingInput = false
    private var lastSequence: UInt64 = 0
    private var outputResyncTask: Task<Void, Never>?
    private var isLiveOutputDiscontinuous = false
    private var outputResyncBufferedDeltas: [TerminalOutputDeltaParams] = []
    private var openGeneration = 0
    private var latestRows: UInt16 = 24
    private var latestCols: UInt16 = 80
    private var hasExited = false
    private(set) var workspaceID: WorkspaceID?
    private(set) var workingRoot: WorkingRootSummary?
    private(set) var sessionSummary: TerminalSessionSummary?
    /// Core's ID for the terminal, which differs from its tab's; set once it
    /// opens.
    private(set) var coreTerminalID: TerminalID?

    var isOpening = false
    var errorText: String?
    var statusText = "Terminal unavailable"

    init(events: TerminalWorkspaceModel) {
        self.events = events
    }

    func configure(
        workspaceID: WorkspaceID,
        workingRoot: WorkingRootSummary,
        backend: DiffReviewModel
    ) {
        self.backend = backend
        if self.workspaceID != workspaceID || self.workingRoot != workingRoot {
            let terminalHadExited = hasExited
            if let sessionID = sessionSummary?.sessionID {
                Task {
                    try? await backend.closeTerminal(sessionID: sessionID)
                }
            }
            pendingOutputBySession.removeAll()
            pendingExitBySession.removeAll()
            resetRuntimeState()
            hasExited = terminalHadExited
        }
        self.workspaceID = workspaceID
        self.workingRoot = workingRoot
        if workingRoot.availability == .available {
            statusText = hasExited
                ? "Terminal exited."
                : "Ready in \(workingRoot.canonicalPath)"
            errorText = nil
        } else {
            statusText = "Working root unavailable"
            errorText = "This Terminal's Project is no longer attached."
        }
    }

    func attachOutputSink(_ sink: @escaping (Data) -> Void) -> TerminalOutputSinkToken {
        nextOutputSinkToken &+= 1
        let token = TerminalOutputSinkToken(rawValue: nextOutputSinkToken)
        outputSinkToken = token
        outputSink = sink
        outputReplayBuffer.replaySnapshot { data in
            sink(data)
        }
        return token
    }

    func detachOutputSink(token: TerminalOutputSinkToken) {
        guard outputSinkToken == token else {
            return
        }
        outputSinkToken = nil
        outputSink = nil
    }

    func startIfNeeded(cwd: String, title: String) {
        guard sessionSummary == nil, !isOpening else {
            return
        }
        guard let workspaceID, let workingRoot, let backend else {
            errorText = "Working root is unavailable."
            statusText = "Terminal unavailable"
            return
        }
        guard workingRoot.availability == .available else {
            errorText = "This Terminal's Project is no longer attached."
            statusText = "Working root unavailable"
            return
        }
        guard !hasExited else {
            return
        }

        isOpening = true
        errorText = nil
        statusText = "Opening terminal..."
        let generation = openGeneration
        let rows = latestRows
        let cols = latestCols

        Task {
            await events?.subscribeIfNeeded(backend: backend)

            do {
                let summary = try await backend.openTerminal(
                    workspaceID: workspaceID,
                    workingRoot: workingRoot.selection,
                    rows: rows,
                    cols: cols,
                    cwd: cwd
                )
                guard openGeneration == generation, self.workspaceID == workspaceID else {
                    try? await backend.closeTerminal(sessionID: summary.sessionID)
                    try? await backend.deleteTerminal(
                        workspaceID: workspaceID,
                        terminalID: summary.terminalID
                    )
                    return
                }
                sessionSummary = summary
                coreTerminalID = summary.terminalID
                isOpening = false
                statusText = "\(summary.shell) in \(summary.cwd)"
                try? await backend.updateTerminalMetadata(
                    workspaceID: workspaceID,
                    terminalID: summary.terminalID,
                    title: title
                )
                flushPendingOutput(for: summary.sessionID)
                if let pendingExit = pendingExitBySession.removeValue(forKey: summary.sessionID) {
                    applyExited(pendingExit)
                }
                if sessionSummary?.sessionID == summary.sessionID,
                   latestRows != summary.rows || latestCols != summary.cols {
                    scheduleResize(sessionID: summary.sessionID, rows: latestRows, cols: latestCols)
                }
                Self.logger.info("terminal opened sessionID=\(summary.sessionID.rawValue, privacy: .public)")
            } catch {
                guard openGeneration == generation else {
                    return
                }
                isOpening = false
                errorText = error.localizedDescription
                statusText = "Terminal failed"
                Self.logger.error("failed to open terminal: \(error.localizedDescription, privacy: .private)")
            }
        }
    }

    func writeInput(_ data: Data) {
        enqueueInput(data)
    }

    func resize(rows: UInt16, cols: UInt16) {
        latestRows = max(rows, 1)
        latestCols = max(cols, 1)
        guard let sessionID = sessionSummary?.sessionID else {
            return
        }
        scheduleResize(sessionID: sessionID, rows: latestRows, cols: latestCols)
    }

    private func scheduleResize(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) {
        let request = (sessionID: sessionID, rows: rows, cols: cols)
        if lastSentResize?.sessionID == request.sessionID,
           lastSentResize?.rows == request.rows,
           lastSentResize?.cols == request.cols {
            return
        }
        pendingResize = request
        resizeTask?.cancel()
        resizeTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(80))
            guard !Task.isCancelled else { return }
            self?.flushPendingResize()
        }
    }

    private func flushPendingResize() {
        guard let request = pendingResize else {
            return
        }
        guard sessionSummary?.sessionID == request.sessionID else {
            pendingResize = nil
            return
        }
        pendingResize = nil
        sendResize(sessionID: request.sessionID, rows: request.rows, cols: request.cols)
    }

    private func sendResize(sessionID: TerminalSessionID, rows: UInt16, cols: UInt16) {
        guard let backend else {
            return
        }
        lastSentResize = (sessionID: sessionID, rows: rows, cols: cols)
        Task {
            do {
                try await backend.resizeTerminal(sessionID: sessionID, rows: rows, cols: cols)
            } catch {
                Self.logger.debug("failed to resize terminal: \(error.localizedDescription, privacy: .private)")
            }
        }
    }

    func closeSession() async {
        let backend = self.backend
        let sessionID = sessionSummary?.sessionID
        resetRuntimeState()
        guard let backend, let sessionID else {
            return
        }
        do {
            try await backend.closeTerminal(sessionID: sessionID)
        } catch {
            Self.logger.debug("failed to close terminal: \(error.localizedDescription, privacy: .private)")
        }
    }

    private func resetRuntimeState() {
        openGeneration += 1
        isOpening = false
        sessionSummary = nil
        lastSequence = 0
        coreTerminalID = nil
        hasExited = false
        isLiveOutputDiscontinuous = false
        outputResyncBufferedDeltas.removeAll(keepingCapacity: true)
        outputResyncTask?.cancel()
        outputResyncTask = nil
        outputReplayBuffer.removeAll(keepingCapacity: true)
        pendingResize = nil
        resizeTask?.cancel()
        resizeTask = nil
        lastSentResize = nil
        pendingInput.removeAll(keepingCapacity: true)
        inputFlushTask?.cancel()
        inputFlushTask = nil
        isFlushingInput = false
    }

    /// Output routed here by `TerminalWorkspaceModel`: this terminal's own, or
    /// any session's while this one is still opening.
    func receiveOutputDelta(_ delta: TerminalOutputDeltaParams) {
        // Both sides are optional now that a sign-in login terminal belongs
        // to no workspace. Without unwrapping ours first, `nil == nil`
        // would route that login's output into a surface that has not been
        // configured yet.
        guard let workspaceID, delta.workspaceID == workspaceID else {
            return
        }
        guard let activeSessionID = sessionSummary?.sessionID else {
            bufferOutputDelta(delta)
            return
        }
        guard delta.sessionID == activeSessionID else {
            if isOpening {
                bufferOutputDelta(delta)
            }
            return
        }
        applyOutputDelta(delta)
    }

    func receiveExited(_ exited: TerminalExitedParams) {
        guard let workspaceID, exited.workspaceID == workspaceID else {
            return
        }
        guard let activeSessionID = sessionSummary?.sessionID else {
            pendingExitBySession[exited.sessionID] = exited
            return
        }
        guard exited.sessionID == activeSessionID else {
            return
        }
        applyExited(exited)
    }

    private func bufferOutputDelta(_ delta: TerminalOutputDeltaParams) {
        var pending = pendingOutputBySession[delta.sessionID, default: []]
        pending.append(delta)
        if pending.count > 256 {
            pending.removeSubrange(0..<(pending.count - 256))
        }
        pendingOutputBySession[delta.sessionID] = pending
        evictStalePendingSessions(keeping: delta.sessionID)
    }

    private func evictStalePendingSessions(keeping sessionID: TerminalSessionID) {
        let maxPendingSessions = 8
        guard pendingOutputBySession.count > maxPendingSessions else { return }
        let evictionCandidates = pendingOutputBySession.keys.filter { $0 != sessionID }
        for staleSessionID in evictionCandidates.prefix(pendingOutputBySession.count - maxPendingSessions) {
            pendingOutputBySession.removeValue(forKey: staleSessionID)
            pendingExitBySession.removeValue(forKey: staleSessionID)
        }
    }

    private func flushPendingOutput(for sessionID: TerminalSessionID) {
        let pending = pendingOutputBySession
            .removeValue(forKey: sessionID)?
            .sorted { $0.sequence < $1.sequence } ?? []
        for delta in pending {
            applyOutputDelta(delta)
        }
    }

    private func applyOutputDelta(_ delta: TerminalOutputDeltaParams) {
        guard delta.sequence > lastSequence else {
            return
        }
        let hasSequenceGap = lastSequence > 0 && delta.sequence > lastSequence + 1
        let isDiscontinuousDelta = delta.deliveryState == "degraded"
            || delta.replaySuggested
            || (delta.droppedChunks ?? 0) > 0
            || (delta.droppedBytes ?? 0) > 0
            || hasSequenceGap
        if isLiveOutputDiscontinuous {
            if outputResyncTask != nil {
                bufferOutputDeltaDuringResync(delta)
            } else {
                lastSequence = delta.sequence
            }
            return
        }
        if isDiscontinuousDelta {
            handleOutputDiscontinuity(delta)
            return
        }
        lastSequence = delta.sequence
        if let data = Data(base64Encoded: delta.dataBase64), !data.isEmpty {
            appendOutput(data)
        }
        if delta.capReached {
            statusText = "Terminal live output limit reached."
        }
    }

    private func handleOutputDiscontinuity(_ delta: TerminalOutputDeltaParams) {
        lastSequence = max(lastSequence, delta.sequence)
        isLiveOutputDiscontinuous = true
        outputResyncBufferedDeltas.removeAll(keepingCapacity: true)
        statusText = "Restoring recent terminal output..."
        guard outputResyncTask == nil else {
            return
        }
        guard let backend,
              let workspaceID = self.workspaceID,
              let terminalID = delta.terminalID ?? coreTerminalID,
              let sessionID = sessionSummary?.sessionID else {
            statusText = "Live terminal output was interrupted. Reopen to restore recent output."
            return
        }

        outputResyncTask = Task { [weak self, backend] in
            do {
                let replay = try await backend.replayTerminal(
                    workspaceID: workspaceID,
                    terminalID: terminalID,
                    maxBytes: Self.outputResyncReplayMaxBytes
                )
                await MainActor.run {
                    self?.finishOutputResync(
                        replay,
                        sessionID: sessionID,
                        minimumSequence: delta.sequence
                    )
                }
            } catch {
                await MainActor.run {
                    self?.finishOutputResync(error: error)
                }
            }
        }
    }

    private func bufferOutputDeltaDuringResync(_ delta: TerminalOutputDeltaParams) {
        outputResyncBufferedDeltas.append(delta)
        lastSequence = delta.sequence
    }

    private func finishOutputResync(
        _ replay: TerminalReplayResult,
        sessionID: TerminalSessionID,
        minimumSequence: UInt64
    ) {
        guard sessionSummary?.sessionID == sessionID else {
            return
        }
        outputResyncTask = nil
        let bufferedDeltas = outputResyncBufferedDeltas.sorted { $0.sequence < $1.sequence }
        outputResyncBufferedDeltas.removeAll(keepingCapacity: true)
        resetOutputViewForReplay()
        var replayedLastSequence: UInt64 = 0
        for chunk in replay.chunks.sorted(by: { $0.sequenceStart < $1.sequenceStart }) {
            guard let data = Data(base64Encoded: chunk.dataBase64), !data.isEmpty else {
                continue
            }
            appendOutput(data)
            replayedLastSequence = max(replayedLastSequence, chunk.sequenceEnd)
        }
        guard replayedLastSequence >= minimumSequence else {
            lastSequence = max(lastSequence, replayedLastSequence)
            isLiveOutputDiscontinuous = true
            statusText = "Live terminal output was interrupted. Reopen to restore recent output."
            return
        }
        lastSequence = replayedLastSequence
        isLiveOutputDiscontinuous = false
        statusText = replay.truncated
            ? "Restored recent terminal output."
            : "Terminal output restored."
        for bufferedDelta in bufferedDeltas where bufferedDelta.sequence > lastSequence {
            applyOutputDelta(bufferedDelta)
        }
    }

    private func finishOutputResync(error: Error) {
        outputResyncTask = nil
        outputResyncBufferedDeltas.removeAll(keepingCapacity: true)
        statusText = "Live terminal output was interrupted. Reopen to restore recent output."
        Self.logger.error("failed to replay terminal output after live delivery gap: \(error.localizedDescription, privacy: .private)")
    }

    private func resetOutputViewForReplay() {
        outputReplayBuffer.removeAll(keepingCapacity: true)
        if let clearData = "\u{001B}c".data(using: .utf8) {
            outputSink?(clearData)
        }
    }

    private func appendOutput(_ data: Data) {
        let maxReplayBytes = 512 * 1024
        outputReplayBuffer.append(data, maxBytes: maxReplayBytes)

        if let outputSink {
            outputSink(data)
        }
    }

    private func enqueueInput(_ data: Data) {
        guard !data.isEmpty else { return }
        // Held while core is busy; past the cap, refuse rather than grow without bound.
        guard pendingInput.count + data.count <= Self.maxPendingInputBytes else {
            errorText = "The terminal is not reading input; some input was not sent."
            Self.logger.error("dropped \(data.count) bytes of terminal input over the held-input cap")
            return
        }
        pendingInput.append(data)
        scheduleInputFlush(delay: .milliseconds(8))
    }

    private func scheduleInputFlush(delay: Duration) {
        guard inputFlushTask == nil else { return }
        inputFlushTask = Task { [weak self] in
            if delay > .zero {
                try? await Task.sleep(for: delay)
            }
            guard !Task.isCancelled else { return }
            self?.inputFlushTask = nil
            self?.flushPendingInput()
        }
    }

    private func flushPendingInput() {
        guard !isFlushingInput, !pendingInput.isEmpty else { return }
        guard let backend, let sessionID = sessionSummary?.sessionID else {
            pendingInput.removeAll(keepingCapacity: true)
            errorText = "Terminal session is unavailable."
            return
        }
        let data = pendingInput
        pendingInput.removeAll(keepingCapacity: true)
        isFlushingInput = true

        Task { [weak self, backend] in
            do {
                try await TerminalInputWriter.write(data) { chunk in
                    try await backend.writeTerminal(sessionID: sessionID, data: chunk)
                }
                await MainActor.run {
                    self?.finishInputFlush(error: nil)
                }
            } catch {
                await MainActor.run {
                    self?.finishInputFlush(error: error)
                }
            }
        }
    }

    private func finishInputFlush(error: Error?) {
        isFlushingInput = false
        if let error {
            errorText = error.localizedDescription
            Self.logger.error("failed to write terminal input: \(error.localizedDescription, privacy: .private)")
            // Input typed after a gap would land out of context; drop it with the failure.
            if TerminalInputWriter.isBusy(error) {
                pendingInput.removeAll(keepingCapacity: true)
            }
        }
        if !pendingInput.isEmpty {
            scheduleInputFlush(delay: .zero)
        }
    }

    private func applyExited(_ exited: TerminalExitedParams) {
        guard sessionSummary?.sessionID == exited.sessionID else {
            return
        }
        sessionSummary = nil
        hasExited = true
        lastSequence = 0
        isLiveOutputDiscontinuous = false
        outputResyncBufferedDeltas.removeAll(keepingCapacity: true)
        outputResyncTask?.cancel()
        outputResyncTask = nil
        statusText = exited.exitStatus.map { "Terminal exited with status \($0)." } ?? "Terminal exited."
    }
}

/// Sends terminal input to core in order, in chunks well under its line limit.
/// Core refuses a chunk with `terminalInputBusy` while the program is not
/// reading and its queue is full; the writer resends that chunk with backoff
/// rather than skip it, and gives up only after `busyGiveUp` of waiting.
enum TerminalInputWriter {
    static let chunkBytes = 16 * 1024
    static let busyGiveUp: Duration = .seconds(30)

    static func write(
        _ data: Data,
        pause: (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
        send: (Data) async throws -> Void
    ) async throws {
        var offset = data.startIndex
        while offset < data.endIndex {
            let end = data.index(offset, offsetBy: chunkBytes, limitedBy: data.endIndex) ?? data.endIndex
            var delay: Duration = .milliseconds(25)
            var waited: Duration = .zero
            while true {
                do {
                    try await send(data[offset..<end])
                    break
                } catch where isBusy(error) && waited < busyGiveUp {
                    try await pause(delay)
                    waited += delay
                    delay = min(delay * 2, .milliseconds(500))
                }
            }
            offset = end
        }
    }

    static func isBusy(_ error: Error) -> Bool {
        guard case .rpcError(_, _, .object(let data)?) = error as? AgentConnectionError,
              case .string("terminalInputBusy")? = data["kind"]
        else { return false }
        return true
    }
}
