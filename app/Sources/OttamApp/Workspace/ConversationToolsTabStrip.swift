import OttamAccessibility
import OttamBackendClient
import SwiftUI

/// The tools pane's tabs, a toolbar item over the pane; Add Tool is the
/// toolbar item after it. The selected tab takes a weighted share of the width
/// under a sliding glass pill; the others split the rest and collapse to their
/// icon, and past that the strip scrolls.
/// See `docs/architecture/app/macos-ui.md`.
struct ConversationToolsTabStrip: View {
    /// The track's height, the toolbar's glass items' height.
    static let trackHeight = ToolTabStripLayout.tabHeight + 2 * trackInset
    private static let trackInset: CGFloat = 4

    let session: WorkspaceWindowSession
    @Namespace private var glass
    @State private var available: CGFloat = 0
    @State private var hovered: ConversationTool?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var conversationID: ConversationID? { session.chatWorkspace.selectedConversationID }

    private var state: ConversationToolsState {
        conversationID.map(session.tools.state(for:)) ?? ConversationToolsState()
    }

    private var motion: Animation? { reduceMotion ? nil : .smooth(duration: 0.35) }

    // The toolbar insets and centers the item.
    var body: some View {
        let state = self.state
        let layout = ToolTabStripLayout(
            available: available,
            selected: state.selected.flatMap(state.tools.firstIndex(of:))
        )
        return ScrollViewReader { proxy in
            ScrollView(.horizontal) {
                ZStack {
                    GlassEffectContainer {
                        layout {
                            ForEach(state.tools, id: \.self) { tool in
                                Color.clear.background { if tool == state.selected { pill } }
                            }
                        }
                    }
                    tabs(state, layout: layout)
                }
                // The tabs animate their own changes. A change made inside
                // `withAnimation` would animate the scroll view's content width
                // too, and scrolling to a tab added with the selection would
                // land short of it against the in-between width.
                .animation(motion, value: state.tools)
                .transaction(value: state.selected) { transaction in
                    transaction[ToolTabSelectionChange.self] = true
                    if !transaction.disablesAnimations { transaction.animation = motion }
                }
            }
            .scrollIndicators(.never)
            .frame(height: ToolTabStripLayout.tabHeight)
            .clipShape(Capsule())
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { available = $0 }
            .onChange(of: state.selected) { _, tool in
                guard let tool else { return }
                withAnimation(motion) { proxy.scrollTo(tool) }
            }
            .onChange(of: available) {
                if let tool = state.selected { proxy.scrollTo(tool) }
            }
            .onAppear {
                if let tool = state.selected { proxy.scrollTo(tool) }
            }
        }
        // Switching conversations swaps the tabs without animating, as the
        // pane snaps.
        .id(conversationID)
        .padding(Self.trackInset)
        .background(Capsule().fill(.quaternary))
    }

    private var pill: some View {
        Color.clear
            .glassEffect(.regular, in: .capsule)
            .matchedGeometryEffect(id: "selection", in: glass)
    }

