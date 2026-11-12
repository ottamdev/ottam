import AppKit
import OttamAccessibility
import OSLog
import SwiftUI

/// Stage-one markdown editor bridge: plain text today, with an AppKit-owned
/// editing surface ready for attributed markdown projection later.
///
/// SwiftUI remains the source of truth for the canonical markdown string.
/// AppKit owns text input, selection, undo, first-responder behavior, and
/// intrinsic-height measurement.
struct MarkdownTextView: NSViewRepresentable {
    @Environment(\.appTypography) private var typography

    @Binding var markdown: String
    @Binding var isFocused: Bool
    @Binding var measuredHeight: CGFloat

    let placeholder: String
    var minimumLines = 1
    var maximumLines = 10
    let onSubmit: () -> Void
    /// Command-Return. Separate from `onSubmit` because the two gestures mean
    /// different things while a run is working: Return queues the message
    /// behind the turn, Command-Return steers it into the turn.
    var onSteer: (() -> Void)?
    var onEscape: (() -> Bool)?
    /// Called before a paste reaches the text system. Returning true means the
    /// pasteboard held an image the composer claimed as an attachment, so the
    /// paste must not also fall through to text insertion.
    var onAttach: ((NSPasteboard) -> Bool)?
    /// Unmodified ↑/↓, Return, Tab, and Esc, offered first to a card that
    /// lists choices for the draft. Returning true consumes the key.
    var onPickerKey: ((ComposerPickerKey) -> Bool)?
    /// Grey text drawn after the draft, such as a command's argument hint.
    var inlineHint: String?

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let contentStorage = NSTextContentStorage()
        let layoutManager = NSTextLayoutManager()
        contentStorage.addTextLayoutManager(layoutManager)

        let textContainer = NSTextContainer(
            size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        )
        textContainer.widthTracksTextView = true
        layoutManager.textContainer = textContainer

        let textView = ComposerTextView(frame: .zero, textContainer: textContainer)
        let font = typography.nsFont(.body)
        textView.delegate = context.coordinator
        textView.font = font
        textView.textColor = .labelColor
        textView.insertionPointColor = .labelColor
        textView.typingAttributes = [
            .font: font,
            .foregroundColor: NSColor.labelColor,
        ]
        textView.drawsBackground = false
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.isEditable = true
        textView.isSelectable = true
        textView.isHorizontallyResizable = false
        textView.isVerticallyResizable = true
        textView.autoresizingMask = [.width]
        textView.textContainerInset = .zero
        textView.minSize = .zero
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.placeholder = placeholder
        textView.string = markdown
        textView.configureForLiteralMarkdownInput()
        textView.setAccessibilityIdentifier(AccessibilityID.chatComposer)
        textView.setAccessibilityLabel(placeholder)
        textView.setAccessibilityPlaceholderValue(placeholder)

        let scrollView = NSScrollView()
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = false
        scrollView.backgroundColor = .clear
        scrollView.hasHorizontalScroller = false
        scrollView.hasVerticalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.horizontalScrollElasticity = .none
        scrollView.documentView = textView

        context.coordinator.contentStorage = contentStorage
        context.coordinator.layoutManager = layoutManager
        context.coordinator.textContainer = textContainer
        context.coordinator.textView = textView
        context.coordinator.scrollView = scrollView
        textView.onSubmit = { [weak coordinator = context.coordinator] in
            coordinator?.submit()
        }
        textView.onSteer = { [weak coordinator = context.coordinator] in
            coordinator?.parent.onSteer?()
        }
        textView.onEscape = { [weak coordinator = context.coordinator] in
            coordinator?.parent.onEscape?() ?? false
        }
        textView.onWidthChange = { [weak coordinator = context.coordinator] in
            coordinator?.scheduleHeightMeasurement()
        }
        textView.onAttach = { [weak coordinator = context.coordinator] pasteboard in
            coordinator?.parent.onAttach?(pasteboard) ?? false
        }
        textView.onPickerKey = { [weak coordinator = context.coordinator] key in
            coordinator?.parent.onPickerKey?(key) ?? false
        }
        // NSTextView claims file drags for itself and swallows them before
        // SwiftUI's drop target on the composer card ever sees them. Text is the
        // only thing the field should accept directly.
        textView.unregisterDraggedTypes()
        textView.registerForDraggedTypes([.string])

        context.coordinator.scheduleHeightMeasurement()
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self

