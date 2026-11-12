#if DEBUG
import AppKit
import OttamAccessibility
import OttamMarkdownEngine
import SwiftUI

/// DEBUG-only workbench for the native AppKit Markdown renderer
/// (docs/architecture/app/chat/transcript/markdown-renderer.md). The gallery renders
/// each fixture through the production transcript controller so block rendering
/// is judged on real pixels; the hosted-card interaction has its own tab.
struct MarkdownEngineLabView: View {
    private enum LabTab: String, CaseIterable, Identifiable {
        case gallery = "Gallery"
        case cards = "Cards"

        var id: String { rawValue }

        /// Kebab-case spelling for `OTTAM_UI_TEST_MARKDOWN_LAB_TAB`.
        init?(scenarioID: String) {
            switch scenarioID {
            case "gallery": self = .gallery
            case "cards": self = .cards
            default: return nil
            }
        }
    }

    private let typography = TypographySettings.shared
    @State private var tab = LabTab.gallery
    @State private var fixture: MarkdownLabFixture

    init() {
        // A capture names its fixture up front so the gallery is already
        // showing it when the window publishes readiness — switching after
        // the fact would race the screenshot.
        _fixture = State(
            initialValue: UITestPreparation.requestedMarkdownLabFixture
                .flatMap(MarkdownLabFixture.init(scenarioID:))
                ?? .kitchenSink
        )
        _tab = State(
            initialValue: UITestPreparation.requestedMarkdownLabTab
                .flatMap(LabTab.init(scenarioID:))
                ?? .gallery
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 16) {
                Picker("Tab", selection: $tab) {
                    ForEach(LabTab.allCases) { tab in
                        Text(tab.rawValue).tag(tab)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                if tab == .gallery {
                    Picker("Fixture", selection: $fixture) {
                        ForEach(MarkdownLabFixture.allCases) { fixture in
                            Text(fixture.title).tag(fixture)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                }
                Spacer()
            }
            .padding(12)

            Divider()

            switch tab {
            case .gallery: gallery
            case .cards: MarkdownLabCardsView()
            }
        }
        .appTypographyRoot(typography.resolved)
        // A real AX container: an identifier on a bare stack propagates to
        // every descendant and overwrites theirs.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.markdownEngineLab)
        .frame(minWidth: 900, minHeight: 560)
        .background {
            UITestWindowProbe { window in
                UITestPreparation.prepareMarkdownLabIfRequested(window: window)
            }
            .frame(width: 0, height: 0)
        }
    }

    private var gallery: some View {
        pane("Native AppKit renderer") {
            MarkdownLabNativeTranscript(
                plan: [
                    TranscriptItem(
                        id: "markdown-lab/\(fixture.scenarioID)",
                        content: .markdown(
                            fixture.markdown,
                            syntax: fixture.syntax,
                            container: fixture == .userInline ? .bubble : nil
                        )
                    )
                ]
            )
        }
    }

    private func pane(_ title: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.headline)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            Divider()
            content()
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// The lab's only SwiftUI/AppKit boundary. SwiftUI owns fixture and interaction
/// state; the production transcript controller owns rendering and lifecycle.
struct MarkdownLabNativeTranscript: NSViewControllerRepresentable {
    typealias CardFactory = @MainActor (
        _ theme: MarkdownTheme,
        _ environment: HostedCardEnvironment
    ) -> [String: any TranscriptCard]

    @MainActor
    final class Coordinator {
        var theme: MarkdownTheme?
        var cards: [String: any TranscriptCard] = [:]
    }

    let plan: [TranscriptItem]
    var resourceRoot: URL?
    var makeCards: CardFactory

    @Environment(\.appTypography) private var typography

    init(
        plan: [TranscriptItem],
        resourceRoot: URL? = nil,
        makeCards: @escaping CardFactory = { _, _ in [:] }
    ) {
        self.plan = plan
        self.resourceRoot = resourceRoot
        self.makeCards = makeCards
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSViewController(context: Context) -> MacAgentTranscriptViewController {
        let viewController = MacAgentTranscriptViewController()
        update(viewController, coordinator: context.coordinator)
        return viewController
    }

    func updateNSViewController(
        _ viewController: MacAgentTranscriptViewController,
        context: Context
    ) {
        update(viewController, coordinator: context.coordinator)
    }

    static func dismantleNSViewController(
        _ viewController: MacAgentTranscriptViewController,
        coordinator: Coordinator
    ) {
        viewController.tearDown()
        coordinator.cards.removeAll()
    }

    private func update(
        _ viewController: MacAgentTranscriptViewController,
        coordinator: Coordinator
    ) {
        let theme = MarkdownTheme.ottam(typography)
        if coordinator.theme != theme {
            coordinator.cards = makeCards(
                theme,
                HostedCardEnvironment(typography: typography)
            )
            coordinator.theme = theme
        }
        viewController.applyPreparingLargeSettledTranscript(
            plan: plan,
            cards: coordinator.cards,
            conversationID: nil,
            theme: theme,
            resourceRoot: resourceRoot,
            permitsBackgroundPreparation: false,
            completion: CanonicalPresentationCompletion { [weak viewController] succeeded in
                if succeeded { viewController?.scrollToTop() }
            }
        )
    }
}

enum MarkdownLabFixture: String, CaseIterable, Identifiable {
    case kitchenSink
    case codeHeavy
    case tableHeavy
    case listsAndQuotes
    case images
    case userInline

    var id: String { rawValue }

    /// Stable kebab-case spelling shared by UI-test scenario IDs and the
    /// `OTTAM_UI_TEST_MARKDOWN_LAB_FIXTURE` environment variable, so a
    /// capture artifact's name and the fixture it shows cannot drift.
    var scenarioID: String {
        switch self {
        case .kitchenSink: return "kitchen-sink"
        case .codeHeavy: return "code-heavy"
        case .tableHeavy: return "table-heavy"
        case .listsAndQuotes: return "lists-and-quotes"
        case .images: return "images"
        case .userInline: return "user-inline"
        }
    }

    init?(scenarioID: String) {
        guard let match = MarkdownLabFixture.allCases.first(where: { $0.scenarioID == scenarioID })
        else { return nil }
        self = match
    }

    var title: String {
        switch self {
        case .kitchenSink: return "Kitchen sink"
        case .codeHeavy: return "Code"
        case .tableHeavy: return "Tables"
        case .listsAndQuotes: return "Lists + quotes"
        case .images: return "Images"
        case .userInline: return "User (inline)"
        }
    }

    var syntax: TranscriptMarkdownSyntax {
        self == .userInline ? .inline : .full
    }

    /// Past the collapse limit, so the gallery shows the fold and its control.
    private static var foldedFence: String {
        let lines = (1...90).map { "let value\($0) = compute(\($0)) // line \($0)" }
            .joined(separator: "\n")
        return "\n\nA fence past the fold (collapsed by default):\n\n```swift\n\(lines)\n```"
    }

    var markdown: String {
        switch self {
        case .kitchenSink:
            return """
            # Release notes draft

            The engine handles **bold**, *italic*, `inline code`, and [links](https://example.com) \
            inside flowing paragraphs that are long enough to wrap at gallery width, which is \
            exactly where hanging indents and paragraph rhythm show their difference.

            > Blockquotes get a bar instead of just dimmed text.
            >
            > A second quoted paragraph keeps the same bar.
            >
            > > A nested quote adds a second bar, and the inner one closes on
            > > its own paragraph while the outer runs past it.
            >
            > - a list inside a quote keeps both the marker and the bar
            > - and the bar spans the whole list
            >
            > ```swift
            > let quotedFence = "keeps its card and its bar"
            > ```

            ## What changed in `MarkdownStyler`

            - Streaming appends now cost `O(1)` storage edits instead of full reparses
            - Wrapped list items align under their own text, not the margin — this line is \
            deliberately long enough to wrap so the hanging indent is visible at a glance
                - Nested items indent one level deeper
                - And keep their own wrap alignment
                    - A third level gets its own glyph
            - A follow-on paragraph:

                this continuation stays under the item marker

            1. Ordered items keep ordinals
            2. Even after an unordered block
            3. `code` opening an item keeps its pill padding
            4. Four
            5. Five
            6. Six
            7. Seven
            8. Eight
            9. Nine
            10. Ten stays aligned with nine

            - [x] Task lists render a checkbox
            - [ ] and an open one, with **bold** intact
            - [ ] a third that is long enough to wrap onto a second line so the hanging indent \
            under a checkbox can be checked too

            ```swift
            func styler(_ nodes: [MarkdownBlockNode]) -> NSAttributedString {
                // Long code lines scroll horizontally inside the card; transcript copy re-emits the whole fence.
                return MarkdownStyler(theme: .ottam(typography)).attributedString(from: nodes)
            }
            ```

            ---

            An inline image: ![diagram](https://example.com/pipeline.png)

            | block | rendering |
            | ----- | --------- |
            | code | native horizontally scrolling block |
            | table | native grid on measured tab stops, horizontal overflow |
            | image | Core Text attachment, sized on load |
            """
        case .tableHeavy:
            return """
            Aligned columns, inline markup in cells, and overflow.

            | Flag | Default | Effect |
            |:-----|:-------:|-------:|
            | `--verbose` | off | prints every **step** |
            | `--jobs` | 4 | parallel *workers* |
            | `--out` | `./dist` | see [docs](https://example.com/out) |

            A wide table that must scroll rather than wrap:

            | Metric | Baseline p50 | Baseline p95 | Feature p50 | Feature p95 | Delta p50 | Delta p95 | Notes |
            |--------|-------------:|-------------:|------------:|------------:|----------:|----------:|-------|
            | preparation | 1.92 ms | 4.10 ms | 1.88 ms | 4.02 ms | -0.04 ms | -0.08 ms | three matched runs on the shared corpus |
            | layout | 0.71 ms | 1.60 ms | 0.70 ms | 1.58 ms | -0.01 ms | -0.02 ms | no visible change |
            | commit | 3.30 ms | 7.90 ms | 3.28 ms | 7.85 ms | -0.02 ms | -0.05 ms | within noise |

            A header-only table and a ragged one:

            | pending | cells |
            |---------|-------|

            | a | b | c |
            |---|---|---|
            | 1 |
            | 1 | 2 | 3 | 4 |

            > | quoted | table |
            > |--------|-------|
            > | keeps | its bar |
            """
        case .codeHeavy:
            return """
            Comparing highlight parity across languages.

            ```swift
            let answer = compute(42) // trailing comment
            struct Foo { var bar: String = "baz" }
            ```

            ```go
            func main() {
                fmt.Println("hello", 3.14) // go comment
            }
            ```

            ```sh
            if [ -f config.yml ]; then echo "found" ; fi # shell comment
            ```

            ```json
            {"enabled": true, "count": 3, "name": "engine"}
            ```

            Plain fence:

            ```
            no highlighting, but the card still applies to this very long unbroken line that scrolls horizontally instead of wrapping
            ```

            A tall card next to a one-liner (height determinism + copy button):

            ```swift
            struct Pipeline {
                let stages: [Stage]
                func run(_ input: Input) -> Output {
                    var current = input.normalized()
                    for stage in stages {
                        current = stage.transform(current)
                        guard current.isValid else { break }
                    }
                    return current.finalized()
                }
            }
            ```

            ```sh
            echo done
            ```
            """ + Self.foldedFence
        case .listsAndQuotes:
            return """
            ### Deep nesting

            - depth one with enough words that the line wraps and shows its hanging indent clearly
                - depth two, same story with the wrap alignment staying under the item text
                    - depth three for good measure
            - back to depth one

            1. first
            2. second
                1. nested ordinal
                2. another
            3. third

            - [x] a finished task dims
            - [ ] an open task keeps full color

            > A quote with `inline code` and **bold** inside it.

            > A separate quote block after a gap.
            """
        case .images:
            return """
            ### Inline images

            A wide image aspect-fits the column:

            ![wide gradient](\(MarkdownLabImages.wide.absoluteString))

            A tall image caps at 320pt and keeps its ratio:

            ![tall gradient](\(MarkdownLabImages.tall.absoluteString))

            A tiny image renders at native size, never upscaled: \
            ![tiny](\(MarkdownLabImages.tiny.absoluteString))

            And a URL that will never load keeps its placeholder glyph:

            ![missing](https://invalid.invalid/missing.png)

            Prose after the images keeps flowing normally.
            """
        case .userInline:
            return """
            user-typed text keeps its own line breaks
            second line stays a second line
            `code span` and **bold** still style
            """
        }
    }
}

/// Runtime-generated PNGs on disk so the images fixture loads without network.
/// Used by the Gallery's images fixture.
enum MarkdownLabImages {
    static let wide = generate(size: CGSize(width: 1200, height: 400), hue: 0.58)
    static let tall = generate(size: CGSize(width: 500, height: 1200), hue: 0.08)
    static let tiny = generate(size: CGSize(width: 48, height: 48), hue: 0.33)

    private static func generate(size: CGSize, hue: CGFloat) -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(
            "ottam-markdown-lab-\(Int(size.width))x\(Int(size.height)).png"
        )
        guard !FileManager.default.fileExists(atPath: url.path) else { return url }
        let image = NSImage(size: size, flipped: false) { rect in
            let gradient = NSGradient(
                starting: NSColor(hue: hue, saturation: 0.7, brightness: 0.9, alpha: 1),
                ending: NSColor(hue: hue, saturation: 0.9, brightness: 0.4, alpha: 1)
            )
            gradient?.draw(in: rect, angle: 45)
            return true
        }
        if let tiff = image.tiffRepresentation,
           let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) {
            try? png.write(to: url)
        }
        return url
    }
}
#endif
