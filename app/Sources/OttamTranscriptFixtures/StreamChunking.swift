import Foundation

/// Cuts a corpus into delta-sized chunks the way the wire actually delivers
/// text: arbitrary byte offsets, mid-word and mid-construct.
///
/// Frozen alongside `MarkdownCorpus` — the chunk boundaries are part of the
/// fixture, not an implementation detail. A streamed corpus whose cuts moved is
/// a different measurement.
public enum StreamChunking {
    /// The default wire shape: 32–256 byte chunks at arbitrary offsets.
    public static func chunks(of corpus: String, seed: UInt64) -> [String] {
        var random = SeededRandom(state: seed)
        var chunks: [String] = []
        var remaining = Substring(corpus)
        while !remaining.isEmpty {
            let size = 32 + random.below(225)
            let cut = remaining.index(
                remaining.startIndex,
                offsetBy: size,
                limitedBy: remaining.endIndex
            ) ?? remaining.endIndex
            chunks.append(String(remaining[..<cut]))
            remaining = remaining[cut...]
        }
        return chunks
    }

    /// Uniform chunks, for the cost-model probe that asks whether per-call cost
    /// still dominates per-byte cost.
    public static func chunks(of corpus: String, fixedSize: Int) -> [String] {
        precondition(fixedSize > 0, "a chunk needs at least one character")
        var chunks: [String] = []
        var remaining = Substring(corpus)
        while !remaining.isEmpty {
            let cut = remaining.index(
                remaining.startIndex,
                offsetBy: fixedSize,
                limitedBy: remaining.endIndex
            ) ?? remaining.endIndex
            chunks.append(String(remaining[..<cut]))
            remaining = remaining[cut...]
        }
        return chunks
    }
}
