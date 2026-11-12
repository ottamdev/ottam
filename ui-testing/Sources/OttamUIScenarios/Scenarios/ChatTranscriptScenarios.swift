import Foundation
import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let chatTranscriptLiveLight = chatTranscriptLiveScenario(appearance: "light")
    static let chatTranscriptLiveDark = chatTranscriptLiveScenario(appearance: "dark")

    /// Stage 3.5: a reply taller than the viewport lands and the transcript
    /// stays pinned to its end — through the run leaving the composer (the
    /// bottom inset shrinks) and through the live→persisted swap. Re-selecting
    /// the conversation rebuilds the surface before the window's insets reach
    /// it, and it still opens on the end.
    static let chatTranscriptComplete = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-complete",
            description: "Complete a turn with a reply taller than the viewport: capture the "
                + "transcript pinned to its end, then re-select the conversation and "
                + "capture it pinned to its end again.",
            tags: ["chat", "markdown", "engine", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["deterministic Rust ACP adapter"],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let title = "Write the long thing"
        try context.armACP(.complete, reply: tallReply(sections: 40))
        try await context.setValue(title, on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)
        // A finished run is waited for on the composer's primary button, not on
        // the absence of `chat.run-status`. That status is a card inside the
        // virtualized transcript, so one missed read of the accessibility tree
        // satisfies a `notExists` while the run is still going — and the next
        // message then lands mid-run as a steer. The button is fixed chrome that
        // is always published.
        //
        // On its value, not its existence: the identifier stays `chat.submit`
        // while a submission is still in flight, so existence alone would let
        // the next message be typed into a composer that has not finished
        // sending the last one. `ready` is the only state that is neither
        // sending nor holding a live run.
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 30)
        // The persisted message lands one detail load after the run ends.
        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(id: "chat-transcript-complete", required: [AccessibilityID.chatComposer])
        )
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
        try await context.selectListRow(named: title, waitForText: title)
        try await waitForChat(context)
        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(id: "chat-transcript-reselected", required: [AccessibilityID.chatComposer])
        )
    }

    /// A reply that arrives the way a real agent's does: ~24-byte deltas, 20ms
    /// apart, for a document taller than the viewport. Every other fixture
    /// delivers its reply in one update, so this is the only scenario that
    /// exercises the per-chunk parse/style/layout path — and the only one a
    /// CPU profile of streaming can be taken against.
    static let chatTranscriptStreaming = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-streaming",
            description: "Stream a reply taller than the viewport in small deltas: capture it "
                + "mid-stream and again once the turn completes, both pinned to the end.",
            tags: ["chat", "markdown", "engine", "performance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["deterministic Rust ACP adapter"],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        // Overridable so a CPU sweep can vary the chunk rate without a new
        // scenario: the question "is streaming cost per-delta or a fixed
        // animation cost?" is answered by running this at several rates.
        try context.armStreamedACP(
            reply: tallReply(sections: streamOverride("SECTIONS", 40)),
            chunkBytes: streamOverride("CHUNK_BYTES", 24),
            chunkDelayMillis: streamOverride("DELAY_MS", 20)
        )
        try await context.setValue("Stream the long thing", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)
        // Long enough that the document is well past one viewport, so the
        // capture (and any profiler attached to the app) sees the steady state
        // rather than the first few chunks.
        try await Task.sleep(for: .seconds(12))
        // The live status is asserted with a retrying wait, and the capture
        // requires `chat.stop` instead. Both say a run is live, but the audit
        // walks the whole tree in one pass while the stream appends every 20ms,
        // and a transcript row that is genuinely present can be missed in that
        // pass. `chat.stop` is composer chrome that does not move.
        try await context.waitFor(
            .exists(context.element(id: AccessibilityID.chatRunStatus)),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "chat-transcript-streaming",
                required: [AccessibilityID.chatStop, AccessibilityID.chatComposer]
            )
        )
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 600)
        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(
                id: "chat-transcript-streamed",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    /// Native AppKit cutover gate: the loaded page is deliberately just beyond
    /// 512 KiB so collection-item reuse and bounded accessibility are exercised
    /// without restoring the removed unlimited-history measurement.
    static let chatTranscriptAppKitNative = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-appkit-native",
            description: "Seed a bounded transcript beyond 512 KiB, then exercise native "
                + "streaming, prepend, image/disclosure reflow, resize, selection/copy, "
                + "detachment, restoration and teardown.",
            tags: ["chat", "appkit", "native", "performance", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: [
                "deterministic Rust ACP adapter",
                "nine bounded 64 KiB turns",
                "repo-local Ottam icon",
            ],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let running = context.element(id: AccessibilityID.chatRunStatus)

        try context.armACP(.complete, reply: "Hello! What would you like to work on?")
        try await context.setValue("First inset gate", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 60)
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-first-exchange",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )

        for index in 1...9 {
            try context.armACP(.complete, reply: boundedNativeTurn(index: index, bytes: 64 * 1_024))
            try await context.setValue("Seed turn \(index)", on: composer)
            try await context.press(submit)
            try await context.waitForACPPromptCount(index + 1, timeout: 20)
            try await context.waitFor(.valueEquals(submit, "ready"), timeout: 60)
        }

        // The loaded event page is byte-bounded, so reaching its top fetches
        // the preceding page. The native renderer must preserve the anchor.
        for _ in 0..<12 {
            try await context.press(context.element(id: AccessibilityID.chatScrollBackProbe))
        }
        try await Task.sleep(for: .seconds(3))
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-prepended",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )
        try await context.press(context.element(id: AccessibilityID.chatScrollBottomProbe))

        // A real local bitmap swaps the placeholder for decoded geometry and
        // forces a targeted image-height/layout update.
        try context.armACP(
            .complete,
            reply: "## Image reflow\n\n![Ottam icon](assets/brand/png/ottam-icon-128.png)\n\nImage loaded."
        )
        try await context.setValue("Render the local image", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(11, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 60)
        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-image-reflow",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )

        let resizeProbe = context.element(id: AccessibilityID.chatResizeProbe)
        try await context.press(resizeProbe)
        try await context.waitFor(.labelContains(resizeProbe, "Window Resized"), timeout: 8)
        try await Task.sleep(for: .seconds(1))
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-resized",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )

        try context.armStreamedACP(reply: tallReply(sections: 200), chunkBytes: 64, chunkDelayMillis: 20)
        try await context.setValue("Stream the native tail", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(12, timeout: 20)
        try await context.waitFor(.exists(running), timeout: 20)
        try await Task.sleep(for: .milliseconds(500))

        try await context.press(context.element(id: AccessibilityID.chatScrollBackProbe))
        let scrollState = context.element(id: AccessibilityID.chatScrollStateProbe)
        try await context.press(scrollState)
        try await context.waitFor(.labelContains(scrollState, "Detached"), timeout: 8)
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-detached",
                // Not `chatRunStatus`: the status is a transcript card now, so
                // scrolling a viewport back correctly de-materializes it out of
                // the accessibility tree. What has to survive detaching is the
                // chrome — the composer, and a live run that is still stoppable.
                required: [AccessibilityID.chatComposer, AccessibilityID.chatStop]
            )
        )

        let selectionCopyProbe = context.element(id: AccessibilityID.chatSelectionCopyProbe)
        try await context.press(selectionCopyProbe)
        try await context.waitFor(.labelContains(selectionCopyProbe, "Selection Copied"), timeout: 8)

        try await context.press(context.element(id: AccessibilityID.chatScrollBottomProbe))
        // Re-pinning and the live status are asserted directly rather than through
        // the capture. The audit walks the tree for about half a second while the
        // stream appends every 20ms, so a row that is genuinely present can still
        // be missed; the capture therefore stays on chrome that does not move.
        try await context.press(scrollState)
        try await context.waitFor(.labelContains(scrollState, "Following Bottom"), timeout: 8)
        try await context.waitFor(.exists(running), timeout: 20)
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-pinned",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )

        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 600)
        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(id: "chat-transcript-appkit-settled", required: [AccessibilityID.chatComposer])
        )

        // Exercise hosted-card disclosure and its height update in the same
        // native collection generation.
        let permissionCard = context.element(id: AccessibilityID.chatPermissionCard)
        try context.armACP(.requestPermission)
        try await context.setValue("Open the approval disclosure", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(13, timeout: 20)
        try await context.waitFor(.exists(permissionCard), timeout: 20)
        try await context.pressControl(
            named: "Request details",
            waitForText: "Request details"
        )
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-disclosure",
                required: [
                    AccessibilityID.chatPermissionCard,
                    AccessibilityID.chatComposer,
                    AccessibilityID.chatTranscript,
                ]
            )
        )
        try await context.pressControl(named: "Deny", waitForText: "Stop")
        try await context.press(context.element(id: AccessibilityID.chatStop))
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 20)

        // Replacing the conversation tears down this controller. Returning to
        // it rebuilds persisted history, and leaving once more is the final
        // lifecycle teardown gate.
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
        try await context.selectListRow(
            named: "First inset gate",
            waitForText: "Retained ACP reply."
        )
        try await waitForChat(context)
        try await Task.sleep(for: .seconds(2))
        try await context.capture(
            routeCapture(
                id: "chat-transcript-appkit-restored",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )
        try await context.selectListRow(named: "Terminal", waitForText: "Terminal")
    }
}

