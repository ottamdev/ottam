import CoreGraphics
import Testing

@testable import OttamPresentationLogic

@Suite struct QuoteReplyTests {
    @Test func quotesASingleLineAboveTheComment() {
        #expect(QuoteReply.messageBody(quote: "close the M6 step", comment: "why first?")
            == "> close the M6 step\n\nwhy first?")
    }

    @Test func quotesEveryLineAndKeepsParagraphBreaksInsideTheQuote() {
        #expect(QuoteReply.messageBody(quote: "one\ntwo\n\nthree", comment: "ok")
            == "> one\n> two\n>\n> three\n\nok")
    }

    @Test func nestsAnExistingQuote() {
        #expect(QuoteReply.messageBody(quote: "> earlier", comment: "ok") == "> > earlier\n\nok")
    }

    @Test func trimsBothParts() {
        #expect(QuoteReply.messageBody(quote: "  hi \n", comment: "\n yes ") == "> hi\n\nyes")
    }

    @Test func needsAComment() {
        #expect(QuoteReply.messageBody(quote: "hi", comment: "  \n") == nil)
    }

    @Test func anEmptyQuoteSendsJustTheComment() {
        #expect(QuoteReply.messageBody(quote: " \n", comment: "yes") == "yes")
    }

    @Test func capsAndMarksALongQuote() {
        let long = String(repeating: "a", count: QuoteReply.maximumQuoteLength + 50)
        let body = QuoteReply.messageBody(quote: long, comment: "ok")
        #expect(body == "> \(String(repeating: "a", count: QuoteReply.maximumQuoteLength))…\n\nok")
    }

    @Test func stagedCommentsComeFirstThenTheComposerText() {
        let comments = [
            QuoteReply.Comment(quote: "one", comment: "why?"),
            QuoteReply.Comment(quote: "two\nlines", comment: "ok"),
        ]
        #expect(QuoteReply.messageBody(comments: comments, text: " thanks \n")
            == "> one\n\nwhy?\n\n> two\n> lines\n\nok\n\nthanks")
        #expect(QuoteReply.messageBody(comments: comments, text: "") == "> one\n\nwhy?\n\n> two\n> lines\n\nok")
        #expect(QuoteReply.messageBody(comments: [], text: "hi") == "hi")
        #expect(QuoteReply.messageBody(comments: [], text: " ") == nil)
    }

    @Test func headerJoinsLinesIntoOne() {
        #expect(QuoteReply.headerText(for: "one\n\n  two  \nthree") == "one two three")
    }

    // A 600×800 viewport under a 50pt titlebar and above a 100pt composer.
    private let band = CGRect(x: 0, y: 50, width: 600, height: 650)
    private let pill = CGSize(width: 120, height: 36)

    @Test func sitsBelowAVisibleSelection() {
        let placement = QuoteReply.placement(anchor: CGRect(x: 40, y: 200, width: 300, height: 18), band: band, size: pill)
        #expect(placement == .init(origin: CGPoint(x: 40, y: 224), pin: .none, side: .below))
    }

    @Test func startsAboveWhenTheBottomIsTooClose() {
        let card = CGSize(width: 360, height: 120)
        let placement = QuoteReply.placement(anchor: CGRect(x: 40, y: 600, width: 300, height: 18), band: band, size: card)
        #expect(placement == .init(origin: CGPoint(x: 40, y: 474), pin: .none, side: .above))
    }

    @Test func aKeptSideClampsInsteadOfFlipping() {
        let card = CGSize(width: 360, height: 120)
        let nearBottom = QuoteReply.placement(
            anchor: CGRect(x: 40, y: 600, width: 300, height: 18), band: band, size: card, side: .below
        )
        #expect(nearBottom == .init(origin: CGPoint(x: 40, y: 568), pin: .none, side: .below))
        let nearTop = QuoteReply.placement(
            anchor: CGRect(x: 40, y: 60, width: 300, height: 18), band: band, size: pill, side: .above
        )
        #expect(nearTop == .init(origin: CGPoint(x: 40, y: 62), pin: .none, side: .above))
    }

    @Test func pinsToTheTopOnceTheSelectionScrollsAboveTheBand() {
        let placement = QuoteReply.placement(anchor: CGRect(x: 40, y: -80, width: 300, height: 18), band: band, size: pill)
        #expect(placement == .init(origin: CGPoint(x: 40, y: 62), pin: .top, side: .below))
    }

    @Test func pinsToTheBottomOnceTheSelectionScrollsBelowTheBand() {
        let placement = QuoteReply.placement(anchor: CGRect(x: 40, y: 760, width: 300, height: 18), band: band, size: pill)
        #expect(placement == .init(origin: CGPoint(x: 40, y: 652), pin: .bottom, side: .above))
    }

    @Test func staysInsideTheBandHorizontally() {
        let card = CGSize(width: 360, height: 120)
        let placement = QuoteReply.placement(anchor: CGRect(x: 500, y: 200, width: 80, height: 18), band: band, size: card)
        #expect(placement.origin.x == 228)
        let narrow = QuoteReply.placement(
            anchor: CGRect(x: 500, y: 200, width: 80, height: 18),
            band: CGRect(x: 0, y: 50, width: 300, height: 650),
            size: card
        )
        #expect(narrow.origin.x == 12)
    }
}
