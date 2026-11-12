import AppKit
import OttamBackendClient
import OttamMarkdownEngine
import OttamPresentationLogic

/// Builds the native host for a card spec. Returns nil for a spec with no
/// conformer yet; the store drops that entry and reports its ID.
@MainActor
protocol ChatTranscriptCardFactory {
    func makeCard(_ spec: ChatTranscriptCardSpec, id: String) -> (any TranscriptCard)?
}

/// Reconciles a transcript plan into the hosted-card instances used by the
/// native collection renderer.
///
/// The plan is a value, while cards are stateful AppKit/SwiftUI hosts. The
/// store therefore keeps one host per entry ID across reconciliations:
/// an unchanged spec reuses it untouched, a changed spec is adopted in place,
/// and IDs that left the plan are evicted.
@MainActor
final class ChatTranscriptCardStore {
    struct Reconciliation {
        var cards: [String: any TranscriptCard] = [:]
        /// Card entries the factory declined (no conformer yet); dropped.
        var unmaterializedIDs: [String] = []
    }

    private struct Slot {
        var spec: ChatTranscriptCardSpec
        var card: any TranscriptCard
    }

    /// Builds cards from here on. A conversation switch that keeps the view
    /// swaps it, so new cards act on the conversation shown.
    var factory: any ChatTranscriptCardFactory
    private var slots: [String: Slot] = [:]

    init(factory: any ChatTranscriptCardFactory) {
        self.factory = factory
    }

    /// `previous` is another store's cards (a re-themed predecessor): a new
    /// card built here takes over the old card's payload under the same ID,
    /// so UI-only state such as an open disclosure survives the theme change.
    func reconcile(
        _ entries: [TranscriptItem],
        inheritingStateFrom previous: ChatTranscriptCardStore? = nil
    ) -> Reconciliation {
        var result = Reconciliation()
        // Pagination can prepend adjacent activities and change the group's first
        // ID. Preserve disclosure state through each normalized member identity.
        let priorSlots = previous?.slots ?? slots
        var activityOwners: [String: any TranscriptCard] = [:]
        for old in priorSlots.values {
            if case .activityGroup(let activities, _) = old.spec {
                for activity in activities { activityOwners[activity.id] = old.card }
            }
        }
        var kept: [String: Slot] = [:]
        for entry in entries {
            switch entry.content {
            case .markdown:
                continue
            case .card(let spec):
                guard let slot = slot(for: entry.id, spec: spec) else {
                    result.unmaterializedIDs.append(entry.id)
                    continue
                }
                if slots[entry.id] == nil {
                    var old = priorSlots[entry.id]?.card
                    if old == nil, case .activityGroup(let activities, _) = spec {
                        old = activities.lazy.compactMap { activityOwners[$0.id] }.first
                    }
                    if let old { (slot.card as? any HostedCardStateInheriting)?.inheritState(from: old) }
                }
                kept[entry.id] = slot
                result.cards[entry.id] = slot.card
            }
        }
        slots = kept
        return result
    }

    private func slot(for id: String, spec: ChatTranscriptCardSpec) -> Slot? {
        if let existing = slots[id] {
            if existing.spec == spec { return existing }
            guard let fresh = factory.makeCard(spec, id: id) else { return nil }
            // `false` also means the payload adopted without a visible change
            // (a reasoning revision); that card is kept, not replaced.
            if existing.card.adoptPayload(from: fresh) || type(of: existing.card) == type(of: fresh) {
                return Slot(spec: spec, card: existing.card)
            }
            // A different concrete type under the same ID cannot adopt the
            // payload, so the native collection item receives a new host.
            return Slot(spec: spec, card: fresh)
        }
        guard let card = factory.makeCard(spec, id: id) else { return nil }
        return Slot(spec: spec, card: card)
    }
}

