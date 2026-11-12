import AppKit
import OttamMarkdownEngine
import SwiftUI
import Testing
@testable import OttamApp

struct ProbePayload: HostedCardPayload {
    struct Actions { var onToggle: () -> Void }
    var text: String
    var expanded: Bool

    var signature: AnyHashable { "\(text)|\(expanded)" }

    func makeBody(actions: Actions) -> some View {
        VStack(alignment: .leading) {
            Text(text).fixedSize(horizontal: false, vertical: true)
            if expanded { Text("details").padding(.top, 40) }
            Button("Toggle") { actions.onToggle() }
        }
        .padding(12)
    }
}

@Suite("Hosted cards")
@MainActor
struct HostedCardTests {
    private let theme = MarkdownTheme.ottam(.builtIn)
    private let environment = HostedCardEnvironment(typography: .builtIn)
    private let long = Array(repeating: "words that wrap across the column", count: 6)
        .joined(separator: " ")

    private func make(_ text: String, expanded: Bool = false) -> HostedCard<ProbePayload> {
        HostedCard(
            payload: ProbePayload(text: text, expanded: expanded),
            actions: .init(onToggle: {}),
            theme: theme,
            environment: environment
        )
    }

    @Test("height is a pure function of payload, width and theme")
    func heightPurity() {
        let a = make(long), b = make(long)
        #expect(a.measuredHeight(forWidth: 400) == b.measuredHeight(forWidth: 400))
        #expect(a.measuredHeight(forWidth: 400) > a.measuredHeight(forWidth: 1_200))
        #expect(
            make(long, expanded: true).measuredHeight(forWidth: 400)
                > a.measuredHeight(forWidth: 400)
        )
    }

    @Test("payload changes refresh the hosted view and invalidate native geometry")
    func payloadChange() {
        let card = make(long)
        let view = card.makeHostedView()
        var invalidations = 0
        card.onPayloadChanged = { _ in invalidations += 1 }
        let collapsed = card.measuredHeight(forWidth: 400)

        card.updatePayload { $0.expanded = true }

        #expect(card.hostedView === view)
        #expect(card.measuredHeight(forWidth: 400) > collapsed)
        #expect(invalidations == 1)
    }
}
