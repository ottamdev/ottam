#if DEBUG
import AppKit
import SwiftUI

struct UITestWindowProbe: NSViewRepresentable {
    let onWindowAvailable: (NSWindow) -> Void

    func makeNSView(context: Context) -> ProbeView {
        ProbeView(onWindowAvailable: onWindowAvailable)
    }

    func updateNSView(_ nsView: ProbeView, context: Context) {
        nsView.onWindowAvailable = onWindowAvailable
        nsView.reportWindowIfAvailable()
    }

    final class ProbeView: NSView {
        var onWindowAvailable: (NSWindow) -> Void

        init(onWindowAvailable: @escaping (NSWindow) -> Void) {
            self.onWindowAvailable = onWindowAvailable
            super.init(frame: .zero)
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) {
            nil
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            reportWindowIfAvailable()
        }

        func reportWindowIfAvailable() {
            guard let window else {
                return
            }
            DispatchQueue.main.async { [weak self, weak window] in
                guard let self, let window else {
                    return
                }
                self.onWindowAvailable(window)
            }
        }
    }
}
#endif
