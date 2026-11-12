import Foundation

/// Deterministic mixed-markdown corpora for transcript and document rendering
/// measurements.
///
/// These are the measurement's independent variable, so every generator here is
/// frozen: given a seed it must produce identical bytes on every machine and in
/// every checkout, forever. Adding a *new* generator is fine; editing an
/// existing one silently invalidates every baseline recorded against it.
///
/// See `docs/operations/performance/transcript-renderer.md` for which
/// fixture each gate runs through, and `TranscriptFixtureCatalog` for the
/// recorded seeds.
public enum MarkdownCorpus {
    /// The shared vocabulary. ASCII only, so a chunk cut at an arbitrary byte
    /// offset is always a valid `String` boundary.
    static let words = [
        "transcript", "stream", "layout", "viewport", "attachment", "reveal",
        "pending", "sealed", "fragment", "anchor", "delta", "storage",
        "element", "grants", "watchdog", "surface", "clock", "token",
    ]

    // MARK: - Stage 3.7 corpus (frozen)

    /// The original stage-3.7 benchmark corpus: prose, headings, lists, quotes,
    /// links with paren URLs (the streaming mask's hardest case) and code
    /// fences, cycling over six section kinds.
    ///
    /// Frozen bit-for-bit so `StreamingBenchmarkTests` keeps comparing against
    /// its recorded history across renderer cutovers. Richer content lives in
    /// ``transcriptStreaming(minimumBytes:seed:)`` instead of being folded in
    /// here.
    public static func stage37(minimumBytes: Int, seed: UInt64) -> String {
        var random = SeededRandom(state: seed)
        var sections: [String] = []
        var bytes = 0
        var index = 0
        while bytes < minimumBytes {
            let section: String
            switch index % 6 {
            case 0:
                section = heading(index: index, random: &random)
            case 1, 4:
                section = prose(random: &random)
            case 2:
                section = list(random: &random)
            case 3:
                section = codeFence(random: &random)
            default:
                section = quotedLink(random: &random)
            }
            sections.append(section)
            bytes += section.utf8.count + 2
            index += 1
        }
        return sections.joined(separator: "\n\n")
    }

    // MARK: - Transcript streaming corpus

    /// The corpus the transcript-renderer spec's streaming fixture calls for:
    /// everything in ``stage37(minimumBytes:seed:)`` plus tables and images,
    /// cycling over eight section kinds.
    ///
    /// Tables and images matter because they are the two constructs the SwiftUI
    /// surface renders as dedicated block views rather than text — a table must
    /// not degrade to monospaced pipe text, and an image resolves
    /// asynchronously and must invalidate only its own block.
    public static func transcriptStreaming(minimumBytes: Int, seed: UInt64) -> String {
        var random = SeededRandom(state: seed)
        var sections: [String] = []
        var bytes = 0
        var index = 0
        while bytes < minimumBytes {
            let section: String
            switch index % 8 {
            case 0:
                section = heading(index: index, random: &random)
            case 1, 5:
                section = prose(random: &random)
            case 2:
                section = list(random: &random)
            case 3:
                section = codeFence(random: &random)
            case 4:
                section = table(random: &random)
            case 6:
                section = image(index: index, random: &random)
            default:
                section = quotedLink(random: &random)
            }
            sections.append(section)
            bytes += section.utf8.count + 2
            index += 1
        }
        return sections.joined(separator: "\n\n")
    }

    /// The corpus split across `count` turns totalling roughly `totalBytes`.
    ///
    /// A single long document and the same bytes spread over forty replies are
    /// different measurements: the second exercises structural reconciliation
    /// and per-turn identity, which is where transcript cost is supposed to stay
    /// independent of history length.
    public static func turns(count: Int, totalBytes: Int, seed: UInt64) -> [String] {
        precondition(count > 0, "a multi-turn corpus needs at least one turn")
        let perTurn = max(1, totalBytes / count)
        return (0..<count).map { turn in
            // Derive each turn's seed from the run seed so the whole corpus is
            // reproducible from one number, and so turn N's bytes do not shift
            // when `count` changes.
            transcriptStreaming(minimumBytes: perTurn, seed: seed &+ UInt64(turn) &* 0x9E37_79B9)
        }
    }

    // MARK: - Section kinds

    private static func heading(index: Int, random: inout SeededRandom) -> String {
        "## Section \(index): \(random.pick(words)) \(random.pick(words))"
    }

    private static func prose(random: inout SeededRandom) -> String {
        let sentences = (0..<(3 + random.below(4))).map { _ in
            "The \(random.pick(words)) meets the \(random.pick(words)) and the "
                + "\(random.pick(words)) stays \(random.pick(words))."
        }
        return sentences.joined(separator: " ")
    }

    private static func list(random: inout SeededRandom) -> String {
        (0..<(3 + random.below(3))).map { line in
            "- item \(line) holds `\(random.pick(words))` and moves on"
        }.joined(separator: "\n")
    }

    private static func codeFence(random: inout SeededRandom) -> String {
        let lines = (0..<(4 + random.below(8))).map { line in
            "    let value\(line) = compute(\(random.below(1000)))  // \(random.pick(words))"
        }
        return "```swift\nfunc run() {\n" + lines.joined(separator: "\n") + "\n}\n```"
    }

    private static func quotedLink(random: inout SeededRandom) -> String {
        "> Quoted: see [the \(random.pick(words))]"
            + "(https://example.org/wiki/Topic_(\(random.pick(words)))) for more."
    }

    /// A pipe table with a header rule — enough columns that it must scroll or
    /// compress at chat column width rather than wrapping into nonsense.
    private static func table(random: inout SeededRandom) -> String {
        let columns = 3 + random.below(2)
        let header = (0..<columns).map { _ in random.pick(words) }
        let rule = (0..<columns).map { _ in "---" }
        let rows = (0..<(2 + random.below(4))).map { row in
            (0..<columns).map { column in
                column == 0 ? "row \(row)" : "\(random.pick(words)) \(random.below(1000))"
            }
        }
        let render: ([String]) -> String = { "| " + $0.joined(separator: " | ") + " |" }
        return ([header, rule] + rows).map(render).joined(separator: "\n")
    }

    /// A local image reference plus its alt text. The path is deliberately
    /// relative: local-resource resolution is root-boundary enforced, and the
    /// failure path is as much a fixture as the success path.
    private static func image(index: Int, random: inout SeededRandom) -> String {
        "![a \(random.pick(words)) diagram](images/figure-\(index).png)"
    }
}
