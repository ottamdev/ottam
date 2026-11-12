import Foundation

/// SplitMix64: tiny, seedable, and stable across runs — benchmark inputs must
/// not drift between checkouts, machines or Swift versions.
///
/// Lifted verbatim from the stage-3.7 streaming benchmark when the fixtures
/// moved out of the test target. The algorithm is fixed: changing it
/// invalidates every recorded baseline, because the corpus bytes are the
/// measurement's independent variable.
public struct SeededRandom {
    public var state: UInt64

    public init(state: UInt64) {
        self.state = state
    }

    public mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }

    public mutating func below(_ bound: Int) -> Int { Int(next() % UInt64(bound)) }

    public mutating func pick<T>(_ options: [T]) -> T { options[below(options.count)] }
}
