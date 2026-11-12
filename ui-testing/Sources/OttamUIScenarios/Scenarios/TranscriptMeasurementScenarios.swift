import OttamAccessibility
import OttamTranscriptFixtures
import Foundation
import MacAutomationKit
import ScenarioKit

/// The native transcript measurement matrix defined by
/// `docs/operations/performance/transcript-renderer.md`.
///
/// Measurement scenarios do not assert pixels. They exist
/// to put the app into one specific, reproducible streaming state so a profile
/// can be taken against it, and they stream bytes from
/// `OttamTranscriptFixtures` rather than composing their own text, because a
/// number is only comparable to another number if both came from the same
/// corpus.
///
/// The existing `chat.transcript-streaming` scenario is deliberately left alone:
/// it has archived evidence recorded against it, and changing what it streams
/// would silently invalidate that history.
/// `transcript.updating-tools` additionally captures label replacement and
/// lifecycle transitions for visual and accessibility regression review.
public extension UIScenarioDefinition {
    static let transcriptUpdatingTools = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "transcript.updating-tools",
            description: "Validate changing tool labels, shimmer motion, completion and remount.",
            tags: ["chat", "animation"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["deterministic Rust ACP adapter"], fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let stop = context.element(id: AccessibilityID.chatStop)
        let activity = context.element(id: AccessibilityID.chatActivityGroup)
        try context.armACP(.changingTools, reply: "Checking changing tool labels.")
        try await context.setValue("Exercise changing shimmer labels", on: composer)
        try await context.press(submit)
        let labels = ["Reading configuration", "Reading configuration and workspace preferences",
            "Search source files for animation lifecycle", "Run step 12", "Run step 21",
            "Inspect very long command:", "Build"]
        for (index, label) in labels.enumerated() {
            try await context.waitFor(.labelContains(activity, label), timeout: 15)
            try await context.waitFor(.exists(stop), timeout: 5)
            try await Task.sleep(for: .milliseconds(250))
            try await context.capture(routeCapture(id: "tool-\(index)-before", required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatRunStatus]))
            try await Task.sleep(for: .milliseconds(450))
            try await context.capture(routeCapture(id: "tool-\(index)-motion", required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatRunStatus]))
        }
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 15)
        try await context.capture(routeCapture(id: "tools-completed", required: [AccessibilityID.chatSubmit]))
        try context.armACP(.holdToolUntilCancel, reply: "Starting a fresh tool after completion.")
        try await context.setValue("Start another tool", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(2, timeout: 15)
        try await context.waitFor(.exists(stop), timeout: 5)
        try await Task.sleep(for: .milliseconds(500))
        let repeatedGroups = [AXAuditException(rule: .duplicateIdentifier, identifier: AccessibilityID.chatActivityGroup, reason: "One activity group in each of two turns.")]
        try await context.capture(routeCapture(id: "remounted-before", required: [AccessibilityID.chatRunStatus], auditExceptions: repeatedGroups))
        try await Task.sleep(for: .milliseconds(450))
        try await context.capture(routeCapture(id: "remounted-motion", required: [AccessibilityID.chatRunStatus], auditExceptions: repeatedGroups))
        try await context.press(stop)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 15)
        try await context.capture(routeCapture(id: "remounted-cancelled", required: [AccessibilityID.chatSubmit], auditExceptions: repeatedGroups))
    }

    /// No stream or tool output during the scored phase.
    static let transcriptMeasureIdleTool = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "transcript.measure-idle-tool",
            description: "Hold one silent tool pending for 20 seconds to profile shimmer and working indicators.",
            tags: ["chat", "performance", "measurement"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["deterministic Rust ACP adapter"], fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let stop = context.element(id: AccessibilityID.chatStop)
        let activity = context.element(id: AccessibilityID.chatActivityGroup)
        try await waitForMeasurementHarnessReady()
        try context.armACP(.holdToolUntilCancel, reply: "The diagnostic tool is waiting without producing output.")
        try await context.setValue("Profile idle tool animations", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.exists(stop), timeout: 20)
        try await context.waitFor(.labelContains(activity, "Waiting for diagnostic tool"), timeout: 20)
        try await Task.sleep(for: .seconds(3))
        try await context.capture(routeCapture(
            id: "idle-tool-before",
            required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatRunStatus, AccessibilityID.chatStop]
        ))
        try await Task.sleep(for: .milliseconds(450))
        try await context.capture(routeCapture(
            id: "idle-tool-motion",
            required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatRunStatus, AccessibilityID.chatStop]
        ))
        try context.recordActivity("measurement_phase_started", fields: ["workload": "silent-pending-tool-20-seconds"])
        try await waitForMeasurementPhaseAcknowledgement("START")
        try await Task.sleep(for: .seconds(20))
        try context.recordActivity("measurement_phase_ended", fields: ["workload": "silent-pending-tool-20-seconds"])
        try await waitForMeasurementPhaseAcknowledgement("END")
        try await context.waitFor(.exists(stop), timeout: 5)
        try await context.capture(routeCapture(
            id: "idle-tool-after",
            required: [AccessibilityID.chatActivityGroup, AccessibilityID.chatRunStatus, AccessibilityID.chatStop]
        ))
        try await context.press(stop)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)
    }

    /// The spec's streaming fixture: ~250 KB across ~40 turns, which is where
    /// per-turn structural reconciliation cost shows up. A single long reply
    /// cannot expose it — the transcript only has one item to reconcile.
    static let transcriptMeasureMultiTurn = measurementScenario(
        id: "transcript.measure-multi-turn",
        summary: "Stream ~250KB across ~40 turns from the shared corpus, for profiling "
            + "structural reconciliation as history grows.",
        turns: TranscriptFixtureCatalog.streamingTurns()
    )

    /// One 64 KB reply. This is the fixture that decides whether full
    /// active-item parsing survives or sealed-prefix parsing is required, so it
    /// is deliberately a *single* item: all the growth is inside one parse.
    static let transcriptMeasureSingleReply = measurementScenario(
        id: "transcript.measure-single-reply",
        summary: "Stream one 64KB reply from the shared corpus, for profiling active-item "
            + "parsing and layout growth within a single item.",
        turns: [TranscriptFixtureCatalog.singleReply()]
    )

    /// The same stream with the reader scrolled back.
    ///
    /// Detached is not a cosmetic variant: pinned streaming pays a scroll
    /// correction per commit that detached streaming does not, so a CPU number
    /// taken only while pinned overstates the cost of reading history during a
    /// run — and one taken only while detached understates streaming.
    static let transcriptMeasureDetached = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "transcript.measure-detached",
            description: "Stream the shared corpus while the reader is scrolled back, so the "
                + "commit path runs without bottom-pinning work.",
            tags: ["chat", "markdown", "engine", "performance", "measurement"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: measurementLaunch,
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let scrollBack = context.element(id: AccessibilityID.chatScrollBackProbe)
        let scrollState = context.element(id: AccessibilityID.chatScrollStateProbe)

        try await waitForMeasurementHarnessReady()

        try context.armStreamedACP(
            reply: TranscriptFixtureCatalog.singleReply(),
            chunkBytes: measurementChunkBytes(),
            chunkDelayMillis: measurementDelayMillis()
        )
        try await context.setValue("Stream the long thing", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)

        // Let the document grow past a viewport before detaching, or scrolling
        // back has nothing to scroll back through.
        try await Task.sleep(for: .seconds(4))

        // More than one viewport back, because the surface re-pins the moment
        // the viewport returns to the bottom (`isAtBottom`) and the document is
        // growing underneath the whole time.
        for _ in 0..<detachViewports() { try await context.press(scrollBack) }
        try await context.press(scrollState)
        try await context.waitFor(.labelContains(scrollState, "Detached"), timeout: 8)
        try context.recordActivity(
            "measurement_phase_started",
            fields: ["workload": "fixed-duration-detached-stream"]
        )
        try await waitForMeasurementPhaseAcknowledgement("START")

        // The profiling window: steady-state streaming with the reader parked.
        try await Task.sleep(for: .seconds(measurementSampleSeconds()))
        try context.recordActivity(
            "measurement_phase_ended",
            fields: ["workload": "fixed-duration-detached-stream"]
        )
        try await waitForMeasurementPhaseAcknowledgement("END")
        try await context.press(scrollState)
        try await context.waitFor(.labelContains(scrollState, "Detached"), timeout: 8)
        try await context.capture(
            routeCapture(
                id: "transcript-measure-detached",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    /// Ten conversations, each the full multi-turn corpus, then a return to
    /// the first. The scored phase is the parked state, where a harness reads
    /// the app's memory footprint. `OTTAM_UI_TEST_SWITCH_CACHE_CAPACITY`
    /// (default 10) sets how many conversations the app keeps cached, so the
    /// same workload compares a full cache with a single entry.
    static let chatMeasureSwitchCacheMemory = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.measure-switch-cache-memory",
            description: "Stream the multi-turn corpus into ten conversations, return to the first, "
                + "and park so a harness can read the memory footprint.",
            tags: ["chat", "cache", "performance", "measurement"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused", routes: "chat",
            environment: [
                "OTTAM_UI_TEST_APPEARANCE": "light",
                "OTTAM_DEBUG_CONVERSATION_CACHE": ProcessInfo.processInfo.environment[
                    "OTTAM_UI_TEST_SWITCH_CACHE_CAPACITY"
                ] ?? "10",
            ],
            fixtures: ["deterministic Rust ACP adapter"], fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let turns = TranscriptFixtureCatalog.streamingTurns()
        let titles = (0..<10).map { "Conversation \(Character(UnicodeScalar(65 + $0)!))" }
        var prompts = 0

        try await waitForMeasurementHarnessReady()
        for (conversation, title) in titles.enumerated() {
            if conversation > 0 {
                // The press can report an AX failure after it has already
                // switched to the draft; pressing New Chat again is harmless.
                for attempt in 1...3 {
                    do {
                        try await context.pressControl(named: "New Chat", waitForText: "New Chat")
                        break
                    } catch where attempt < 3 {
                        try await Task.sleep(for: .seconds(1))
                    }
                }
            }
            for (index, turn) in turns.enumerated() {
                try context.armStreamedACP(
                    reply: turn,
                    chunkBytes: measurementChunkBytes(),
                    chunkDelayMillis: measurementDelayMillis()
                )
                try await context.setValue(index == 0 ? title : "Turn \(index + 1)", on: composer)
                try await context.press(submit)
                prompts += 1
                try await context.waitForACPPromptCount(prompts, timeout: 20)
                // Send reappears only once the run has ended.
                try await context.waitFor(.valueEquals(submit, "ready"), timeout: 120)
            }
        }

        try await context.selectListRow(named: titles[0], waitForText: titles[0])
        try await waitForChat(context)
        try await Task.sleep(for: .seconds(5))
        try context.recordActivity("measurement_phase_started", fields: ["workload": "ten-cached-conversations"])
        try await waitForMeasurementPhaseAcknowledgement("START")
        try context.recordActivity("measurement_phase_ended", fields: ["workload": "ten-cached-conversations"])
        try await waitForMeasurementPhaseAcknowledgement("END")
        try await context.capture(routeCapture(id: "switch-memory-parked", required: [AccessibilityID.chatComposer]))
    }

}

// MARK: - Shared shape

private let measurementLaunch = ScenarioLaunchConfiguration(
    validationProfile: "focused",
    routes: "chat",
    environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
    fixtures: ["deterministic Rust ACP adapter"],
    fixtureRequirements: [.testACP]
)

/// A measurement run streams every turn of a corpus through the chat, then
/// parks so a profiler can sample steady state.
///
/// Each turn is armed and submitted separately because `armStreamedACP` arms one
/// prompt at a time. That is not a workaround: a real forty-turn conversation is
/// forty prompts, and collapsing them into one reply would measure a different
/// thing than the fixture describes.
private func measurementScenario(
    id: String,
    summary: String,
    turns: [String]
) -> UIScenarioDefinition {
    UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: id,
            description: summary,
            tags: ["chat", "markdown", "engine", "performance", "measurement"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: measurementLaunch,
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let running = context.element(id: AccessibilityID.chatStop)

        let chunkBytes = measurementChunkBytes()
        let delayMillis = measurementDelayMillis()

        try await waitForMeasurementHarnessReady()

        for (index, turn) in turns.enumerated() {
            try context.armStreamedACP(
                reply: turn,
                chunkBytes: chunkBytes,
                chunkDelayMillis: delayMillis
            )
            try await context.setValue("Turn \(index + 1)", on: composer)
            if index == 0 {
                try context.recordActivity(
                    "measurement_phase_started",
                    fields: ["workload": "complete-frozen-corpus"]
                )
                try await waitForMeasurementPhaseAcknowledgement("START")
            }
            try await context.press(submit)
            try await context.waitForACPPromptCount(index + 1, timeout: 15)
            // Fixed composer controls remain accessible while transcript cards
            // are virtualized. Stop establishes the live phase; submit's ready
            // value establishes completion without depending on a hosted row.
            try await context.waitFor(.exists(running), timeout: 20)
            try await context.waitFor(.valueEquals(submit, "ready"), timeout: 600)
        }

        try context.recordActivity(
            "measurement_phase_ended",
            fields: ["workload": "complete-frozen-corpus"]
        )
        try await waitForMeasurementPhaseAcknowledgement("END")

        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(
                id: id.replacingOccurrences(of: ".", with: "-"),
                required: [AccessibilityID.chatComposer]
            )
        )
    }
}

/// Sweep knobs. The corpus is fixed; the *wire shape* is what a sweep varies, so
/// these are overridable while the bytes never are.
private func measurementOverride(_ name: String, _ fallback: Int) -> Int {
    ProcessInfo.processInfo.environment["OTTAM_UI_TEST_STREAM_\(name)"]
        .flatMap(Int.init) ?? fallback
}

private func waitForMeasurementHarnessReady() async throws {
    guard let path = ProcessInfo.processInfo.environment[
        "OTTAM_UI_TEST_MEASUREMENT_READY_FILE"
    ] else { return }
    try await waitForMeasurementFile(at: path)
}

private func waitForMeasurementPhaseAcknowledgement(_ boundary: String) async throws {
    guard let path = ProcessInfo.processInfo.environment[
        "OTTAM_UI_TEST_MEASUREMENT_PHASE_\(boundary)_ACK_FILE"
    ] else { return }
    try await waitForMeasurementFile(at: path)
}

private func waitForMeasurementFile(at path: String) async throws {
    for _ in 0..<2_400 {
        if FileManager.default.fileExists(atPath: path) { return }
        try await Task.sleep(for: .milliseconds(50))
    }
    throw CocoaError(.fileReadNoSuchFile)
}

private func measurementChunkBytes() -> Int { measurementOverride("CHUNK_BYTES", 64) }

/// 20 ms is 50 deltas/s — the top of the spec's sweep, and the rate the stage-3.6
/// baseline was recorded at. The sweep's other points are 62 ms (16/s) and
/// 200 ms (5/s).
private func measurementDelayMillis() -> Int { measurementOverride("DELAY_MS", 20) }

private func measurementSampleSeconds() -> Int { measurementOverride("SAMPLE_SECONDS", 12) }

private func detachViewports() -> Int { measurementOverride("DETACH_VIEWPORTS", 3) }