        guard let textView = coordinator.textView else { return }
        textView.configureForLiteralMarkdownInput()
        textView.placeholder = placeholder
        textView.inlineHint = inlineHint
        // The composer is content, so it follows the UI font — and AppKit cannot
        // read the environment for itself.
        let font = typography.nsFont(.body)
        if textView.font != font {
            textView.font = font
            textView.typingAttributes[.font] = font
            textView.needsDisplay = true
        }
        textView.setAccessibilityIdentifier(AccessibilityID.chatComposer)
        textView.setAccessibilityLabel(placeholder)
        textView.setAccessibilityPlaceholderValue(placeholder)

        if textView.string != markdown {
            let selection = textView.selectedRange()
            // A caret at the end stays at the end, so a picked command
            // completes the draft with the caret after it.
            let caretAtEnd = selection.length == 0 && selection.location == (textView.string as NSString).length
            coordinator.isApplyingExternalText = true
            textView.string = markdown
            coordinator.isApplyingExternalText = false
            let newLength = (markdown as NSString).length
            let location = caretAtEnd ? newLength : min(selection.location, newLength)
            let length = caretAtEnd ? 0 : min(selection.length, newLength - location)
            textView.setSelectedRange(NSRange(location: location, length: length))
            textView.needsDisplay = true
        }

        coordinator.scheduleHeightMeasurement()

        if isFocused, textView.window?.firstResponder !== textView {
            DispatchQueue.main.async { [weak coordinator] in
                guard let coordinator,
                      coordinator.parent.isFocused,
                      let textView = coordinator.textView,
                      let window = textView.window
                else { return }
                window.makeFirstResponder(textView)
            }
        }
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: MarkdownTextView
        var contentStorage: NSTextContentStorage?
        var layoutManager: NSTextLayoutManager?
        var textContainer: NSTextContainer?
        fileprivate weak var textView: ComposerTextView?
        weak var scrollView: NSScrollView?
        var isApplyingExternalText = false
        private var heightMeasurementScheduled = false
        private let measurementContentStorage = NSTextContentStorage()
        private let measurementLayoutManager = NSTextLayoutManager()
        private let measurementTextContainer = NSTextContainer(
            size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        )

        init(parent: MarkdownTextView) {
            self.parent = parent
            measurementContentStorage.addTextLayoutManager(measurementLayoutManager)
            measurementTextContainer.lineFragmentPadding = 0
            measurementLayoutManager.textContainer = measurementTextContainer
        }

        func textDidChange(_ notification: Notification) {
            guard !isApplyingExternalText,
                  let textView = notification.object as? NSTextView
            else { return }
            parent.markdown = textView.string
            textView.needsDisplay = true
            scheduleHeightMeasurement()
        }

        func textDidBeginEditing(_ notification: Notification) {
            updateFocus(true)
        }

        func textDidEndEditing(_ notification: Notification) {
            updateFocus(false)
        }

        func submit() {
            parent.onSubmit()
        }

