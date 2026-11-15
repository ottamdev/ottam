import AppKit
import Darwin
import Dispatch

/// Turns SIGTERM into a real AppKit quit.
///
/// `scripts/run-app.sh` stops the instance it owns by signalling the exact PID.
/// The default disposition kills the process outright, which tears the Core
/// child down mid-flight and bypasses AppKit's normal termination and native
/// window-restoration lifecycle. Handling the signal ourselves lets AppKit run
/// that quit sequence.
///
/// The launcher waits for this quit sequence; shared credential refresh must
/// never be bypassed by a timed SIGKILL fallback.
@MainActor
enum TerminationSignalHandler {
    private static var source: DispatchSourceSignal?
    /// A signalled quit has nobody to answer a question, so it never asks.
    /// It lasts until that quit is cancelled, so later quits ask again.
    private(set) static var quitRequested = false

    /// Set while quit asks about working agents. AppKit ignores `terminate`
    /// while a quit is pending, so a signal answers that question instead.
    static var answerQuitQuestion: (@MainActor () -> Void)?

    static func quitCancelled() {
        quitRequested = false
    }

    static func install() {
        guard source == nil else { return }
        // The dispatch source only fires once the default disposition is off.
        signal(SIGTERM, SIG_IGN)
        let source = makeSource()
        source.resume()
        self.source = source
    }

    /// Off the main queue: quit's question runs a modal loop inside a
    /// main-actor job, and the main queue drains nothing until it returns.
    /// Nonisolated, so the handler doesn't inherit the main actor.
    private nonisolated static func makeSource() -> DispatchSourceSignal {
        let source = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global())
        source.setEventHandler {
            // On the main run loop, not the main queue: a deferred quit
            // (`.terminateLater`) waits on main-actor work, so it must not
            // start inside a main-queue callout. Common modes, so a quit also
            // lands while a modal panel or a menu runs the loop.
            let main = CFRunLoopGetMain()
            CFRunLoopPerformBlock(main, CFRunLoopMode.commonModes.rawValue) {
                MainActor.assumeIsolated {
                    quitRequested = true
                    if let answerQuitQuestion {
                        answerQuitQuestion()
                    } else {
                        NSApplication.shared.terminate(nil)
                    }
                }
            }
            CFRunLoopWakeUp(main)
        }
        return source
    }
}