/// Closures back into the model, handed to the factory as one value. Not in
/// any payload: identity-free, so they never make two specs unequal.
struct ChatTranscriptActions {
    var resolveDelivery: (ConversationRunInput, _ retry: Bool) -> Void = { _, _ in }
    var retryFailedRun: (ConversationRunID) -> Void = { _ in }
    var checkRuntime: () -> Void = {}
    var installRuntime: () -> Void = {}
    var signIn: (ChatAuthRecovery) -> Void = { _ in }
    var cancelSignIn: (ChatAuthRecovery) -> Void = { _ in }
    var signInMcp: (McpServerID) -> Void = { _ in }
    var cancelMcpSignIn: (McpServerID) -> Void = { _ in }
    var resolvePermission: (ChatPendingPermission, String?) -> Void = { _, _ in }
    /// The run that started the task, and the adapter's task id.
    var stopBackgroundTask: (ConversationRunID, String) -> Void = { _, _ in }
    /// Shows a subagent's thread in the Subagent tab.
    var openSubagentThread: (String) -> Void = { _ in }
    /// Shows a decided plan in the Plan tab.
    var openPlan: (ChatPlan) -> Void = { _ in }
    /// Opens a delegated child, or a child's parent.
    var openConversation: (ConversationID) -> Void = { _ in }
    /// Delivers the conversation's waiting notes now.
    var continueDelegation: () -> Void = {}
    /// Wakes a delegated child's parent with the child's waiting report.
    var sendToParent: () -> Void = {}
    /// Opens a file a link or a Read row names: markdown in a file tab,
    /// anything else in its app.
    var openFile: (URL) -> Void = { NSWorkspace.shared.open($0) }
    var copy: (String) -> Void = { WorkspacePasteboard.copy($0) }
    var readPayload: ChatPayloadDetailStore.PayloadReader?
    var readBody: ChatPayloadDetailStore.BodyReader?
}

/// The app's factory: one hosted conformer per spec.
@MainActor
struct LiveChatTranscriptCardFactory: ChatTranscriptCardFactory {
    var theme: MarkdownTheme
    var environment: HostedCardEnvironment
    var actions: ChatTranscriptActions
    /// Routed to every card's `onPayloadChanged`: the native surface
    /// invalidates cached geometry even when the collection item is offscreen.
    var onPayloadChanged: (any TranscriptCard) -> Void = { _ in }
    /// The conversation's remembered open groups, read when a group is
    /// built: it opens when it holds a member the reader left open, and its
    /// toggles are recorded there.
    var disclosures: () -> ChatConversationUIState? = { nil }