        func scheduleHeightMeasurement() {
            guard !heightMeasurementScheduled else { return }
            heightMeasurementScheduled = true
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.heightMeasurementScheduled = false
                self.measureHeight()
            }
        }

        private func updateFocus(_ focused: Bool) {
            guard parent.isFocused != focused else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.parent.isFocused != focused else { return }
                self.parent.isFocused = focused
            }
        }

        private func measureHeight() {
            guard let textView,
                  let scrollView,
                  let textContainer,
                  let font = textView.font
            else { return }

            let width = max(scrollView.contentSize.width, 1)
            if abs(textView.frame.width - width) > 0.5 {
                textView.setFrameSize(NSSize(width: width, height: textView.frame.height))
            }
            if abs(textContainer.size.width - width) > 0.5 {
                textContainer.size = NSSize(
                    width: width,
                    height: CGFloat.greatestFiniteMagnitude
                )
            }

            let lineHeight = ceil(font.ascender - font.descender + font.leading)
            let minimumHeight = lineHeight * CGFloat(max(parent.minimumLines, 1))
            let maximumHeight = lineHeight * CGFloat(
                max(parent.maximumLines, parent.minimumLines)
            )
            let textWidth = max(
                width - (textContainer.lineFragmentPadding * 2),
                1
            )
            let contentHeight = measuredContentHeight(
                textView.string,
                font: font,
                width: textWidth,
                lineHeight: lineHeight,
                stopAfter: maximumHeight
            )
            let clampedHeight = min(max(contentHeight, minimumHeight), maximumHeight)
            let overflows = contentHeight > maximumHeight + 0.5

            // AppKit owns the document view's full height and TextKit viewport layout.
            // We only establish the visible minimum/capped height; forcing layout over
            // `documentRange` here defeats TextKit 2's laziness for large pasted prompts.
            if !overflows || textView.frame.height < clampedHeight {
                textView.setFrameSize(NSSize(width: width, height: clampedHeight))
            }
            scrollView.hasVerticalScroller = overflows

            if abs(parent.measuredHeight - clampedHeight) > 0.5 {
                parent.measuredHeight = clampedHeight
            }
        }

        /// Measures only as much leading text as needed to prove the editor has
        /// reached its visible line cap. Normal drafts are measured once in full;
        /// large inputs stop after a bounded prefix instead of laying out the whole
        /// TextKit document on the main actor.
        private func measuredContentHeight(
            _ text: String,
            font: NSFont,
            width: CGFloat,
            lineHeight: CGFloat,
            stopAfter maximumHeight: CGFloat
        ) -> CGFloat {
            let source = text as NSString
            guard source.length > 0 else { return lineHeight }

            measurementTextContainer.size = NSSize(
                width: width,
                height: CGFloat.greatestFiniteMagnitude
            )
            measurementTextContainer.maximumNumberOfLines =
                max(parent.maximumLines, parent.minimumLines) + 1

            var measuredLength = min(source.length, 4_096)
            while true {
                // Avoid ending a bounded UTF-16 prefix between a surrogate pair.
                if measuredLength < source.length,
                   measuredLength > 0,
                   (0xD800...0xDBFF).contains(Int(source.character(at: measuredLength - 1))) {
                    measuredLength -= 1
                }

                let sample = source.substring(to: measuredLength)
                measurementContentStorage.textStorage?.setAttributedString(
                    NSAttributedString(string: sample, attributes: [.font: font])
                )
                measurementLayoutManager.ensureLayout(
                    for: measurementLayoutManager.documentRange
                )
                let height = ceil(
                    max(measurementLayoutManager.usageBoundsForTextContainer.maxY, lineHeight)
                )
                if height > maximumHeight + 0.5 || measuredLength == source.length {
                    return height
                }
                measuredLength = min(measuredLength * 2, source.length)
            }
        }
    }
}

final class ComposerTextView: NSTextView {
    var placeholder = "" {
        didSet {
            if oldValue != placeholder {
                needsDisplay = true
            }
        }
    }

    var inlineHint: String? {
        didSet {
            if oldValue != inlineHint {
                needsDisplay = true
            }
        }
    }

    var onSubmit: (() -> Void)?
    var onSteer: (() -> Void)?
    var onEscape: (() -> Bool)?
    var onWidthChange: (() -> Void)?
    var onAttach: ((NSPasteboard) -> Bool)?
    var onPickerKey: ((ComposerPickerKey) -> Bool)?

    /// Image types the composer claims out-of-band. A ⌘⌃⇧4 screenshot puts
    /// *only* TIFF on the pasteboard.
    private static let attachableTypes: [NSPasteboard.PasteboardType] = [.png, .tiff, .fileURL]

    /// Widened so AppKit will *offer* us the paste in the first place.
    ///
    /// `NSTextView` validates the Paste menu item by asking whether the
    /// pasteboard carries any type in this list; with `isRichText = false` it
    /// is `[.string]` alone, so a screenshot — which has no string
    /// representation — disables Paste, and ⌘V beeps without ever reaching
    /// `paste(_:)`. Declaring the image types re-enables the item; `paste(_:)`
    /// below intercepts them before the text system tries to insert anything.
    override var readablePasteboardTypes: [NSPasteboard.PasteboardType] {
        super.readablePasteboardTypes + ComposerTextView.attachableTypes
    }