    private func tabs(_ state: ConversationToolsState, layout: ToolTabStripLayout) -> some View {
        let widths = ToolTabStripLayout.widths(count: state.tools.count, selected: layout.selected, available: available)
        let titles = state.titles()
        return layout {
            ForEach(Array(state.tools.enumerated()), id: \.element) { index, tool in
                let selected = tool == state.selected
                let next = state.tools.indices.contains(index + 1) ? state.tools[index + 1] : nil
                ConversationToolTab(
                    tool: tool,
                    name: titles[tool] ?? tool.kind.title,
                    ordinal: state.ordinal(of: tool),
                    selected: selected,
                    collapsed: !selected && widths[index] < ConversationToolTab.labelMinimum,
                    hovered: hovered == tool,
                    separator: next != nil && !selected && next != state.selected
                        && hovered != tool && hovered != next,
                    select: { select(tool) },
                    close: { close(tool) }
                )
                .onHover { inside in
                    if inside {
                        hovered = tool
                    } else if hovered == tool {
                        hovered = nil
                    }
                }
                .id(tool)
                // A closed tab clears out before its neighbor slides over it.
                .transition(.asymmetric(
                    insertion: .opacity,
                    removal: AnyTransition.opacity.animation(.easeOut(duration: 0.1))
                ))
                .scrollTransition(.interactive, axis: .horizontal) { content, phase in
                    content
                        .scaleEffect(1 - 0.2 * abs(phase.value))
                        .opacity(1 - 0.6 * abs(phase.value))
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(.isTabBar)
        .accessibilityLabel("Tools")
        .accessibilityIdentifier(AccessibilityID.conversationToolsTabs)
    }

    private func select(_ tool: ConversationTool) {
        guard let conversationID else { return }
        session.selectTool(tool, in: conversationID)
    }

    private func close(_ tool: ConversationTool) {
        guard let conversationID else { return }
        session.closeTool(tool, in: conversationID)
    }
}

/// Gives the selected tab a weighted share of the visible width, capped
/// unless an even split is wider, and splits the rest evenly, never below an
/// icon-only width. Past that the tabs are wider than `available` and the
/// strip scrolls. A horizontal scroll view proposes no width, so a stack
/// inside it could not share one.
struct ToolTabStripLayout: Layout {
    var available: CGFloat
    var selected: Int?

    static let tabHeight: CGFloat = 28
    static let collapsedWidth: CGFloat = 44
    static let selectedWidths: ClosedRange<CGFloat> = 132...280
    /// The selected tab narrows to this before the strip scrolls.
    static let selectedFloor: CGFloat = 112
    static let selectedWeight: CGFloat = 1.6

    static func widths(count: Int, selected: Int?, available: CGFloat) -> [CGFloat] {
        guard count > 0 else { return [] }
        guard count > 1, let selected else {
            return Array(repeating: max(collapsedWidth, available / CGFloat(count)), count: count)
        }
        let others = CGFloat(count - 1)
        let weighted = available * selectedWeight / (selectedWeight + others)
        let room = available - others * collapsedWidth
        let preferred = min(max(weighted, selectedWidths.lowerBound), selectedWidths.upperBound)
        let width = max(room >= selectedFloor ? min(preferred, room) : preferred, available / CGFloat(count))
        let share = max(collapsedWidth, (available - width) / others)
        return (0..<count).map { $0 == selected ? width : share }
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let widths = Self.widths(count: subviews.count, selected: selected, available: available)
        return CGSize(width: widths.reduce(0, +), height: Self.tabHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let widths = Self.widths(count: subviews.count, selected: selected, available: available)
        var x = bounds.minX
        for (subview, width) in zip(subviews, widths) {
            subview.place(
                at: CGPoint(x: x, y: bounds.minY),
                proposal: ProposedViewSize(width: width, height: bounds.height)
            )
            x += width
        }
    }
}

/// One tab. Every state is one view tree whose modifiers change: swapping
/// views (an `if` or `ViewThatFits`) crossfades two copies under animation,
/// and the outgoing one stays frozen at its old frame.
struct ConversationToolTab: View {
    /// Below this a tab shows only its icon: the title would keep a few letters.
    static let labelMinimum: CGFloat = 76
    /// Room the close button takes at a tab's leading edge. The selected tab
    /// insets its label by this; another shows close only where its label
    /// already starts this far in.
    static let closeClearance: CGFloat = 28

    let tool: ConversationTool
    /// The kind's title, or a file tab's file name.
    let name: String
    let ordinal: Int?
    let selected: Bool
    let collapsed: Bool
    let hovered: Bool
    let separator: Bool
    let select: () -> Void
    let close: () -> Void
    /// Where the label starts in the tab.
    @State private var labelInset: CGFloat = 0
    private static let space = "ConversationToolTab"

    /// The selected tab always offers close. Another offers it under the
    /// pointer only beside its label, never over it: a narrow tab is selected
    /// first, as in Safari. The toolbar keeps right clicks for its own menu,
    /// so a tab has no context menu.
    static func showsClose(selected: Bool, labelInset: CGFloat, hovered: Bool) -> Bool {
        selected || (hovered && labelInset >= closeClearance)
    }

    private var title: String {
        ordinal.map { "\(name) \($0)" } ?? name
    }

    /// A file tab's tooltip is the full path, which its title shortens.
    private var help: String {
        if case .file(let file) = tool { file.path } else { title }
    }

    private var isFile: Bool { tool.kind == .file }

    var body: some View {
        let showsClose = Self.showsClose(selected: selected, labelInset: labelInset, hovered: hovered)
        ZStack(alignment: .leading) {
            Button(action: select) {
                HStack(spacing: 0) {
                    ConversationToolIcon(kind: tool.kind)
                        .frame(width: 16)
                    Text(name)
                        .lineLimit(1)
                        // A file tab's title ends in the folders that tell
                        // same-name files apart, so it shortens in the middle,
                        // as Finder shortens names.
                        .truncationMode(isFile ? .middle : .tail)
                        .padding(.leading, 5)
                        .frame(maxWidth: collapsed ? 0 : nil)
                        .opacity(collapsed ? 0 : 1)
                    if let ordinal {
                        Text("\(ordinal)")
                            .font(.caption2.monospacedDigit())
                            .padding(.leading, 2)
                            .frame(maxWidth: collapsed ? nil : 0)
                            .opacity(collapsed ? 1 : 0)
                    }
                }
                // The label moves as one: by default each leaf animates its
                // own frame, and the icon and title drift apart in the slide.
                .geometryGroup()
                .onGeometryChange(for: CGFloat.self) { $0.frame(in: .named(Self.space)).minX } action: {
                    labelInset = $0
                }
                .padding(.leading, selected ? Self.closeClearance : 8)
                .padding(.trailing, selected ? 12 : 8)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .foregroundStyle(selected ? .primary : .secondary)
            .help(help)
            .accessibilityLabel(title)
            .accessibilityAddTraits(selected ? .isSelected : [])
            .accessibilityIdentifier(AccessibilityID.conversationToolsTab)

            Button(role: .close, action: close) { Image(systemName: "xmark") }
                .buttonStyle(ToolTabCloseButtonStyle())
                .padding(.leading, selected ? 6 : 8)
                .transaction(Self.snapsOnSelection) { $0.opacity(showsClose ? 1 : 0) }
                .allowsHitTesting(showsClose)
                .accessibilityHidden(!showsClose)
                .accessibilityLabel("Close \(title)")
                .accessibilityIdentifier(AccessibilityID.conversationToolsTabClose)
        }
        .coordinateSpace(.named(Self.space))
        .background {
            Capsule().fill(.quinary)
                .transaction(Self.snapsOnSelection) { $0.opacity(hovered && !selected ? 1 : 0) }
        }
        .overlay(alignment: .trailing) {
            Rectangle().fill(.separator).frame(width: 1, height: 14).opacity(separator ? 1 : 0)
        }
        .animation(.easeOut(duration: 0.12), value: hovered)
    }

    /// The glass pill takes over the selected tab, so a hover fill it replaces,
    /// and the close of a tab it leaves, go at once rather than fading with
    /// the slide. Only their opacity: the close still slides with its tab.
    private static func snapsOnSelection(_ transaction: inout Transaction) {
        if transaction[ToolTabSelectionChange.self] { transaction.animation = nil }
    }
}

/// A kind of tool's icon, the same in its tabs and the picker.
struct ConversationToolIcon: View {
    let kind: ConversationToolKind

    var body: some View {
        Group {
            switch kind {
            case .review:
                Image(systemName: "plusminus")
            case .subagent:
                Image(systemName: "person.2")
            case .plan:
                Image(systemName: "list.bullet.clipboard")
            case .file:
                Image(systemName: "doc.text")
            case .terminal:
                // A size down, so the glyph matches a symbol's size and fits a
                // tab's 16pt icon slot; set tight, so it reads as one mark.
                Text(ConversationToolKind.terminalGlyph)
                    .font(.callout.monospaced())
                    .tracking(-1)
                    .fixedSize()
                    .accessibilityHidden(true)
            }
        }
        // Bold, so the icon stands apart from the title beside it.
        .fontWeight(.bold)
    }
}

/// Marks a transaction that changes the selected tab.
private struct ToolTabSelectionChange: TransactionKey {
    static let defaultValue = false
}

/// A bare ✕ that gains a circle and brightens under the pointer, as window
/// tabs draw it. `Button(role: .close)` draws the word "Close" outside a
/// toolbar, or a thin ✕ as an icon, so the role stays and the look is ours.
private struct ToolTabCloseButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Plated(configuration: configuration)
    }

    private struct Plated: View {
        let configuration: Configuration
        @State private var hovering = false

        var body: some View {
            configuration.label
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(hovering ? HierarchicalShapeStyle.primary : .secondary)
                .frame(width: 16, height: 16)
                .background {
                    Circle()
                        .fill(configuration.isPressed ? HierarchicalShapeStyle.tertiary : .quaternary)
                        .opacity(hovering ? 1 : 0)
                }
                .contentShape(Circle())
                .onHover { hovering = $0 }
                .animation(.easeOut(duration: 0.12), value: hovering)
        }
    }
}
