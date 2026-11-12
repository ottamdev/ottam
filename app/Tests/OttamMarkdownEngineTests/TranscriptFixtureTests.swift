import CryptoKit
import OttamTranscriptFixtures
import Foundation
import Testing

/// The fixtures are a measurement's independent variable, so "deterministic" is
/// not a nice property here — it is the whole contract. A corpus that quietly
/// changes bytes invalidates every baseline recorded against it, and the failure
/// is silent: the numbers still look like numbers.
///
/// Hence golden digests. They are deliberately annoying to update: if one of
/// these fails and the change was intended, the recorded baselines are stale and
/// have to be retaken, which is exactly the conversation the failure should
/// force.
@Suite("Transcript fixtures")
struct TranscriptFixtureTests {
    private func digest(_ text: String) -> String {
        SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    // MARK: - Frozen corpora

    @Test("the stage-3.7 corpus is byte-frozen")
    func stage37CorpusIsFrozen() {
        #expect(
            digest(MarkdownCorpus.stage37(
                minimumBytes: 250_000,
                seed: TranscriptFixtureCatalog.stage37CorpusSeed
            )) == "35457e249d9b62ecb516346e90a1cb987894a9005686d56955967374a0b245a4"
        )
        #expect(
            digest(MarkdownCorpus.stage37(
                minimumBytes: 12_000,
                seed: TranscriptFixtureCatalog.stage37ProbeSeed
            )) == "7ae134fc01551d5f4f0e1146d27f600c782a30921f075285ca95296c81c0d507"
        )
    }

    @Test("the transcript streaming corpus is byte-frozen")
    func streamingCorpusIsFrozen() {
        let corpus = MarkdownCorpus.transcriptStreaming(
            minimumBytes: TranscriptFixtureCatalog.streamingTotalBytes,
            seed: TranscriptFixtureCatalog.streamingCorpusSeed
        )
        #expect(
            digest(corpus)
                == "dca3dd1c6bcba96084b8d8cf03724f0d05e4430d12d4172d750b69abbc61fa97"
        )
        #expect(corpus.utf8.count == 250_152)
    }

    @Test("the 64KB single reply is byte-frozen")
    func singleReplyIsFrozen() {
        let reply = TranscriptFixtureCatalog.singleReply()
        #expect(
            digest(reply)
                == "b86185afee412bda7af0546b2cd329fc08cacacbe3a404101940f380a2d4aa1b"
        )
        #expect(reply.utf8.count == 64_168)
    }

    @Test("the multi-turn corpus is byte-frozen")
    func turnsAreFrozen() {
        let turns = TranscriptFixtureCatalog.streamingTurns()
        #expect(turns.count == 40)
        #expect(
            digest(turns.joined(separator: "\u{1}"))
                == "7ab4eadc4e764df66d31bca60866e5c521f5382bef7a8407c9290dff5d9bb734"
        )
    }

    @Test("the long-history corpus is byte-frozen")
    func longHistoryIsFrozen() {
        let replies = TranscriptFixtureCatalog.longHistoryReplies()
        #expect(replies.count == TranscriptFixtureCatalog.longHistoryItemCount / 2)
        let total = replies.reduce(0) { $0 + $1.utf8.count }
        #expect(total >= TranscriptFixtureCatalog.longHistoryTotalBytes)
        #expect(digest(replies.joined(separator: "\u{1}")) == "a9b6fc09a5ad7a33f0c4b5a97c9455d147e7c1769df6c5bbe84f9e06513f329c")
    }

    // MARK: - Shape

    /// Tables and images are the two constructs the SwiftUI surface renders as
    /// dedicated block views rather than text. A streaming corpus without them
    /// cannot exercise the block kinds most likely to break.
    @Test("the streaming corpus carries every block kind the gates need")
    func streamingCorpusIsMixed() {
        let corpus = MarkdownCorpus.transcriptStreaming(
            minimumBytes: TranscriptFixtureCatalog.streamingTotalBytes,
            seed: TranscriptFixtureCatalog.streamingCorpusSeed
        )
        #expect(corpus.contains("## Section"), "headings")
        #expect(corpus.contains("\n- item "), "lists")
        #expect(corpus.contains("```swift"), "fenced code")
        #expect(corpus.contains("> Quoted:"), "quotes")
        #expect(corpus.contains("| --- |"), "tables")
        #expect(corpus.contains("!["), "images")
        // The mask's hardest case: a link destination with balanced parens,
        // which must not flash the raw URL while the destination is still open.
        #expect(corpus.contains("https://example.org/wiki/Topic_("), "paren links")
    }

    /// ASCII only, so a chunk boundary at an arbitrary byte offset is always a
    /// valid `String` boundary. A multi-byte character would make the chunker
    /// produce different cuts on different runs.
    @Test("corpora stay ASCII so arbitrary byte cuts are valid")
    func corpusIsASCII() {
        let corpus = MarkdownCorpus.transcriptStreaming(minimumBytes: 60_000, seed: 7)
        let isASCII = corpus.allSatisfy(\.isASCII)
        #expect(isASCII)
    }

    @Test("turns are distinct and sum to roughly the requested size")
    func turnsAreDistinct() {
        let turns = TranscriptFixtureCatalog.streamingTurns()
        #expect(Set(turns).count == turns.count, "each turn must be its own content")
        let total = turns.reduce(0) { $0 + $1.utf8.count }
        #expect(total >= TranscriptFixtureCatalog.streamingTotalBytes)
        #expect(total < TranscriptFixtureCatalog.streamingTotalBytes * 2)
    }

    // MARK: - Chunking

    @Test("wire chunks stay in the 32–256 byte band and conserve the corpus")
    func chunkingConservesBytes() {
        let corpus = MarkdownCorpus.transcriptStreaming(minimumBytes: 60_000, seed: 11)
        let chunks = StreamChunking.chunks(of: corpus, seed: 13)
        #expect(chunks.joined() == corpus, "chunking must not lose or reorder bytes")
        // The final chunk is whatever remains, so it alone may be short.
        for chunk in chunks.dropLast() {
            #expect((32...256).contains(chunk.count))
        }
    }

    @Test("fixed-size chunking conserves the corpus")
    func fixedChunkingConservesBytes() {
        let corpus = MarkdownCorpus.transcriptStreaming(minimumBytes: 20_000, seed: 17)
        #expect(StreamChunking.chunks(of: corpus, fixedSize: 384).joined() == corpus)
        #expect(StreamChunking.chunks(of: corpus, fixedSize: 16).joined() == corpus)
    }

    @Test("the same seed produces the same bytes twice")
    func generationIsRepeatable() {
        let a = MarkdownCorpus.transcriptStreaming(minimumBytes: 40_000, seed: 99)
        let b = MarkdownCorpus.transcriptStreaming(minimumBytes: 40_000, seed: 99)
        #expect(a == b)
        let c = MarkdownCorpus.transcriptStreaming(minimumBytes: 40_000, seed: 100)
        #expect(a != c, "a different seed must produce different bytes")
    }
}
