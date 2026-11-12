import Foundation

/// The named fixtures the transcript-renderer gates run through, with their
/// recorded seeds.
///
/// The seeds live here rather than at each call site so a measurement can be
/// reproduced from a fixture name alone, and so a seed cannot drift between the
/// unit benchmark, the UI-test scenarios and the document lab — three consumers
/// that must stream identical bytes for their numbers to be comparable.
///
/// Sizes are *minimums*: a corpus finishes the section it is building, so the
/// realized byte count is slightly above the target and is reported with the
/// run rather than assumed.
public enum TranscriptFixtureCatalog {
    // MARK: - Seeds

    /// Frozen: the stage-3.7 benchmark's original corpus seed.
    public static let stage37CorpusSeed: UInt64 = 0xC0FF_EE
    /// Frozen: the stage-3.7 benchmark's original chunk seed.
    public static let stage37ChunkSeed: UInt64 = 0xBEEF
    /// Frozen: the stage-3.7 cost-model probe's corpus seed.
    public static let stage37ProbeSeed: UInt64 = 0xFACA_DE

    /// The transcript-renderer streaming fixtures.
    public static let streamingCorpusSeed: UInt64 = 0x5747_4D11
    public static let streamingChunkSeed: UInt64 = 0x0DD1_7E5
    /// The long-history fixture.
    public static let longHistorySeed: UInt64 = 0x1_0000_1E5
    /// The settled-document fixture.
    public static let documentSeed: UInt64 = 0xD0CC_0DE

    // MARK: - Sizes

    /// The always-on streaming corpus: ~250 KB spread across ~40 turns.
    public static let streamingTotalBytes = 250_000
    public static let streamingTurnCount = 40

    /// A single reply large enough to expose active-item parsing and layout
    /// growth within one item, which the multi-turn corpus deliberately does
    /// not. This is the fixture that decides whether full active-item parsing
    /// survives or sealed-prefix parsing is required.
    public static let singleReplyBytes = 64_000

    /// The opt-in sustained run, gated behind `OTTAM_BENCH=1`.
    public static let sustainedBytes = 4_000_000

    /// The long-history fixture: 1,000 items holding at least 4 MB of markdown.
    public static let longHistoryItemCount = 1_000
    public static let longHistoryTotalBytes = 4_000_000

    // MARK: - Built fixtures

    /// ~40 turns totalling ~250 KB.
    public static func streamingTurns() -> [String] {
        MarkdownCorpus.turns(
            count: streamingTurnCount,
            totalBytes: streamingTotalBytes,
            seed: streamingCorpusSeed
        )
    }

    /// One 64 KB reply.
    public static func singleReply() -> String {
        MarkdownCorpus.transcriptStreaming(
            minimumBytes: singleReplyBytes,
            seed: streamingCorpusSeed
        )
    }

    /// The opt-in 4 MB sustained corpus.
    public static func sustained() -> String {
        MarkdownCorpus.transcriptStreaming(
            minimumBytes: sustainedBytes,
            seed: streamingCorpusSeed
        )
    }

    /// The assistant replies for the long-history fixture.
    ///
    /// One prompt/reply cycle produces two transcript items — the user message
    /// and the assistant message — so reaching `longHistoryItemCount` items
    /// takes half that many replies, each carrying its share of the total.
    ///
    /// These are seeded by driving real prompts through the test ACP adapter
    /// rather than by writing to the store, because the gate is about what the
    /// app materializes and a hand-seeded store can drift from what the app
    /// actually persists. It is slower per run; that cost buys the number's
    /// meaning.
    public static func longHistoryReplies() -> [String] {
        let replies = longHistoryItemCount / 2
        return MarkdownCorpus.turns(
            count: replies,
            totalBytes: longHistoryTotalBytes,
            seed: longHistorySeed
        )
    }

    /// A settled markdown document at one of the standing sizes.
    public static func document(bytes: Int) -> String {
        MarkdownCorpus.transcriptStreaming(minimumBytes: bytes, seed: documentSeed)
    }

    /// Whether the opt-in benchmark runs are enabled for this process.
    public static var sustainedRunsEnabled: Bool {
        ProcessInfo.processInfo.environment["OTTAM_BENCH"] == "1"
    }
}
