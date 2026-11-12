import OttamAccessibility
import MacAutomationKit
import ScenarioKit

public extension UIScenarioDefinition {
    static let workspaceCoreReplacement = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "workspace.core-replacement",
            description: "Replace the exact owned Core and retain the visible Workspace.",
            tags: ["workspace", "core", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "real Rust ottam-core"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            environment: ["OTTAM_UI_TEST_WORKSPACE_COUNT": "2"],
            fixtures: ["exact owned Core lifecycle"],
            fixtureRequirements: [.coreLifecycle, .workspaceWindows]
        ),
        preparation: waitForChat
    ) { context in
        let workspace = context.element(id: AccessibilityID.workspaceRoot)
        try await context.assertFixture("workspace-window-lifecycle", timeout: 20)
        try await context.waitFor(
            .identifierCountEquals(workspace, 2),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "workspace-before-core-replacement",
                scope: nil,
                required: [
                    AccessibilityID.workspaceRoot,
                    AccessibilityID.chatComposer,
                ],
                auditExceptions: workspaceWindowAuditExceptions
            )
        )
        try await context.terminateOwnedCore(expectReplacement: true)
        try await context.waitFor(.labelContains(workspace, "core ready"), timeout: 20)
        try await context.waitFor(
            .identifierCountEquals(workspace, 2),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "workspace-after-core-replacement",
                scope: nil,
                required: [
                    AccessibilityID.workspaceRoot,
                    AccessibilityID.chatComposer,
                ],
                auditExceptions: workspaceWindowAuditExceptions
            )
        )
    }

    static let coreRecoveryPresentation = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "core.recovery-presentation",
            description: "Keep reconnecting quiet and present recovery only after exhaustion.",
            tags: ["core", "recovery", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "real Rust ottam-core"]
        ),
        launchConfiguration: ScenarioLaunchConfiguration(
            validationProfile: "focused",
            routes: "chat",
            fixtures: ["temporarily unavailable staged Core executable"],
            fixtureRequirements: [.coreFailure]
        ),
        preparation: waitForChat
    ) { context in
        let workspace = context.element(id: AccessibilityID.workspaceRoot)
        let sheet = context.element(id: AccessibilityID.coreRecoverySheet)
        let retry = context.element(id: AccessibilityID.coreRecoveryRetry)
        try context.setBundledCoreAvailable(false)
        try await context.terminateOwnedCore(expectReplacement: false)
        try await context.waitFor(
            .labelContains(workspace, "core recovering"),
            timeout: 8
        )
        try await context.assert(.notExists(sheet))
        try await context.capture(
            routeCapture(
                id: "core-reconnecting-quiet",
                scope: nil,
                required: [AccessibilityID.workspaceRoot]
            )
        )
        try await context.waitFor(.exists(sheet), timeout: 15)
        try await context.capture(
            routeCapture(
                id: "core-recovery-exhausted",
                scope: nil,
                required: [
                    AccessibilityID.coreRecoverySheet,
                    AccessibilityID.coreRecoveryRetry,
                ]
            )
        )
        try context.setBundledCoreAvailable(true)
        try await context.press(retry)
        try await context.waitFor(.notExists(sheet), timeout: 20)
        try await context.waitFor(
            .labelContains(workspace, "core ready"),
            timeout: 20
        )
    }

    static let chatInterruptAndContinue = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.interrupt-and-continue",
            description: "Interrupt an ACP turn and continue with retained partial output.",
            tags: ["chat", "steering", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let running = context.element(id: AccessibilityID.chatRunStatus)
        let partial = context.element(id: AccessibilityID.chatPartialOutput)
        try context.armACP(.holdUntilCancel)
        try await context.setValue("Hold the first ACP turn for steering", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(running), timeout: 15)
        try await context.waitForACPPromptCount(1, timeout: 10)
        try await context.capture(
            routeCapture(
                id: "chat-steering-active",
                required: [
                    AccessibilityID.chatRunStatus,
                    AccessibilityID.chatStop,
                ]
            )
        )
        try context.armACP(.complete)
        try await context.setValue("Continue with revised direction", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(partial), timeout: 20)
        try await context.waitFor(.notExists(running), timeout: 20)
        try await context.waitForACPPromptCount(2, timeout: 15)
        try context.assertACPPromptCount(2)
        try await context.capture(
            routeCapture(
                id: "chat-steering-partial",
                required: [
                    AccessibilityID.chatPartialOutput,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }

    static let chatNativeSteerAndStop = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.native-steer-and-stop",
            description: "Deliver native steering on the active run and stop it visibly.",
            tags: ["chat", "steering", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "native steering fixture"]
        ),
        launchConfiguration: chatLaunch(nativeSteering: true),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let running = context.element(id: AccessibilityID.chatRunStatus)
        let stop = context.element(id: AccessibilityID.chatStop)
        try await context.assertFixture("chat-engine-selection", timeout: 15)
        try await context.setValue("Start native steering turn", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(running), timeout: 15)
        try await context.focus(composer)
        try await context.setValue("Apply native steering", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(running), timeout: 10)
        try await context.waitFor(.focused(composer, true), timeout: 5)
        try await context.capture(
            routeCapture(
                id: "chat-native-steering",
                required: [
                    AccessibilityID.chatRunStatus,
                    AccessibilityID.chatStop,
                    AccessibilityID.chatComposer,
                ],
                requiredActions: [
                    AXRequiredAction(
                        identifier: AccessibilityID.chatStop,
                        actions: ["AXPress"]
                    ),
                ],
                requiredFocusedIdentifier: AccessibilityID.chatComposer
            )
        )
        try await context.press(stop)
        try await context.waitFor(.notExists(running), timeout: 15)
        try await context.capture(
            routeCapture(
                id: "chat-native-stopped",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    static let chatQueueDrain = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.queue-drain",
            description: "Queue two messages behind a streaming turn and watch them drain in order.",
            tags: ["chat", "steering", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let running = context.element(id: AccessibilityID.chatRunStatus)
        let queued = context.element(id: AccessibilityID.chatQueuedMessages)

        // Streamed rather than `.holdUntilCancel`: this turn has to end by
        // itself, because ending it by cancel is exactly the case that holds
        // the queue instead of draining it. Only the first prompt is armed —
        // `armStreamedACP` arms one — so the two drained messages take the
        // fake's default `Complete` and land immediately.
        try context.armStreamedACP(
            reply: String(
                repeating: "The turn keeps streaming while messages queue behind it. ",
                count: 40
            ),
            chunkBytes: 24,
            chunkDelayMillis: 80
        )
        try await context.setValue("Start a turn long enough to queue behind", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(running), timeout: 15)
        try await context.waitForACPPromptCount(1, timeout: 10)

        // Each message is confirmed queued before the next is typed. Queueing
        // empties the draft, which under a live run turns the primary button
        // back into Stop, so the composer has to be observed settling rather
        // than driven blind.
        try await context.setValue("First queued message", on: composer)
        try await context.press(submit)
        try await context.waitFor(.valueEquals(queued, "1 queued"), timeout: 10)
        try await context.setValue("Second queued message", on: composer)
        try await context.press(submit)
        try await context.waitFor(.valueEquals(queued, "2 queued"), timeout: 10)

        // The run is still working, and neither queued message has reached the
        // adapter: queueing is a client-side hold, not an early send.
        try context.assertACPPromptCount(1)
        try await context.capture(
            routeCapture(
                id: "chat-queue-held",
                required: [
                    AccessibilityID.chatRunStatus,
                    AccessibilityID.chatQueuedMessages,
                    AccessibilityID.chatComposer,
                ]
            )
        )

        // The popover is its own window, so the capture scopes to the popover
        // rather than to the Workspace — scoping to neither means walking the
        // whole application tree, which overruns the node budget. The audit,
        // not the screenshot, is what asserts its contents.
        try await context.press(queued)
        try await context.capture(
            routeCapture(
                id: "chat-queue-popover",
                scope: AccessibilityID.chatQueuedMessagesPopover,
                required: [
                    AccessibilityID.chatQueuedMessageSendNow,
                    AccessibilityID.chatQueuedMessageRemove,
                ],
                requiredActions: [
                    AXRequiredAction(
                        identifier: AccessibilityID.chatQueuedMessageSendNow,
                        actions: ["AXPress"]
                    ),
                    AXRequiredAction(
                        identifier: AccessibilityID.chatQueuedMessageRemove,
                        actions: ["AXPress"]
                    ),
                ],
                auditExceptions: queueRowAuditExceptions
            )
        )
        // Left open deliberately. The popover is anchored to the pill, and the
        // pill unmounts with the last queued message, so the drain dismisses it
        // — which is the behavior worth exercising, and there is no key-press
        // helper in the scenario contract to dismiss it any other way.

        // Three prompts, one per message, and the queue is empty: the head
        // drained when the turn completed, and the second followed it.
        try await context.waitForACPPromptCount(3, timeout: 30)
        try await context.waitFor(.notExists(running), timeout: 20)
        try await context.waitFor(.notExists(queued), timeout: 10)
        try await context.capture(
            routeCapture(
                id: "chat-queue-drained",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    static let chatDeliveryRetry = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.delivery-retry",
            description: "Recover an ambiguous Chat delivery through the visible Retry action.",
            tags: ["chat", "delivery", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(coreLifecycle: true),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let card = context.element(id: AccessibilityID.chatDeliveryRecovery)
        let retry = context.element(id: AccessibilityID.chatDeliveryRetry)
        try context.armACP(.neverAcknowledge)
        try await context.setValue("Exercise delivery ambiguity", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 10)
        try await context.terminateOwnedCore(expectReplacement: true)
        try await context.waitFor(.valueEquals(card, "delivery unknown"), timeout: 20)
        try context.assertACPPromptCount(1)
        try await context.capture(
            routeCapture(
                id: "chat-delivery-unknown",
                required: [
                    AccessibilityID.chatDeliveryRecovery,
                    AccessibilityID.chatDeliveryRetry,
                    AccessibilityID.chatDeliveryMarkDelivered,
                ],
                requiredActions: [
                    AXRequiredAction(
                        identifier: AccessibilityID.chatDeliveryRetry,
                        actions: ["AXPress"]
                    ),
                    AXRequiredAction(
                        identifier: AccessibilityID.chatDeliveryMarkDelivered,
                        actions: ["AXPress"]
                    ),
                ]
            )
        )
        try context.armACP(.complete)
        try await context.press(retry)
        try await context.waitFor(.notExists(card), timeout: 20)
        try await context.waitForACPPromptCount(2, timeout: 15)
        try context.assertACPPromptCount(2)
        try await context.capture(
            routeCapture(
                id: "chat-delivery-retried",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    static let chatDeliveryMarkDelivered = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.delivery-mark-delivered",
            description: "Resolve ambiguous Chat delivery without resending it.",
            tags: ["chat", "delivery", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(coreLifecycle: true),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let card = context.element(id: AccessibilityID.chatDeliveryRecovery)
        let mark = context.element(id: AccessibilityID.chatDeliveryMarkDelivered)
        let marked = context.element(id: AccessibilityID.chatMarkedDelivered)
        try context.armACP(.neverAcknowledge)
        try await context.setValue("Exercise mark-delivered recovery", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 10)
        try await context.terminateOwnedCore(expectReplacement: true)
        try await context.waitFor(.exists(card), timeout: 20)
        try await context.press(mark)
        try await context.waitFor(.notExists(card), timeout: 15)
        try await context.waitFor(.exists(marked), timeout: 10)
        try context.assertACPPromptCount(1)
        try await context.capture(
            routeCapture(
                id: "chat-delivery-marked",
                required: [
                    AccessibilityID.chatMarkedDelivered,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }

    static let chatPendingRetryResume = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.pending-retry-resume",
            description: "Resume a durably persisted retry after its first launch fails.",
            tags: ["chat", "delivery", "recovery", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(coreLifecycle: true),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let card = context.element(id: AccessibilityID.chatDeliveryRecovery)
        let retry = context.element(id: AccessibilityID.chatDeliveryRetry)
        let startRetry = context.element(id: AccessibilityID.chatPendingRetryStart)
        try context.armACP(.neverAcknowledge)
        try await context.setValue("Exercise persisted retry recovery", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 10)
        try await context.terminateOwnedCore(expectReplacement: true)
        try await context.waitFor(.valueEquals(card, "delivery unknown"), timeout: 20)

        try context.setACPExecutableAvailable(false)
        try await context.press(retry)
        try await context.waitFor(.valueEquals(card, "pending retry"), timeout: 20)
        try context.assertACPPromptCount(1)
        try await context.capture(
            routeCapture(
                id: "chat-pending-retry-ready",
                required: [
                    AccessibilityID.chatDeliveryRecovery,
                    AccessibilityID.chatPendingRetryStart,
                ],
                requiredActions: [
                    AXRequiredAction(
                        identifier: AccessibilityID.chatPendingRetryStart,
                        actions: ["AXPress"]
                    ),
                ]
            )
        )

        try context.setACPExecutableAvailable(true)
        try await context.terminateOwnedCore(expectReplacement: true)
        try await context.waitFor(.valueEquals(card, "pending retry"), timeout: 20)
        try context.armACP(.complete)
        try await context.press(startRetry)
        try await context.waitFor(.notExists(card), timeout: 20)
        try await context.waitForACPPromptCount(2, timeout: 15)
        try context.assertACPPromptCount(2)
        try await context.capture(
            routeCapture(
                id: "chat-pending-retry-resumed",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    static let chatAuthRecovery = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.auth-recovery",
            description: "Sign in after an auth failure and send the retained message again.",
            tags: ["chat", "auth", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(signIn: true),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let card = context.element(id: AccessibilityID.chatAuthRecovery)
        let signIn = context.element(id: AccessibilityID.chatAuthSignIn)
        let sendAgain = context.element(id: AccessibilityID.chatAuthSendAgain)
        try context.armACP(.authRequired)
        try await context.setValue("Exercise expired credentials", on: composer)
        try await context.press(submit)
        try await context.waitFor(.valueEquals(card, "signed out"), timeout: 20)
        try await context.capture(
            routeCapture(
                id: "chat-auth-signed-out",
                required: [
                    AccessibilityID.chatAuthRecovery,
                    AccessibilityID.chatAuthSignIn,
                ],
                requiredActions: [
                    AXRequiredAction(
                        identifier: AccessibilityID.chatAuthSignIn,
                        actions: ["AXPress"]
                    ),
                ]
            )
        )
        try context.armACP(.complete)
        try await context.press(signIn)
        try await context.waitFor(.valueEquals(card, "signing in"), timeout: 8)
        try await context.capture(
            routeCapture(
                id: "chat-auth-signing-in",
                scope: AccessibilityID.chatAuthRecovery,
                required: [AccessibilityID.chatAuthRecovery],
                snapshotPolicy: .animatedRoute
            )
        )
        try context.completeACPSignIn()
        try await context.waitFor(.valueEquals(card, "signed in"), timeout: 15)
        try await context.capture(
            routeCapture(
                id: "chat-auth-signed-in",
                required: [
                    AccessibilityID.chatAuthRecovery,
                    AccessibilityID.chatAuthSendAgain,
                ]
            )
        )
        try await context.press(sendAgain)
        try await context.waitFor(.notExists(card), timeout: 20)
        try await context.waitForACPPromptCount(2, timeout: 15)
        try context.assertACPPromptCount(2)
        try await context.capture(
            routeCapture(
                id: "chat-auth-recovered",
                required: [AccessibilityID.chatComposer]
            )
        )
    }

    /// The approval, from asked to answered to remembered. The card renders inline
    /// in the thread at the call it gates, so the capture has to show it there with
    /// the run status and composer still inside the window. Answering it must then
    /// leave a record behind — and that record has to survive leaving the
    /// conversation and coming back, which is the only thing that proves it is being
    /// rebuilt from persisted events rather than held in the live run.
    static let chatPermissionPrompt = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.permission-prompt",
            description: "Approve a tool call inline and keep the decision in the transcript.",
            tags: ["chat", "permissions", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let card = context.element(id: AccessibilityID.chatPermissionCard)
        // The approval has to land in a conversation the thread already knows
        // about, so take one plain turn first rather than asking for approval
        // on the turn that creates the conversation.
        try context.armACP(.complete)
        try await context.setValue("Open the thread", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)
        try await context.capture(
            routeCapture(
                id: "chat-permission-baseline",
                required: [
                    AccessibilityID.chatComposer,
                    AccessibilityID.chatSubmit,
                ]
            )
        )
        try context.armACP(.requestPermission)
        try await context.setValue("Run something that needs approval", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(card), timeout: 20)
        try await context.capture(
            routeCapture(
                id: "chat-permission-pending",
                required: [
                    AccessibilityID.chatPermissionCard,
                    AccessibilityID.chatRunStatus,
                    AccessibilityID.chatComposer,
                    AccessibilityID.chatStop,
                ]
            )
        )

        // Answering it swaps the card for the record, in the same place.
        let record = context.element(id: AccessibilityID.chatPermissionRecord)
        // `waitForText` needs an exactly-matching AX string, and the record row's
        // label merges with the command beside it — so settle for the Stop button,
        // which is on screen throughout. The record element below is the assertion.
        try await context.pressControl(named: "Allow Once", waitForText: "Stop")
        try await context.waitFor(.exists(record), timeout: 20)
        try await context.capture(
            routeCapture(
                id: "chat-permission-allowed",
                required: [
                    AccessibilityID.chatPermissionRecord,
                    AccessibilityID.chatComposer,
                ]
            )
        )

        // The permission decision keeps its normalized identity through the
        // terminal Run boundary and remains visible in durable history.
        try await context.press(context.element(id: AccessibilityID.chatStop))
        // Wait for live status to settle before re-checking the durable record.
        try await context.waitFor(
            .notExists(context.element(id: AccessibilityID.chatRunStatus)),
            timeout: 20
        )
        try await context.waitFor(.exists(record), timeout: 20)
        try await context.capture(
            routeCapture(
                id: "chat-permission-recorded",
                required: [
                    AccessibilityID.chatPermissionRecord,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }

    /// An agent's question, from asked to answered. The panel rides above the
    /// composer as a second glass card while the run waits; answering it
    /// closes the panel and leaves the answer on the asking tool's row.
    static let chatQuestionPanel = UIScenarioDefinition(
        descriptor: ScenarioDescriptor(
            id: "chat.question-panel",
            description: "Answer an agent's question above the composer and keep the answer on its tool call.",
            tags: ["chat", "questions", "acceptance"],
            requirements: ["Accessibility", "Screen Recording", "ottam-test-acp"]
        ),
        launchConfiguration: chatLaunch(),
        preparation: waitForChat
    ) { context in
        let composer = context.element(id: AccessibilityID.chatComposer)
        let submit = context.element(id: AccessibilityID.chatSubmit)
        let panel = context.element(id: AccessibilityID.chatQuestionPanel)
        // As with approvals, ask in a conversation the thread already knows.
        try context.armACP(.complete)
        try await context.setValue("Open the thread", on: composer)
        try await context.press(submit)
        try await context.waitForACPPromptCount(1, timeout: 15)

        try context.armACP(.askQuestion)
        try await context.setValue("Ask me which approach to take", on: composer)
        try await context.press(submit)
        try await context.waitFor(.exists(panel), timeout: 20)
        try await context.capture(
            routeCapture(
                id: "chat-question-pending",
                required: [
                    AccessibilityID.chatQuestionPanel,
                    AccessibilityID.chatQuestionOther,
                    AccessibilityID.chatQuestionSkip,
                    AccessibilityID.chatQuestionSubmit,
                    AccessibilityID.chatComposer,
                    AccessibilityID.chatStop,
                ]
            )
        )

        // The options read as a radio group; pick one as VoiceOver would.
        // Not the Other field: an AX value write doesn't reach a SwiftUI
        // text field's binding, so typing there is checked by hand.
        try await context.pressControl(named: "Rewrite", waitForText: "Skip")
        try await context.press(context.element(id: AccessibilityID.chatQuestionSubmit))
        try await context.waitFor(.notExists(panel), timeout: 20)
        let record = context.element(id: AccessibilityID.chatQuestionRecord)
        try await context.waitFor(.valueEquals(record, "answered"), timeout: 20)
        try await context.waitFor(
            .notExists(context.element(id: AccessibilityID.chatRunStatus)),
            timeout: 20
        )
        try await context.capture(
            routeCapture(
                id: "chat-question-answered",
                required: [
                    AccessibilityID.chatQuestionRecord,
                    AccessibilityID.chatComposer,
                ]
            )
        )
    }
}

func waitForChat(_ context: any UIScenarioContext) async throws {
    try await context.waitFor(
        .exists(context.element(id: AccessibilityID.chatComposer)),
        timeout: 20
    )
}

private func chatLaunch(
    nativeSteering: Bool = false,
    coreLifecycle: Bool = false,
    signIn: Bool = false
) -> ScenarioLaunchConfiguration {
    var requirements: [ScenarioFixtureRequirement] =
        nativeSteering ? [.nativeSteering] : [.testACP]
    if coreLifecycle {
        requirements.append(.coreLifecycle)
    }
    if signIn {
        requirements.append(.testACPSignIn)
    }
    return ScenarioLaunchConfiguration(
        validationProfile: "focused",
        routes: "chat",
        fixtures: nativeSteering
            ? ["deterministic native steering engine"]
            : ["deterministic Rust ACP adapter"],
        fixtureRequirements: requirements
    )
}

func routeCapture(
    id: String,
    controlName: String = "route-chat",
    scope: String? = AccessibilityID.workspaceRoot,
    windowGroupFixtureName: String? = nil,
    required: [String],
    requiredActions: [AXRequiredAction] = [],
    requiredFocusedIdentifier: String? = nil,
    auditExceptions: [AXAuditException] = [],
    snapshotPolicy: SemanticSnapshotPolicy = .route
) -> ScenarioCaptureRequest {
    ScenarioCaptureRequest(
        controlName: controlName,
        captureID: id,
        scopeIdentifier: scope,
        windowGroupFixtureName: windowGroupFixtureName,
        snapshotPolicy: snapshotPolicy,
        auditPolicy: .route(
            requiredIdentifiers: required,
            uniqueIdentifiers: AccessibilityID.all,
            requiredActions: requiredActions,
            requiredFocusedIdentifier: requiredFocusedIdentifier,
            exceptions: auditExceptions
        )
    )
}

/// The queue popover repeats its row controls, one pair per queued message,
/// which is what a list of rows is. `routeCapture` holds every registered
/// identifier to be unique, so the repetition is declared here rather than
/// worked around with per-row identifiers nothing would assert on.
let queueRowAuditExceptions = [
    AccessibilityID.chatQueuedMessageSendNow,
    AccessibilityID.chatQueuedMessageRemove,
].map { (identifier: String) in
    AXAuditException(
        rule: .duplicateIdentifier,
        identifier: identifier,
        reason: "The queue popover lists one \(identifier) per queued message."
    )
}

let workspaceWindowAuditExceptions = [
    AccessibilityID.workspaceRoot,
    AccessibilityID.workspaceSidebar,
    AccessibilityID.workspaceDetail,
    AccessibilityID.routeChat,
    AccessibilityID.chatComposer,
    AccessibilityID.chatSubmit,
    AccessibilityID.chatEngineMenu,
    AccessibilityID.sidebarIdentityBlock,
].map { (identifier: String) in
    AXAuditException(
        rule: .duplicateIdentifier,
        identifier: identifier,
        reason: "The scenario intentionally opens two Workspace windows, each with one \(identifier)."
    )
}