extension UIScenarioDefinition {
    /// Rich code blocks in the real Chat window: a streamed reply with a wide
    /// fence, a fence past the fold, and an unclosed fence the run settles
    /// on. Proves the card chrome, the fold toggle and the Copy button
    /// against the live→settled path, and that an incomplete fence leaves
    /// exactly one code surface.
    static let chatCodeBlocks = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.code-blocks",
            description: "Stream a reply with a wide fence, a fence past the fold and an "
                + "unclosed fence; capture settled and expanded, then press Copy.",
            tags: ["chat", "appkit", "native", "markdown"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["deterministic Rust ACP adapter"],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let copy = context.element(id: AccessibilityID.chatCodeBlockCopy)
        let expand = context.element(id: AccessibilityID.chatCodeBlockExpand)

        try context.armStreamedACP(reply: codeBlocksReply(), chunkBytes: 24, chunkDelayMillis: 5)
        try await context.setValue("Show me the code blocks", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 120)
        try await context.waitFor(.exists(expand), timeout: 10)
        try await Task.sleep(for: .seconds(1))
        try await context.capture(
            routeCapture(
                id: "chat-code-blocks-settled",
                required: [
                    AccessibilityID.chatComposer,
                    AccessibilityID.chatTranscript,
                    AccessibilityID.chatCodeBlockCopy,
                    AccessibilityID.chatCodeBlockExpand,
                ]
            )
        )

        try await context.press(expand)
        try await context.waitFor(.labelContains(expand, "Show less"), timeout: 8)
        try await Task.sleep(for: .seconds(1))
        try await context.capture(
            routeCapture(
                id: "chat-code-blocks-expanded",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatCodeBlockExpand]
            )
        )

