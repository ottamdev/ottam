import AppKit
import SwiftUI

/// Native text layout avoids materializing thousands of SwiftUI accessibility
/// paragraphs for one bounded payload page.
struct ChatPayloadTextView: NSViewRepresentable {
    static let inputMaximumLines = 10
    static let outputMaximumLines = 20
    private static let textInset = NSSize(width: 8, height: 8)
    private static let lineFragmentPadding: CGFloat = 5

    let text: String
    let font: NSFont
    let maximumLines: Int
    let followsEnd: Bool
    let unsaved: Bool
    let onScroll: (Bool) -> Void
    let onNextPage: () -> Void
    var accessibilityID = "chat.payload.text"

    static func height(forLines lines: Int, font: NSFont) -> CGFloat {
        ceil(NSLayoutManager().defaultLineHeight(for: font) * CGFloat(lines)) + 2 * textInset.height
    }

    /// Matches the text view's wrapping, but lays out only the lines that can
    /// affect the capped viewport. This also gives card measurement a bounded
    /// way to distinguish growing content from content already at the cap.
    static func height(for text: String, width: CGFloat, font: NSFont, maximumLines: Int) -> CGFloat {
        let maximumHeight = height(forLines: maximumLines, font: font)
        guard !text.isEmpty, width > 0 else { return height(forLines: 1, font: font) }
        let storage = NSTextStorage(string: text, attributes: [.font: font])
        let layout = NSLayoutManager()
        let container = NSTextContainer(size: NSSize(
            width: max(width - 2 * textInset.width, 1),
            height: CGFloat.greatestFiniteMagnitude
        ))
        container.lineFragmentPadding = lineFragmentPadding
        container.maximumNumberOfLines = max(maximumLines, 1)
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
        layout.ensureLayout(for: container)
        let contentHeight = ceil(layout.usedRect(for: container).height) + 2 * textInset.height
        return min(maximumHeight, max(height(forLines: 1, font: font), contentHeight))
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView scroll: NSScrollView, context: Context) -> CGSize? {
        let width = proposal.width ?? scroll.bounds.width
        guard width > 0 else { return nil }
        return CGSize(
            width: width,
            height: Self.height(for: text, width: width, font: font, maximumLines: maximumLines)
        )
    }

    final class Coordinator {
        var observer: NSObjectProtocol?
        var updating = false
        var wasUnsaved = false
        var onScroll: (Bool) -> Void = { _ in }
        var onNextPage: () -> Void = {}
        deinit { if let observer { NotificationCenter.default.removeObserver(observer) } }
    }
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        let view = NSTextView(frame: .zero)
        view.isEditable = false
        view.isSelectable = true
        view.isRichText = false
        view.drawsBackground = false
        view.font = font
        view.textColor = .labelColor
        view.textContainerInset = Self.textInset
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.textContainer?.widthTracksTextView = true
        view.textContainer?.lineFragmentPadding = Self.lineFragmentPadding
        view.textContainer?.containerSize = NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        view.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        view.setAccessibilityIdentifier(accessibilityID)
        scroll.documentView = view
        scroll.contentView.postsBoundsChangedNotifications = true
        let coordinator = context.coordinator
        coordinator.observer = NotificationCenter.default.addObserver(forName: NSView.boundsDidChangeNotification, object: scroll.contentView, queue: .main) { [weak scroll, weak coordinator] _ in
            MainActor.assumeIsolated {
                guard let scroll, let coordinator, !coordinator.updating else { return }
                let atEnd = scroll.documentVisibleRect.maxY >= (scroll.documentView?.bounds.height ?? 0) - 24
                coordinator.onScroll(atEnd)
                if atEnd { coordinator.onNextPage() }
            }
        }
        return scroll
    }
    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        coordinator.onScroll = onScroll
        coordinator.onNextPage = onNextPage
        guard let view = scroll.documentView as? NSTextView else { return }
        guard view.string != text || view.font != font else { coordinator.wasUnsaved = unsaved; return }
        coordinator.updating = true
        let origin = scroll.documentVisibleRect.origin
        if view.string != text { view.string = text }
        view.font = font
        if let container = view.textContainer { view.layoutManager?.ensureLayout(for: container) }
        if followsEnd && (unsaved || coordinator.wasUnsaved) { view.scrollToEndOfDocument(nil) }
        else { scroll.contentView.scroll(to: origin); scroll.reflectScrolledClipView(scroll.contentView) }
        coordinator.updating = false
        coordinator.wasUnsaved = unsaved
    }
}