    /// Belt and braces for the same problem: some validation paths consult the
    /// menu item rather than the readable-types list.
    override func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(paste(_:)),
           NSPasteboard.general.availableType(from: ComposerTextView.attachableTypes) != nil {
            return true
        }
        return super.validateMenuItem(menuItem)
    }

    /// `paste(_:)`, not `readSelection(from:)` — with `isRichText = false` the
    /// text system never offers image types to the reading path, so a pasted
    /// screenshot would be dropped before the composer could see it.
    override func paste(_ sender: Any?) {
        let claimed = onAttach?(NSPasteboard.general) ?? false
        ComposerTextView.logger.notice(
            """
            composer paste: hasHandler=\(self.onAttach != nil, privacy: .public) \
            claimed=\(claimed, privacy: .public) \
            types=\(NSPasteboard.general.types?.map(\.rawValue).joined(separator: ",") ?? "nil", privacy: .public)
            """
        )
        if claimed {
            return
        }
        super.paste(sender)
    }

    fileprivate static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "ChatAttachments"
    )

    override func keyDown(with event: NSEvent) {
        // A choice card sees its keys before Return sends or Escape stops.
        if !hasMarkedText(),
           event.modifierFlags.intersection([.shift, .option, .command, .control]).isEmpty,
           let key = ComposerPickerKey(keyCode: event.keyCode),
           onPickerKey?(key) == true {
            return
        }
        // Escape belongs to the input method while a composition is marked;
        // key repeat must never count as another physical stop-confirmation press.
        if event.keyCode == 53, !event.isARepeat, !hasMarkedText(), onEscape?() == true {
            return
        }
        let isReturn = event.keyCode == 36 || event.keyCode == 76
        guard isReturn else {
            super.keyDown(with: event)
            return
        }

        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if modifiers.contains(.shift) || modifiers.contains(.option) {
            insertNewlineIgnoringFieldEditor(nil)
            return
        }

        if !modifiers.contains(.command), !modifiers.contains(.control), let onSubmit {
            onSubmit()
            return
        }

        // Command-Return steers past a queue. `contains`, like every other test
        // here: an exact match against `.command` alone drops the gesture the
        // moment Caps Lock or the numeric keypad adds a flag of its own.
        if modifiers.contains(.command), let onSteer {
            onSteer()
            return
        }

        super.keyDown(with: event)
    }

    override func didChangeText() {
        super.didChangeText()
        needsDisplay = true
    }

    override func setFrameSize(_ newSize: NSSize) {
        let widthChanged = abs(frame.width - newSize.width) > 0.5
        super.setFrameSize(newSize)
        if widthChanged {
            onWidthChange?()
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        let font = font ?? .systemFont(ofSize: NSFont.systemFontSize)
        let origin = textContainerOrigin
        let textOriginX = origin.x + (textContainer?.lineFragmentPadding ?? 0)
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: NSColor.placeholderTextColor,
        ]
        // A hint follows a one-line draft, such as `/review `.
        if let inlineHint, !inlineHint.isEmpty, !string.isEmpty, !string.contains("\n") {
            let x = textOriginX + ceil((string as NSString).size(withAttributes: [.font: font]).width)
            (inlineHint as NSString).draw(
                at: NSPoint(x: x, y: origin.y),
                withAttributes: attributes
            )
        }
        guard string.isEmpty, !placeholder.isEmpty else { return }

        (placeholder as NSString).draw(
            in: NSRect(
                x: textOriginX,
                y: origin.y,
                width: max(bounds.width - textOriginX * 2, 0),
                height: ceil(font.ascender - font.descender + font.leading)
            ),
            withAttributes: attributes
        )
    }

    /// Prompt and markdown editing must preserve literal technical input.
    /// Configure every public AppKit assistance/transform switch before the
    /// view can become first responder.
    func configureForLiteralMarkdownInput() {
        isContinuousSpellCheckingEnabled = false
        isGrammarCheckingEnabled = false
        isAutomaticSpellingCorrectionEnabled = false
        isAutomaticTextReplacementEnabled = false
        isAutomaticQuoteSubstitutionEnabled = false
        isAutomaticDashSubstitutionEnabled = false
        smartInsertDeleteEnabled = false
        isAutomaticLinkDetectionEnabled = false
        isAutomaticDataDetectionEnabled = false
        isAutomaticTextCompletionEnabled = false
        enabledTextCheckingTypes = 0
        inlinePredictionType = .no
        mathExpressionCompletionType = .no
        writingToolsBehavior = .none
    }
}

/// The keys the composer offers a choice card before handling them itself.
enum ComposerPickerKey: Equatable {
    case up, down, pick, complete, dismiss

    init?(keyCode: UInt16) {
        switch keyCode {
        case 126: self = .up
        case 125: self = .down
        case 36, 76: self = .pick
        case 48: self = .complete
        case 53: self = .dismiss
        default: return nil
        }
    }
}
