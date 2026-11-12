import SwiftUI

/// The Ottam thread mark, drawn natively so the thread can move. Geometry and
/// colours match `assets/brand/mark.svg`.
///
/// The view is square and scales with its frame. It animates through SwiftUI,
/// so inside the hosted transcript prefer a Core Animation mask (see
/// `ChatShimmerText`).
struct OttamThreadMark: View {
    enum Tint {
        /// Amber to red-orange, left to right. The gradient is pinned to the
        /// frame and the thread travels through it. Reads on light and dark.
        case brand
        /// The current foreground style: follows light and dark mode, or
        /// whatever `foregroundStyle` the caller sets (white on a brand tile).
        case foreground
    }

    enum Motion {
        case still
        /// Threads in once when the view appears, then rests.
        case drawIn
        /// Threads in, rests for `hold` seconds, pulls out the far side, and
        /// repeats. A short hold reads as progress; a long one as an idle
        /// flourish.
        case flow(hold: TimeInterval = 0.6)
    }

    var tint: Tint = .brand
    var motion: Motion = .drawIn

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drawn = false
    /// Bumped once for each pass of `.flow`.
    @State private var pass = 0

    private static let travelDuration: TimeInterval = 1.1
    /// Nothing is visible between pulling out and threading back in.
    private static let gapDuration: TimeInterval = 0.4
    private static let travel = Animation.timingCurve(0.65, 0, 0.35, 1, duration: travelDuration)

    var body: some View {
        Group {
            switch reduceMotion ? .still : motion {
            case .still:
                ThreadStroke(span: .whole, tint: tint)
            case .drawIn:
                ThreadStroke(span: drawn ? .whole : .before, tint: tint)
                    .onAppear(perform: drawIn)
            case .flow(let hold):
                // One pass per trigger, so nothing animates while the thread
                // rests. A delayed animation as the hold keeps SwiftUI
                // redrawing the thread every frame of it.
                PhaseAnimator([ThreadSpan.whole, .after, .before], trigger: pass) { span in
                    ThreadStroke(span: drawn ? span : .before, tint: tint)
                } animation: { span in
                    switch span {
                    case .whole, .after: Self.travel
                    case .before: .linear(duration: Self.gapDuration)
                    }
                }
                .onAppear(perform: drawIn)
                .task(id: hold) {
                    let passDuration = 2 * Self.travelDuration + Self.gapDuration
                    while (try? await Task.sleep(for: .seconds(hold + passDuration))) != nil {
                        pass += 1
                    }
                }
            }
        }
        .aspectRatio(1, contentMode: .fit)
        .accessibilityHidden(true)
    }

    private func drawIn() {
        withAnimation(Self.travel.delay(0.2)) { drawn = true }
    }
}

/// The stretch of the thread that is visible, as fractions of its length.
private enum ThreadSpan {
    case before
    case whole
    case after

    var from: CGFloat { self == .after ? 1 : 0 }
    var to: CGFloat { self == .before ? 0 : 1 }
}

private struct ThreadStroke: View {
    let span: ThreadSpan
    let tint: OttamThreadMark.Tint

    var body: some View {
        ThreadShape(from: span.from, to: span.to).fill(style)
    }

    private var style: AnyShapeStyle {
        switch tint {
        case .brand:
            AnyShapeStyle(
                LinearGradient(
                    colors: [
                        Color(red: 1, green: 149 / 255, blue: 0),
                        Color(red: 1, green: 77 / 255, blue: 15 / 255),
                    ],
                    startPoint: UnitPoint(x: 0.16, y: 0.5),
                    endPoint: UnitPoint(x: 0.84, y: 0.5)
                )
            )
        case .foreground:
            AnyShapeStyle(.foreground)
        }
    }
}

/// The stroked outline of the thread, so a fill pinned to the frame shows
/// through whichever stretch is visible.
private struct ThreadShape: Shape {
    var from: CGFloat
    var to: CGFloat

    var animatableData: AnimatablePair<CGFloat, CGFloat> {
        get { AnimatablePair(from, to) }
        set {
            from = newValue.first
            to = newValue.second
        }
    }

    func path(in rect: CGRect) -> Path {
        // mark.svg's 100-unit box, centred in the frame.
        let unit = min(rect.width, rect.height) / 100
        func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: rect.midX + (x - 50) * unit, y: rect.midY + (y - 50) * unit)
        }

        var thread = Path()
        thread.move(to: point(16, 74))
        thread.addLine(to: point(50, 74))
        // Up the right side, over the top, down the left. `clockwise` is
        // mirrored in SwiftUI's flipped coordinate space.
        thread.addArc(
            center: point(50, 50), radius: 24 * unit,
            startAngle: .degrees(90), endAngle: .degrees(-90), clockwise: true)
        thread.addArc(
            center: point(50, 50), radius: 24 * unit,
            startAngle: .degrees(-90), endAngle: .degrees(-270), clockwise: true)
        thread.addLine(to: point(84, 74))

        // Normalizing merges the stroker's per-segment pieces into one
        // outline; without it their seams show as notches at the joins.
        return thread
            .trimmedPath(from: from, to: to)
            .strokedPath(StrokeStyle(lineWidth: 8 * unit, lineCap: .round, lineJoin: .round))
            .normalized(eoFill: false)
    }
}