        // The driver presses the first Copy in the tree, which belongs to the
        // wide fence at the top; bring it into view so the capture shows it.
        for _ in 0..<6 { try await context.press(context.element(id: AccessibilityID.chatScrollBackProbe)) }
        try await Task.sleep(for: .milliseconds(500))
        try await context.press(copy)
        try await context.waitFor(.labelContains(copy, "Copied"), timeout: 4)
        // Inside the 1.5 s dwell: the checkmark is the settled end of the
        // symbol replace transition.
        try await context.capture(
            routeCapture(
                id: "chat-code-blocks-copied",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatCodeBlockCopy]
            )
        )
    }
}

extension UIScenarioDefinition {
    /// Rich tables in the real Chat window: a streamed reply with an aligned
    /// table carrying inline markup, a wide table that overflows, and a table
    /// the reply stops inside. Proves the tile and the aligned grid against
    /// the live→settled path.
    static let chatTables = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.tables",
            description: "Stream a reply with an aligned table, a wide table and an unfinished "
                + "table; capture the settled transcript.",
            tags: ["chat", "appkit", "native", "markdown"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_APPEARANCE": "light"],
            fixtures: ["deterministic Rust ACP adapter"],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)

        try context.armStreamedACP(reply: tablesReply(), chunkBytes: 24, chunkDelayMillis: 5)
        try await context.setValue("Show me the tables", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 20)
        try await context.waitFor(.valueEquals(submit, "ready"), timeout: 120)
        try await Task.sleep(for: .seconds(1))
        try await context.capture(
            routeCapture(
                id: "chat-tables-settled",
                required: [AccessibilityID.chatComposer, AccessibilityID.chatTranscript]
            )
        )
    }
}

