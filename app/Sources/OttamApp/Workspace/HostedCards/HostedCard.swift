import AppKit
import OttamMarkdownEngine
import SwiftUI

/// The collection renderer's card boundary. Cards are AppKit views with
/// deterministic height; they are not text attachments and do not participate
/// in TextKit layout.
@MainActor
protocol TranscriptCard: AnyObject {
    var hostedView: NSView? { get set }
    func makeHostedView() -> NSView
    func measuredHeight(forWidth width: CGFloat) -> CGFloat
    func adoptPayload(from fresh: any TranscriptCard) -> Bool
    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? { get }
}

extension TranscriptCard {
    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? { nil }
}

struct TranscriptCardAccessibilityPlaceholder: Equatable, Sendable {
    var identifier: String
    var value: String?

    init(identifier: String, value: String? = nil) {
        self.identifier = identifier
        self.value = value
    }
}

/// A card is a value payload plus callbacks into the model. View-local state
/// that affects height belongs in the payload so measurement and rendering
/// always agree.
protocol HostedCardPayload: Equatable {
    associatedtype Body: View
    associatedtype Actions
    var signature: AnyHashable { get }
    /// What the measured height depends on. A change to `signature` alone
    /// redraws the card without measuring it again.
    @MainActor var heightSignature: AnyHashable { get }
    /// Width-sensitive content can provide a more precise signature. The
    /// default keeps the original width-independent behavior.
    @MainActor func heightSignature(forWidth width: CGFloat, environment: HostedCardEnvironment) -> AnyHashable
    func makeBody(actions: Actions) -> Body
    mutating func adopt(_ fresh: Self)
    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? { get }
    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>)
    @MainActor static func didBindActions(to card: HostedCard<Self>)
}

extension HostedCardPayload {
    @MainActor var heightSignature: AnyHashable { signature }
    @MainActor func heightSignature(forWidth width: CGFloat, environment: HostedCardEnvironment) -> AnyHashable { heightSignature }
    mutating func adopt(_ fresh: Self) { self = fresh }
    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? { nil }
    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {}
    @MainActor static func didBindActions(to card: HostedCard<Self>) {}
}

struct HostedCardEnvironment: Equatable {
    var typography: AppTypography.Resolution
}

@MainActor
protocol HostedCardStateInheriting {
    func inheritState(from other: any TranscriptCard)
}

/// Stateful SwiftUI card host retained by transcript item ID. It deliberately
/// has no NSTextAttachment superclass: the native collection owns its geometry,
/// reuse and accessibility lifecycle directly.
@MainActor
final class HostedCard<Payload: HostedCardPayload>: TranscriptCard, HostedCardStateInheriting {
    private(set) var payload: Payload
    var actions: Payload.Actions
    let theme: MarkdownTheme
    let environment: HostedCardEnvironment
    weak var hostedView: NSView?
    var onPayloadChanged: ((HostedCard) -> Void)?

    private var cachedHeight: (key: HeightKey, height: CGFloat)?
    /// Retained across measurements; building a hosting controller per
    /// measurement dominated streaming updates.
    private var measuringHost: NSHostingController<AnyView>?
    /// Measurements taken, for tests.
    private(set) var measurementCount = 0

    private struct HeightKey: Hashable {
        let signature: AnyHashable
        let width: CGFloat
        let theme: String
    }

    init(
        payload: Payload,
        actions: Payload.Actions,
        theme: MarkdownTheme,
        environment: HostedCardEnvironment
    ) {
        self.payload = payload
        self.actions = actions
        self.theme = theme
        self.environment = environment
        Payload.bindActions(&self.actions, to: self)
        Payload.didBindActions(to: self)
    }

    func makeRoot() -> AnyView {
        AnyView(
            payload.makeBody(actions: actions)
                .appTypographyRoot(environment.typography)
                .transaction { $0.animation = nil }
        )
    }

