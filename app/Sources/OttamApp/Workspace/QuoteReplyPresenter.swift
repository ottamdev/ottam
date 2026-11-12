import AppKit
import OttamAccessibility
import OttamPresentationLogic
import SwiftUI

/// The responder-chain action behind the transcript's Reply… menu item and
/// shortcut. The collection view implements it; the menu, which lives in the
/// text item view, finds it by selector.
@MainActor @objc protocol TranscriptQuoteReplying {
    func replyToSelection(_ sender: Any?)
}

/// What a transcript host supplies to take quote replies. Without one the
/// transcript shows no reply surface and disables Reply….
struct TranscriptQuoteReplyConfiguration {
    var agentName: String
    /// Stages the comment in the host's composer, which sends it with the
    /// next message, so several replies can go out together.
    var onAdd: @MainActor (_ quote: String, _ comment: String) -> Void
}

/// Reply-in-place over the transcript: a segmented Reply | Copy capsule at the
/// selection that grows into a comment card.
///
/// AppKit owns where it is: the transcript places it on every scroll, in the
/// same pass that moves the text, and pins it to the visible band's edge once
/// the selection leaves. SwiftUI owns how it looks: one glass shape that
/// materializes, resizes from capsule to card, and dissolves, like the panels
/// above the composer. The glass carries no `glassEffectID` or union; those,
/// in a container around focusable controls, loop the focus engine's key-view
/// walk forever on macOS 26.
@MainActor
final class QuoteReplyPresenter {
    /// Covers the scroll view so placement is in its flipped coordinates, and
    /// passes every event that misses the surface through to the transcript.
    final class LayerView: NSView {
        override var isFlipped: Bool { true }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let view = super.hitTest(point)
            return view === self ? nil : view
        }
    }

    /// Larger than the surface so SwiftUI can grow the glass inside it; clicks
    /// on the empty rest go through to the transcript.
    final class HostingView: NSHostingView<QuoteReplyContent> {
        /// The shown surface, in this view's coordinates.
        var surfaceRect: NSRect = .zero

        override func hitTest(_ point: NSPoint) -> NSView? {
            guard surfaceRect.contains(convert(point, from: superview)) else { return nil }
            return super.hitTest(point)
        }
    }

    static let cardWidth: CGFloat = 360
    /// Room for the card at its tallest, so AppKit never resizes the hosting
    /// view while SwiftUI animates the glass inside it.
    private static let boxHeight: CGFloat = 320

    let layer = LayerView()
    var configuration: TranscriptQuoteReplyConfiguration? {
        didSet { state.agentName = configuration?.agentName ?? "" }
    }
    /// Scrolls the selection back into view, from the pinned capsule's arrow.
    var onReveal: () -> Void = {}
    /// The reply was staged: the host clears its selection, which hides this.
    var onAdded: () -> Void = {}

    private let hosting: HostingView
    private let state = QuoteReplyState()
    private var lastPlacement: (anchor: NSRect, band: NSRect)?

    var isShown: Bool { state.isVisible }
    var pin: QuoteReply.Pin { state.pin }
    /// The surface's resting frame in `layer`'s coordinates.
    var surfaceFrame: NSRect {
        hosting.surfaceRect.offsetBy(dx: hosting.frame.minX, dy: hosting.frame.minY)
    }

    init() {
        hosting = HostingView(rootView: QuoteReplyContent(state: state))
        hosting.sizingOptions = []
        hosting.sceneBridgingOptions = []
        layer.addSubview(hosting)
        hosting.rootView = QuoteReplyContent(
            state: state,
            onReply: { [weak self] in self?.compose() },
            onCopy: { [weak self] in
                guard let self else { return }
                WorkspacePasteboard.copy(state.quote)
            },
            onReveal: { [weak self] in self?.onReveal() },
            onAdd: { [weak self] in self?.add() },
            onDismiss: { [weak self] in self?.endComposing() },
            onSize: { [weak self] size in self?.contentSizeChanged(size) }
        )
    }

    /// Shows the capsule for a finished selection of `quote`.
    func show(quote: String) {
        if state.isVisible {
            // Content settling under the selection (a streaming reply, a
            // restyle) changes its text but not the reply being written. A
            // new gesture hides the surface first, so it never lands here.
            state.quote = quote
            return
        }
        // A hidden surface may still be dissolving as the card it was. Reset
        // without animation and start a fresh surface at the capsule's size.
        var reset = Transaction()
        reset.disablesAnimations = true
        withTransaction(reset) {
            state.quote = quote
            state.comment = ""
            state.isComposing = false
            state.side = nil
            state.contentSize = .zero
            state.generation += 1
        }
        withAnimation(.snappy) { state.isVisible = true }
    }

    func hide() {
        guard state.isVisible else { return }
        // Placement stops here; SwiftUI dissolves the glass where it stands.
        lastPlacement = nil
        hosting.surfaceRect = .zero
        withAnimation(.snappy) { state.isVisible = false }
    }

    /// Grows the capsule into the comment card.
    func compose() {
        guard state.isVisible, !state.isComposing else { return }
        withAnimation(.snappy(duration: 0.24)) { state.isComposing = true }
    }

    /// Places the surface for the selection `anchor`, inside the unobscured
    /// `band`, both in `layer`'s coordinates.
    func place(anchor: NSRect, band: NSRect) {
        guard isShown else { return }
        lastPlacement = (anchor, band)
        apply()
    }

    private var surfaceSize: CGSize {
        state.contentSize == .zero ? CGSize(width: 120, height: 36) : state.contentSize
    }

    private func apply() {
        guard let (anchor, band) = lastPlacement else { return }
        let size = surfaceSize
        let placement = QuoteReply.placement(anchor: anchor, band: band, size: size, side: state.side)
        if state.side != placement.side { state.side = placement.side }
        if state.pin != placement.pin { state.pin = placement.pin }
        // The box keeps the surface's edge nearest the selection still, so a
        // resize grows away from the selection.
        let box = NSSize(width: max(Self.cardWidth, size.width), height: max(Self.boxHeight, size.height))
        let above = placement.side == .above
        let frame = NSRect(
            x: placement.origin.x,
            y: above ? placement.origin.y + size.height - box.height : placement.origin.y,
            width: box.width,
            height: box.height
        )
        if hosting.frame != frame { hosting.frame = frame }
        hosting.surfaceRect = NSRect(
            origin: CGPoint(x: 0, y: above ? box.height - size.height : 0),
            size: size
        )
    }

    private func contentSizeChanged(_ size: CGSize) {
        guard size != state.contentSize, size.width > 0, size.height > 0 else { return }
        state.contentSize = size
        apply()
    }

    private func endComposing() {
        // Losing focus to a dismissal is not a return to the capsule.
        guard state.isVisible, state.isComposing else { return }
        withAnimation(.snappy(duration: 0.24)) {
            state.isComposing = false
            state.comment = ""
        }
    }

    private func add() {
        let comment = state.comment.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let configuration, !comment.isEmpty else { return }
        configuration.onAdd(state.quote, comment)
        onAdded()
    }
}

