import OttamAccessibility
import OttamBackendClient
import OttamMarkdownEngine
import OttamPresentationLogic
import SwiftUI

/// The row beneath a user message: the same actions the assistant turn's
/// trailer carries, mirrored onto the user's side of the transcript.
///
/// `showsCopied` is UI-only and does not participate in geometry, matching
/// `TurnTrailerCardPayload`.
struct UserTrailerCardPayload: HostedCardPayload {
    struct Actions {
        /// The host's clipboard write; `onCopy` is bound on top of it.
        var copy: (String) -> Void = { _ in }
        var onCopy: () -> Void = {}
        var onResend: () -> Void = {}
    }

    var presentation: ChatUserTrailerPresentation
    var showsCopied = false

    /// The delivery state is derived rather than stored, so the row and the
    /// card below it can never disagree about what an input means.
    var state: ChatDeliveryStatusPresentation? {
        guard let input = presentation.input,
              case .status(let status) = ChatDeliveryPresentation(input: input)
        else { return nil }
        return status
    }

    /// One line in every state, so every state shares one cached measurement.
    /// Only the label can change the row's width enough to matter, and width
    /// is not what is cached. The copy button is the tallest thing the row
    /// holds, so a message with nothing to copy measures on its own rather
    /// than borrowing a height it does not fill.
    var signature: AnyHashable {
        presentation.body.isEmpty ? "user-trailer-bare" : "user-trailer"
    }

    func makeBody(actions: Actions) -> some View {
        ChatUserTrailerRow(
            messageBody: presentation.body,
            createdAt: presentation.createdAt,
            state: state,
            failureReason: presentation.failureReason,
            showsResend: presentation.showsResend,
            isMutating: presentation.isMutating,
            isLeading: presentation.isLeading,
            showsCopied: showsCopied,
            onCopy: actions.onCopy,
            onResend: actions.onResend
        )
        .frame(maxWidth: .infinity, alignment: presentation.isLeading ? .leading : .trailing)
    }

    mutating func adopt(_ fresh: Self) {
        let showsCopied = self.showsCopied
        self = fresh
        self.showsCopied = showsCopied
    }

    var accessibilityPlaceholder: TranscriptCardAccessibilityPlaceholder? {
        guard let state, state.isUserMarkedDelivered else { return nil }
        return TranscriptCardAccessibilityPlaceholder(
            identifier: AccessibilityID.chatMarkedDelivered,
            value: state.label
        )
    }

    @MainActor static func bindActions(_ actions: inout Actions, to card: HostedCard<Self>) {
        let copy = actions.copy
        actions.onCopy = { [weak card] in
            guard let card, !card.payload.presentation.body.isEmpty else { return }
            copy(card.payload.presentation.body)
            card.updatePayload { $0.showsCopied = true }
            Task { [weak card] in
                try? await Task.sleep(for: .seconds(1.5))
                card?.updatePayload { $0.showsCopied = false }
            }
        }
    }
}

typealias HostedUserTrailerCard = HostedCard<UserTrailerCardPayload>

extension HostedCard where Payload == UserTrailerCardPayload {
    static func userTrailer(
        _ presentation: ChatUserTrailerPresentation,
        theme: MarkdownTheme,
        environment: HostedCardEnvironment,
        copy: @escaping (String) -> Void,
        resend: @escaping () -> Void
    ) -> HostedUserTrailerCard {
        HostedUserTrailerCard(
            payload: UserTrailerCardPayload(presentation: presentation),
            actions: .init(copy: copy, onResend: resend),
            theme: theme,
            environment: environment
        )
    }
}

private struct ChatUserTrailerRow: View {
    let messageBody: String
    let createdAt: String
    let state: ChatDeliveryStatusPresentation?
    let failureReason: String?
    let showsResend: Bool
    let isMutating: Bool
    let isLeading: Bool
    let showsCopied: Bool
    let onCopy: () -> Void
    let onResend: () -> Void

    var body: some View {
        HStack(spacing: 0) {
            // The same zero-width strut the assistant trailer uses: one callout
            // line height in every state, without a hard-coded pixel height.
            // `fixedSize` before the frame, or the text wraps to two lines at
            // zero width and the row measures twice the height it needs.
            Text("Ag")
                .appFont(.callout)
                .hidden()
                .fixedSize()
                .frame(width: 0)
                .accessibilityHidden(true)

            if !isLeading {
                Spacer(minLength: 0)
            }

            // Copy sits at the row's outer edge on either side, so a leading
            // row reads like the assistant trailer: copy, time, then state.
            HStack(spacing: 10) {
                if isLeading {
                    copyButton
                    timestamp
                    stateLabel
                } else {
                    stateLabel
                    timestamp
                    copyButton
                }
            }

            if isLeading {
                Spacer(minLength: 0)
            }
        }
        // No top padding, unlike the assistant trailer. That row follows plain
        // text and needs the gap; this one follows a bubble or a thumbnail,
        // both of which end in a hard edge the eye already reads as a break.
        // Leading, it starts at the card's edge, as the card's own header does.
        .padding(.trailing, isLeading ? 0 : 4)
    }

    @ViewBuilder
    private var stateLabel: some View {
        if let state {
            ChatRunInputStateLabel(
                status: state,
                resend: showsResend ? onResend : nil,
                isMutating: isMutating
            )
            .help(failureReason ?? "")
        }
    }

    @ViewBuilder
    private var timestamp: some View {
        if let timestamp = chatMessageTimestamp(createdAt) {
            Text(timestamp)
                .appFont(.callout)
                .foregroundStyle(.tertiary)
                .lineLimit(1)
        }
    }

    /// An attachment-only message has no text to put on the clipboard, so
    /// the affordance stays out of the way.
    @ViewBuilder
    private var copyButton: some View {
        if !messageBody.isEmpty {
            Button(action: onCopy) {
                Image(systemName: showsCopied ? "checkmark" : "square.on.square")
                    .appFont(.callout)
                    .contentTransition(.symbolEffect(.replace))
                    .frame(width: 16, height: 16)
                    .animation(.easeInOut(duration: 0.2), value: showsCopied)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .pointerStyle(.link)
            .help("Copy message")
            .accessibilityLabel(showsCopied ? "Copied" : "Copy message")
        }
    }
}
