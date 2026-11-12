import OttamBackendClient

/// The tile a named agent or a delegated child is drawn with: a palette
/// color and an upright shape, both picked by one 32-bit seed, so a seed
/// always draws the same tile. An agent product keeps its brand tile
/// instead; this is for personas over a product.
public struct AgentAvatar: Equatable, Hashable, Sendable {
    public enum Shape: Int, CaseIterable, Sendable {
        case circle, triangle, square, diamond, ring, halfMoon
    }

    /// How many colors the app's palette has; `paletteIndex` is below it.
    public static let paletteCount = 8

    public var seed: UInt32

    public init(seed: UInt32) {
        self.seed = seed
    }

    /// A stored seed, as the protocol carries it, or else one derived from
    /// `name`, as for a named agent saved before seeds existed.
    public init(seed: Int?, name: String) {
        self.init(seed: seed.map { UInt32(truncatingIfNeeded: $0) } ?? Self.seed(forName: name))
    }

    /// FNV-1a over the lowercased name: stable across launches, unlike
    /// `Hasher`.
    public static func seed(forName name: String) -> UInt32 {
        var hash: UInt32 = 2_166_136_261
        for byte in name.lowercased().utf8 {
            hash ^= UInt32(byte)
            hash = hash &* 16_777_619
        }
        return hash
    }

    /// A random seed, for Shuffle.
    public static func randomSeed() -> UInt32 {
        UInt32.random(in: .min ... .max)
    }

    /// The seed, mixed so neighboring seeds differ in every part (the
    /// murmur3 finalizer).
    private var mixed: UInt32 {
        var value = seed
        value ^= value >> 16
        value = value &* 0x85EB_CA6B
        value ^= value >> 13
        value = value &* 0xC2B2_AE35
        value ^= value >> 16
        return value
    }

    public var paletteIndex: Int { Int(mixed % UInt32(Self.paletteCount)) }

    public var shape: Shape {
        Shape.allCases[Int((mixed >> 8) % UInt32(Shape.allCases.count))]
    }
}

extension ConversationSummary {
    /// A delegated child's tile; nil for a conversation nothing delegated.
    public var delegatedAvatar: AgentAvatar? {
        guard let agent = delegatedAgentName else { return nil }
        return AgentAvatar(seed: delegatedAvatarSeed, name: agent)
    }
}

extension NamedAgent {
    public var avatar: AgentAvatar { AgentAvatar(seed: avatarSeed, name: name) }
}
