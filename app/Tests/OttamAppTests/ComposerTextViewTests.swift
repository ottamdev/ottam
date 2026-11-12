import AppKit
import OttamAccessibility
import Testing
@testable import OttamApp

@Suite("Composer keyboard input")
@MainActor
struct ComposerTextViewTests {
    private func escape(repeating: Bool = false) throws -> NSEvent {
        try #require(NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: 0,
            context: nil,
            characters: "\u{1b}",
            charactersIgnoringModifiers: "\u{1b}",
            isARepeat: repeating,
            keyCode: 53
        ))
    }

    @Test("holding Escape cannot confirm stopping a run")
    func escapeRepeatDoesNotConfirmStop() throws {
        let editor = ComposerTextView(frame: NSRect(x: 0, y: 0, width: 400, height: 100))
        var confirmation = ChatEscapeStopConfirmation()
        var stops = 0
        let now = ContinuousClock().now
        editor.onEscape = {
            if confirmation.press(at: now) { stops += 1 }
            return true
        }

        editor.keyDown(with: try escape())
        #expect(confirmation.isArmed)
        for _ in 0..<3 {
            editor.keyDown(with: try escape(repeating: true))
        }
        #expect(stops == 0)
        #expect(confirmation.isArmed)

        editor.keyDown(with: try escape())
        #expect(stops == 1)
        #expect(!confirmation.isArmed)
    }

    @Test("Escape during marked text does not arm or confirm run cancellation")
    func escapeBelongsToMarkedText() throws {
        let editor = ComposerTextView(frame: NSRect(x: 0, y: 0, width: 400, height: 100))
        var escapeCalls = 0
        editor.onEscape = {
            escapeCalls += 1
            return true
        }

        for _ in 0..<2 {
            editor.setMarkedText(
                "にほん",
                selectedRange: NSRange(location: 3, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
            #expect(editor.hasMarkedText())
            editor.keyDown(with: try escape())
            #expect(escapeCalls == 0)
        }

        editor.unmarkText()
        editor.keyDown(with: try escape())
        #expect(escapeCalls == 1)
    }
}

@Suite("Composer primary action")
@MainActor
struct ChatComposerActionTests {
    private func action(liveRun: Bool, canSend: Bool, submitting: Bool) -> ChatComposerAction {
        ChatComposerAction(hasLiveRun: liveRun, canSend: canSend, isSubmitting: submitting)
    }

    @Test("a steer on the wire keeps the submit identity instead of becoming Stop")
    func steerInFlightIsNotStop() {
        // `canSend` requires `model.canSubmit`, which is false for the length
        // of a send. Reading that as "nothing to steer with" would put Stop
        // under the cursor mid-steer, and Return or Escape would then kill the
        // run the reader is steering.
        let sending = action(liveRun: true, canSend: false, submitting: true)
        #expect(sending == .sending)
        #expect(sending != .stop)
        #expect(sending.accessibilityIdentifier == AccessibilityID.chatSubmit)
        #expect(sending.glyph == nil, "a spinner stands in, so the arrow does not flicker away")
    }

    @Test("the button means Stop only with a live run and nothing to send")
    func stopNeedsALiveRunAndAnEmptyDraft() {
        #expect(action(liveRun: true, canSend: false, submitting: false) == .stop)
        #expect(action(liveRun: true, canSend: true, submitting: false) == .queue)
        #expect(action(liveRun: false, canSend: false, submitting: false) == .submit)
        #expect(action(liveRun: false, canSend: true, submitting: false) == .submit)
    }

    @Test("only Stop reads as a different control")
    func onlyStopChangesIdentifier() {
        for action in [ChatComposerAction.submit, .queue, .sending] {
            #expect(action.accessibilityIdentifier == AccessibilityID.chatSubmit)
        }
        #expect(ChatComposerAction.stop.accessibilityIdentifier == AccessibilityID.chatStop)
    }

    @Test("a submission with no run still shows its spinner")
    func submittingWithoutARunSpins() {
        #expect(action(liveRun: false, canSend: false, submitting: true) == .sending)
    }

    @Test("the value separates states the identifier deliberately merges")
    func valueSeparatesWhatTheIdentifierMerges() {
        let values = [ChatComposerAction.submit, .queue, .sending, .stop]
            .map(\.accessibilityValue)
        #expect(Set(values).count == values.count)
        // Only `ready` means the button is neither sending nor holding a run,
        // which is what tells a reader — and a scenario — that the last message
        // is fully out and the next one can go.
        #expect(ChatComposerAction.submit.accessibilityValue == "ready")
        #expect(ChatComposerAction.sending.accessibilityValue != "ready")
        #expect(ChatComposerAction.queue.accessibilityValue != "ready")
    }
}