    func makeCard(_ spec: ChatTranscriptCardSpec, id: String) -> (any TranscriptCard)? {
        switch spec {
        case .pendingPermission(let permission):
            let actions = actions
            return wire(HostedPermissionCard.permission(permission, theme: theme, environment: environment) { option in
                actions.resolvePermission(permission, option)
            })
        case .attachments(let attachments):
            return wire(HostedAttachmentsCard(
                payload: AttachmentsCardPayload(attachments: attachments),
                actions: .init(quickLook: ChatAttachmentQuickLook()),
                theme: theme, environment: environment
            ))
        case .userTrailer(let presentation):
            let actions = actions
            return wire(HostedUserTrailerCard.userTrailer(
                presentation,
                theme: theme,
                environment: environment,
                copy: actions.copy,
                resend: {
                    guard let input = presentation.input else { return }
                    actions.resolveDelivery(input, true)
                }
            ))
        case .deliveryRecovery(let input, let isMutating):
            guard case .recovery(let presentation) = ChatDeliveryPresentation(input: input) else { return nil }
            let actions = actions
            return wire(HostedDeliveryRecoveryCard(
                payload: DeliveryRecoveryCardPayload(presentation: presentation, isMutating: isMutating),
                actions: .init(onResolve: { retry in actions.resolveDelivery(input, retry) }),
                theme: theme, environment: environment
            ))
        case .activityGroup(let activities, let isActive):
            let disclosures = disclosures()
            return wire(HostedActivityGroupCard.activityGroup(
                activities, isActive: isActive,
                isExpanded: disclosures?.isExpanded(activities.map(\.id)) ?? false,
                theme: theme, environment: environment,
                readPayload: actions.readPayload, readBody: actions.readBody,
                stopBackgroundTask: actions.stopBackgroundTask,
                openThread: actions.openSubagentThread,
                openConversation: actions.openConversation,
                openFile: actions.openFile,
                onExpansionChange: { [weak disclosures] expanded, members in
                    disclosures?.setExpanded(expanded, activityIDs: members.map(\.id))
                }
            ))
        case .turnTrailer(let presentation):
            return wire(HostedTurnTrailerCard.turnTrailer(
                presentation,
                theme: theme,
                environment: environment,
                copy: actions.copy,
                sendToParent: actions.sendToParent
            ))
        case .runFailure(let presentation):
            return wire(HostedRunFailureCard(
                payload: RunFailureCardPayload(presentation: presentation),
                actions: .init(onRetry: { actions.retryFailedRun(presentation.runID) }),
                theme: theme, environment: environment
            ))
        case .permissionRecord(let record):
            return wire(HostedPermissionRecordCard.permissionRecord(
                record, theme: theme, environment: environment, openPlan: actions.openPlan
            ))
        case .mcpSignIn(let signIn):
            let actions = actions
            return wire(HostedMcpSignInCard(
                payload: McpSignInCardPayload(signIn: signIn),
                actions: .init(
                    onSignIn: { actions.signInMcp(signIn.serverID) },
                    onCancel: { actions.cancelMcpSignIn(signIn.serverID) }
                ),
                theme: theme, environment: environment
            ))
        case .continuedAutomatically:
            return wire(HostedContinuationDividerCard(
                payload: ContinuationDividerCardPayload(divider: .continuedAutomatically),
                actions: .init(), theme: theme, environment: environment
            ))
        case .engineSwitched(let divider):
            return wire(HostedContinuationDividerCard(
                payload: ContinuationDividerCardPayload(divider: divider),
                actions: .init(), theme: theme, environment: environment
            ))
        case .delegation(let presentation):
            let actions = actions
            return wire(HostedDelegationCard(
                payload: DelegationCardPayload(presentation: presentation),
                actions: .init(
                    onOpen: { presentation.openConversationID.map(actions.openConversation) },
                    onContinue: actions.continueDelegation
                ),
                theme: theme, environment: environment
            ))
        case .frontmatter(let frontmatter):
            return wire(HostedFrontmatterCard(
                payload: FrontmatterCardPayload(frontmatter: frontmatter), actions: .init(),
                theme: theme, environment: environment
            ))
        case .runtimeRecovery(let recovery, let install, let isChecking):
            return wire(HostedRuntimeRecoveryCard(
                payload: RuntimeRecoveryCardPayload(recovery: recovery, install: install, isChecking: isChecking),
                actions: .init(onInstall: actions.installRuntime, onCheckAgain: actions.checkRuntime),
                theme: theme, environment: environment
            ))
        case .selectionRecovery(let recovery, let isMutating):
            return wire(HostedSelectionRecoveryCard(
                payload: SelectionRecoveryCardPayload(recovery: recovery, isMutating: isMutating),
                actions: .init(onSendAgain: {
                    guard let input = recovery.undeliveredInput else { return }
                    actions.resolveDelivery(input, true)
                }),
                theme: theme, environment: environment
            ))
        case .authRecovery(let recovery, let failure, let isTerminalLogin, let isMutating):
            let actions = actions
            return wire(HostedAuthRecoveryCard(
                payload: AuthRecoveryCardPayload(
                    recovery: recovery, failure: failure, isTerminalLogin: isTerminalLogin, isMutating: isMutating
                ),
                actions: .init(
                    onSignIn: { actions.signIn(recovery) },
                    onCancelSignIn: { actions.cancelSignIn(recovery) },
                    onSendAgain: {
                        guard let input = recovery.undeliveredInput else { return }
                        actions.resolveDelivery(input, true)
                    }
                ),
                theme: theme, environment: environment
            ))
        }
    }

    private func wire<P>(_ card: HostedCard<P>) -> HostedCard<P> {
        let onPayloadChanged = onPayloadChanged
        card.onPayloadChanged = { onPayloadChanged($0) }
        return card
    }
}
