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

    static func install() {
        guard source == nil else { return }
        // The dispatch source only fires once the default disposition is off.
        signal(SIGTERM, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        source.setEventHandler {
            // Not from inside this main-queue callout: a deferred quit
            // (`.terminateLater`) waits on main-actor work, which can't run
            // until the callout returns, so the quit would deadlock. Common
            // modes, so a quit also lands while a modal panel or a menu runs
            // the loop.
            RunLoop.main.perform(inModes: [.common]) {
                MainActor.assumeIsolated {
                    NSApplication.shared.terminate(nil)
                }
            }
        }
        source.resume()
        self.source = source
    }
}