@MainActor @Observable
final class QuoteReplyState {
    var quote = ""
    var comment = ""
    var agentName = ""
    var isVisible = false
    var isComposing = false
    var pin: QuoteReply.Pin = .none
    /// The side of the selection the surface took when it appeared.
    var side: QuoteReply.Side?
    /// The surface's natural size, measured by SwiftUI.
    var contentSize: CGSize = .zero
    /// Counts appearances, so each one is a new surface rather than the
    /// last one called back mid-dissolve.
    var generation = 0
}

/// The capsule and the card on their glass.
struct QuoteReplyContent: View {
    let state: QuoteReplyState
    var onReply: () -> Void = {}
    var onCopy: () -> Void = {}
    var onReveal: () -> Void = {}
    var onAdd: () -> Void = {}
    var onDismiss: () -> Void = {}
    var onSize: (CGSize) -> Void = { _ in }

    /// The old content leaves at once, so the glass resizes empty, and the new
    /// content fades in once it has settled. Content shown during the resize
    /// is clipped by the growing glass and reads as a wipe.
    private static let contentTransition = AnyTransition.asymmetric(
        insertion: .opacity.animation(.easeOut(duration: 0.16).delay(0.2)),
        removal: .identity
    )

    private static let addButtonSize: CGFloat = 24

    @FocusState private var fieldFocused: Bool