/// An aligned table with inline markup, a wide table, and a table the reply
/// stops inside — the shape of a response stopped mid-grid.
private func tablesReply() -> String {
    """
    Three table surfaces at once.

    | Flag | Default | Effect |
    |:-----|:-------:|-------:|
    | `--verbose` | off | prints every **step** |
    | `--jobs` | 4 | parallel *workers* |
    | `--out` | `./dist` | see [docs](https://example.com/out) |

    Wide enough to scroll:

    | Metric | Baseline p50 | Baseline p95 | Feature p50 | Feature p95 | Delta p50 | Delta p95 | Notes |
    |--------|-------------:|-------------:|------------:|------------:|----------:|----------:|-------|
    | preparation | 1.92 ms | 4.10 ms | 1.88 ms | 4.02 ms | -0.04 ms | -0.08 ms | three matched runs on the shared corpus |
    | layout | 0.71 ms | 1.60 ms | 0.70 ms | 1.58 ms | -0.01 ms | -0.02 ms | no visible change |

    Stopped mid-table:

    | step | state |
    |------|-------|
    | one | done |
    | two | in progress
    """
}

/// One wide fence, one fence past the fold, and a fence the reply never
/// closes — the shape of a response stopped mid-code.
private func codeBlocksReply() -> String {
    let folded = (1...85).map { "let value\($0) = compute(\($0)) // line \($0)" }
        .joined(separator: "\n")
    return """
    Three code surfaces at once.

    ```swift
    let wide = "a deliberately long unbroken line that runs past the column so the tile scrolls sideways instead of wrapping"
    ```

    Below the fold:

    ```swift
    \(folded)
    ```

    Stopped mid-fence:

    ```rust
    fn main() {
        println!("the reply ends here");
    """
}

private func boundedNativeTurn(index: Int, bytes: Int) -> String {
    let line = "turn \(index): native AppKit collection reuse keeps only visible Core Text blocks alive.\n"
    var value = "## Bounded turn \(index)\n\n"
    while value.utf8.count + line.utf8.count <= bytes { value += line }
    let remainder = bytes - value.utf8.count
    if remainder > 0 { value += String(repeating: "x", count: remainder) }
    return value
}

private func streamOverride(_ name: String, _ fallback: Int) -> Int {
    ProcessInfo.processInfo.environment["OTTAM_UI_TEST_STREAM_\(name)"]
        .flatMap(Int.init) ?? fallback
}

/// Headings, wrapped paragraphs, lists, code and quotes — every block kind
/// whose height TextKit would otherwise estimate — ending on a line the
/// capture can be checked against.
func tallReply(sections: Int) -> String {
    (1...sections).map { index in
        """
        ## Section \(index)

        \(String(repeating: "A long wrapped sentence about section \(index) that keeps going so it spans several lines at this width. ", count: 4))

        - item one for \(index) with enough words to wrap onto a second line when the column is narrow
        - item two for \(index)
        - item three for \(index)

        ```swift
        func section\(index)() -> Int {
            let value = compute(\(index))
            return value * 2
        }
        ```

        > a quoted line for section \(index) that also wraps because it is fairly long in its own right
        """
    }.joined(separator: "\n\n") + "\n\nEnd of reply — this line must sit above the composer."
}

/// Stage 3.4: the chat transcript is one engine surface, and everything that
/// is not markdown is a hosted SwiftUI card.
/// The deterministic normalized adapter puts text, activity groups and a live
/// ticker on screen at once; the audit proves the hosted cards keep their
/// accessibility identifiers, and the absent jump pill proves the surface
/// stays pinned to the bottom through the cards' layout.
private func chatTranscriptLiveScenario(appearance: String) -> UIScenarioDefinition {
    UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.transcript-live-\(appearance)",
            description: "Capture a mid-run chat transcript on the engine surface in "
                + "\(appearance) appearance: a user bubble, streamed text and hosted "
                + "activity-group cards, pinned to the bottom.",
            tags: ["chat", "markdown", "engine", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: [
                "OTTAM_UI_TEST_APPEARANCE": appearance,
            ],
            fixtures: ["deterministic Rust ACP adapter"],
            fixtureRequirements: [.testACP]
        ),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let activityGroup = context.element(id: AccessibilityID.chatActivityGroup)
        let jumpPill = context.element(id: AccessibilityID.chatPermissionJump)
        try context.armACP(.normalizedTranscript, reply: "Researching alternatives to ACP.")
        try await context.setValue("Research alternatives to ACP", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)
        try await context.waitFor(.exists(activityGroup), timeout: 20)
        try await context.waitFor(.notExists(jumpPill), timeout: 5)
        try await context.capture(
            routeCapture(
                id: "chat-transcript-live-\(appearance)",
                required: [
                    AccessibilityID.chatActivityGroup,
                    AccessibilityID.chatRunStatus,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }
}
