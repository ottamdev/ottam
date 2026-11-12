import AppKit

/// Semantic custom attributes the styler stamps onto its `NSAttributedString`.
///
/// These attributes preserve semantic block identity for viewers, editors and
/// round-trip serialization. Values are ObjC-bridgeable because they live in
/// `NSAttributedString`.
public enum MarkdownAttribute {
    /// `NSNumber` wrapping `MarkdownBlockRole.rawValue`; spans the whole block.
    public static let blockRole = NSAttributedString.Key("ottam.md.blockRole")
    /// `NSNumber` — stable per-block index within one styled document. Blocks keep
    /// their ID when a document is restyled with an unchanged prefix.
    public static let blockID = NSAttributedString.Key("ottam.md.blockID")
    /// `NSNumber` — list nesting depth, 1-based. Present on list items only.
    public static let listDepth = NSAttributedString.Key("ottam.md.listDepth")
    /// `NSNumber` — enclosing blockquote levels, 1-based. Absent at depth zero.
    /// Orthogonal to `blockRole`: quoting is containment, so any role can carry it.
    public static let quoteDepth = NSAttributedString.Key("ottam.md.quoteDepth")
    /// `NSString` — normalized fence hint. Present on code blocks only.
    public static let codeLanguage = NSAttributedString.Key("ottam.md.codeLanguage")
    /// `NSNumber` wrapping `MarkdownInlineRole.rawValue`; marks inline runs.
    public static let inlineRole = NSAttributedString.Key("ottam.md.inlineRole")
}

public enum MarkdownBlockRole: Int, Sendable {
    case paragraph, heading, codeBlock, listItem, thematicBreak, table

    public init?(attributeValue: Any?) {
        guard let number = attributeValue as? NSNumber,
              let role = MarkdownBlockRole(rawValue: number.intValue) else { return nil }
        self = role
    }

    public init(_ kind: MarkdownBlockNode.Kind) {
        switch kind {
        case .paragraph: self = .paragraph
        case .heading: self = .heading
        case .codeBlock: self = .codeBlock
        case .listItem: self = .listItem
        case .thematicBreak: self = .thematicBreak
        case .table: self = .table
        }
    }
}

public extension MarkdownBlockNode {
    var role: MarkdownBlockRole { MarkdownBlockRole(kind) }
}

/// Vertical rhythm between adjacent blocks, decided by the pair of roles.
///
/// The gap belongs *above* a block and is chosen by what precedes it, so a
/// heading gets its section break only after prose, a list or a tile, and a
/// stacked `## A` / `### B` stays tight. One-sided by design: TextKit adds
/// `paragraphSpacing` and `paragraphSpacingBefore` rather than collapsing them,
/// and Core Text drops the trailing spacing of a frame's last paragraph, so a
/// renderer that lays blocks out as separate frames has to own the gap itself.
public struct MarkdownBlockSpacing: Equatable, Sendable {
    /// Prose after prose.
    public var paragraph: CGFloat = 10
    /// Consecutive list items, any depth.
    public var listItem: CGFloat = 3
    /// A heading after anything but another heading: the section break.
    public var beforeHeading: CGFloat = 18
    /// Anything after a heading, and a heading after a heading.
    public var afterHeading: CGFloat = 6
    /// Either side of a code block, table or rule, and prose after a list.
    public var tile: CGFloat = 12

    public init() {}

    /// Space to leave above `next` when `previous` sits directly before it.
    public func gap(from previous: MarkdownBlockRole, to next: MarkdownBlockRole) -> CGFloat {
        if previous == .heading { return afterHeading }
        if next == .heading { return beforeHeading }
        if previous.isTile || next.isTile { return tile }
        if previous == .listItem { return next == .listItem ? listItem : tile }
        return paragraph
    }

    /// What the gap needs to know about a block beyond its role: enough to
    /// tell two adjacent lists apart and to carry a quote bar across a gap.
    public struct Neighbor: Equatable, Hashable, Sendable {
        public var role: MarkdownBlockRole
        public var quoteDepth: Int
        /// List nesting, 1-based; zero for anything but a list item.
        public var listDepth: Int
        public var listIsOrdered: Bool

        public init(role: MarkdownBlockRole, quoteDepth: Int = 0, listDepth: Int = 0, listIsOrdered: Bool = false) {
            self.role = role
            self.quoteDepth = quoteDepth
            self.listDepth = listDepth
            self.listIsOrdered = listIsOrdered
        }

        public init(_ node: MarkdownBlockNode) {
            role = node.role
            quoteDepth = node.quoteDepth
            if case .listItem(let depth, let marker, _) = node.kind {
                listDepth = depth
                listIsOrdered = marker.isOrdered
            } else {
                listDepth = 0
                listIsOrdered = false
            }
        }
    }

    /// The role gap, except that a top-level item whose marker kind differs
    /// from the top-level item before it opens a new list and takes the tile
    /// gap: `1.` straight after a bullet list is two lists, not one.
    public func gap(from previous: Neighbor, to next: Neighbor) -> CGFloat {
        if previous.listDepth == 1, next.listDepth == 1, previous.listIsOrdered != next.listIsOrdered {
            return tile
        }
        return gap(from: previous.role, to: next.role)
    }

    /// Space that belongs *inside* `next` rather than between the two: when
    /// both blocks are quoted, the quote bar has to run through the gap, so
    /// the layout leaves no gap and the block draws it as a lead-in with the
    /// bar continued across it. Zero unless both sides are quoted prose:
    /// tiles (fences, tables, rules) draw no bar, so the bar breaks at them
    /// and the layout keeps the gap.
    public func quoteLeadIn(from previous: Neighbor, to next: Neighbor) -> CGFloat {
        guard previous.quoteDepth > 0, next.quoteDepth > 0,
              !previous.role.isTile, !next.role.isTile
        else { return 0 }
        return gap(from: previous, to: next)
    }
}

private extension MarkdownBlockRole {
    var isTile: Bool {
        switch self {
        case .codeBlock, .table, .thematicBreak: true
        case .paragraph, .heading, .listItem: false
        }
    }
}

public enum MarkdownInlineRole: Int, Sendable {
    case code
    /// The run carrying an inline image: one attachment character holding a
    /// `MarkdownImageAttachment`.
    case image

    public init?(attributeValue: Any?) {
        guard let number = attributeValue as? NSNumber,
              let role = MarkdownInlineRole(rawValue: number.intValue) else { return nil }
        self = role
    }
}