    private var canAdd: Bool {
        !state.comment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var alignment: Alignment {
        state.side == .above ? .bottomLeading : .topLeading
    }

    var body: some View {
        GlassEffectContainer {
            if state.isVisible {
                surface
                    .id(state.generation)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: alignment)
    }

    private var surface: some View {
        let shape = RoundedRectangle(cornerRadius: state.isComposing ? 16 : 18, style: .continuous)
        return ZStack(alignment: alignment) {
            if state.isComposing {
                card
                    .transition(Self.contentTransition)
            } else {
                pill
                    .transition(Self.contentTransition)
            }
        }
        .clipShape(shape)
        .glassEffect(.regular, in: shape)
        .glassEffectTransition(.materialize)
        .contentShape(shape)
        .onGeometryChange(for: CGSize.self) { $0.size } action: { onSize($0) }
    }

    private var pill: some View {
        HStack(spacing: 0) {
            if state.pin != .none {
                revealButton
                    .padding(.leading, 6)
                Divider()
                    .frame(height: 16)
            }
            segment("Reply", action: onReply)
                .accessibilityIdentifier(AccessibilityID.chatQuoteReplyButton)
            Divider()
                .frame(height: 16)
            segment("Copy", action: onCopy)
                .accessibilityIdentifier(AccessibilityID.chatQuoteReplyCopy)
        }
        .fixedSize()
    }

    private var revealButton: some View {
        Button(action: onReveal) {
            Image(systemName: state.pin == .top ? "chevron.up" : "chevron.down")
                .fontWeight(.semibold)
                .frame(width: 24, height: 36)
                .contentShape(.rect)
        }
        .buttonStyle(QuoteReplySegmentStyle())
        .accessibilityLabel("Show Selection")
        .accessibilityIdentifier(AccessibilityID.chatQuoteReplyReveal)
    }

    private func segment(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .appFont(.callout)
                .padding(.horizontal, 14)
                .frame(height: 36)
                .contentShape(.rect)
        }
        .buttonStyle(QuoteReplySegmentStyle())
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 10) {
            // Centered, not baseline-aligned: the bar has no baseline, so it
            // would sit on the text's baseline and ride high.
            HStack(alignment: .center, spacing: 8) {
                Capsule()
                    .fill(.tertiary)
                    .frame(width: 2.5, height: 14)
                Text(QuoteReply.headerText(for: state.quote))
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                if state.pin != .none {
                    Spacer(minLength: 0)
                    Button(action: onReveal) {
                        Image(systemName: state.pin == .top ? "chevron.up" : "chevron.down")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel("Show Selection")
                    .accessibilityIdentifier(AccessibilityID.chatQuoteReplyReveal)
                }
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Reply to \(state.agentName)", text: Bindable(state).comment, axis: .vertical)
                    .textFieldStyle(.plain)
                    .appFont(.body)
                    .lineLimit(1...5)
                    // The button's height, so one line centers on it; more
                    // lines grow up and leave the button at the bottom.
                    .frame(minHeight: Self.addButtonSize)
                    .focused($fieldFocused)
                    .onSubmit(onAdd)
                    .onKeyPress(.escape) {
                        onDismiss()
                        return .handled
                    }
                    .accessibilityIdentifier(AccessibilityID.chatQuoteReplyField)
                // The composer's send button, scaled to the card's one-line
                // field. It adds rather than sends: the composer sends.
                Button(action: onAdd) {
                    ZStack {
                        Circle()
                            .fill(canAdd ? AnyShapeStyle(.primary) : AnyShapeStyle(.tertiary))
                            .frame(width: Self.addButtonSize, height: Self.addButtonSize)
                        Image(systemName: "plus")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Color(nsColor: .windowBackgroundColor))
                    }
                }
                .buttonStyle(.plain)
                .disabled(!canAdd)
                .help("Add to Message")
                .accessibilityLabel("Add to Message")
                .accessibilityIdentifier(AccessibilityID.chatQuoteReplyAdd)
            }
        }
        .padding(14)
        .frame(width: QuoteReplyPresenter.cardWidth, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: fieldFocused) { _, focused in
            // Clicking away (the composer, the transcript) closes the card.
            if !focused { onDismiss() }
        }
        // The transcript holds first responder until the field is in the
        // window; focusing on appear is too early and silently does nothing.
        .task {
            await Task.yield()
            fieldFocused = true
        }
    }
}

/// A segment of the capsule, highlighted under the pointer the way toolbar
/// items on glass are. Custom because that highlight belongs to the toolbar;
/// no public button style draws it for a button on its own glass.
private struct QuoteReplySegmentStyle: ButtonStyle {
    @State private var isHovered = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background {
                Capsule()
                    .fill(.primary.opacity(configuration.isPressed ? 0.16 : isHovered ? 0.08 : 0))
                    .padding(3)
            }
            .onHover { isHovered = $0 }
            .animation(.easeOut(duration: 0.12), value: isHovered)
    }
}