    func updatePayload(_ change: (inout Payload) -> Void) {
        let before = payload
        change(&payload)
        guard payload != before else { return }
        (hostedView as? HostedCardView<Payload>)?.setRoot(makeRoot())
        // Reference-backed state (the detail store) has already changed in
        // `before`, so compare with what was last measured. A change that
        // leaves the height alone needs no transcript layout.
        if let cachedHeight,
           cachedHeight.key.signature == payload.heightSignature(forWidth: cachedHeight.key.width, environment: environment) { return }
        cachedHeight = nil
        onPayloadChanged?(self)
    }

    func inheritState(from other: any TranscriptCard) {
        guard let other = other as? HostedCard<Payload>, other !== self else { return }
        var inherited = other.payload
        inherited.adopt(payload)
        payload = inherited
        cachedHeight = nil
        // Reference-backed state can still have callbacks bound to the old card.
        // Reconfigure only after releasing exclusive access to the payload.
        Payload.didBindActions(to: self)
        (hostedView as? HostedCardView<Payload>)?.setRoot(makeRoot())
    }

    func makeHostedView() -> NSView {
        let view = HostedCardView<Payload>(root: makeRoot())
        hostedView = view
        return view
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        payload.accessibilityPlaceholder
    }

    func measuredHeight(forWidth width: CGFloat) -> CGFloat {
        let key = HeightKey(signature: payload.heightSignature(forWidth: width, environment: environment), width: width, theme: theme.signature)
        if let cachedHeight, cachedHeight.key == key { return cachedHeight.height }
        measurementCount += 1
        // A fresh identity per measurement: observable state read through the
        // payload (the detail store) only invalidates a retained graph on the
        // next transaction, so a reused graph would measure stale content.
        let root = AnyView(makeRoot().id(measurementCount))
        let host = measuringHost ?? HostedCardMeasurer.host(for: root)
        if measuringHost == nil { measuringHost = host } else { host.rootView = root }
        let height = HostedCardMeasurer.height(of: host, width: width)
        cachedHeight = (key, height)
        return height
    }

    func height(forWidth width: CGFloat) -> CGFloat {
        measuredHeight(forWidth: width)
    }

    func adoptPayload(from fresh: any TranscriptCard) -> Bool {
        guard let fresh = fresh as? HostedCard<Payload> else { return false }
        let before = payload
        payload.adopt(fresh.payload)
        actions = fresh.actions
        Payload.bindActions(&actions, to: self)
        // Configuration may synchronously refresh the host and read its actions.
        // Run it only after bindActions releases exclusive access to actions.
        Payload.didBindActions(to: self)
        guard payload != before else { return false }
        if let cachedHeight,
           cachedHeight.key.signature != payload.heightSignature(forWidth: cachedHeight.key.width, environment: environment) {
            self.cachedHeight = nil
        }
        (hostedView as? HostedCardView<Payload>)?.setRoot(makeRoot())
        return true
    }
}

/// The native collection item's view for a SwiftUI card. The card body owns
/// its chrome; this host only supplies lifecycle and exact bounds.
final class HostedCardView<Payload: HostedCardPayload>: NSView {
    private let hostingView: NSHostingView<AnyView>

    init(root: AnyView) {
        hostingView = NSHostingView(rootView: root)
        super.init(frame: .zero)
        hostingView.sizingOptions = []
        hostingView.translatesAutoresizingMaskIntoConstraints = true
        hostingView.autoresizingMask = [.width, .height]
        addSubview(hostingView)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    func setRoot(_ root: AnyView) {
        hostingView.rootView = root
    }

    override func layout() {
        super.layout()
        hostingView.frame = bounds
    }
}

@MainActor
enum HostedCardMeasurer {
    static func height(of root: AnyView, width: CGFloat) -> CGFloat {
        height(of: host(for: root), width: width)
    }

    static func host(for root: AnyView) -> NSHostingController<AnyView> {
        let controller = NSHostingController(rootView: root)
        controller.sizingOptions = []
        return controller
    }

    static func height(of controller: NSHostingController<AnyView>, width: CGFloat) -> CGFloat {
        controller.sizeThatFits(
            in: NSSize(width: width, height: CGFloat.greatestFiniteMagnitude)
        ).height.rounded(.up)
    }
}
