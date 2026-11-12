import OttamBackendClient
import Testing

@testable import OttamPresentationLogic

private func engine(
    authState: EngineAuthState = .ready,
    installState: EngineInstallState = .ready
) -> EngineSummary {
    EngineSummary(
        engineID: EngineID("claude"),
        displayName: "Claude Code",
        authState: authState,
        installState: installState,
        capabilities: [],
        models: [],
        modes: [],
        thoughtLevels: [],
        steeringCapability: .interruptThenPrompt,
        authMethods: [],
        runtime: nil
    )
}

private func composer(
    loadState: ComposerLoadState = .loaded,
    hasWorkspace: Bool = true,
    isSubmitting: Bool = false,
    hasUnresolvedDelivery: Bool = false,
    isWorkingRootAvailable: Bool = true,
    hasLiveRun: Bool = false,
    selectedEngine: EngineSummary? = engine(),
    engines: [EngineSummary] = [engine()]
) -> ComposerPresentation {
    ComposerPresentation(
        loadState: loadState,
        hasWorkspace: hasWorkspace,
        isSubmitting: isSubmitting,
        hasUnresolvedDelivery: hasUnresolvedDelivery,
        isWorkingRootAvailable: isWorkingRootAvailable,
        hasLiveRun: hasLiveRun,
        selectedEngine: selectedEngine,
        engines: engines
    )
}

private func agent(
    id: String = "claude",
    runtime: AgentRuntimeAvailability = .ready,
    component: String? = nil,
    nodeVersion: String? = "v22.0.0",
    lifecycle: AgentLifecycleState = .stopped,
    authState: AgentAuthState = .unknown,
    credentialsSeen: Bool = false,
    lastError: String? = nil,
    adapterPackage: AgentAdapterPackage? = AgentAdapterPackage(
        name: "@zed-industries/claude-code-acp",
        version: "1.0.0",
        source: .npm,
        installState: .installed
    ),
    vendorName: String = "Anthropic",
    vendorHome: String? = "~/.claude",
    accountLabel: String = "Claude account",
    accountKind: AgentAccountKind = .subscription
) -> AgentSummary {
    AgentSummary(
        agentID: AgentID(id),
        displayName: "Agent",
        vendorName: vendorName,
        vendorHome: vendorHome,
        accountLabel: accountLabel,
        accountKind: accountKind,
        runtime: AgentRuntimeStatus(
            availability: runtime,
            component: component,
            nodeVersion: nodeVersion
        ),
        adapter: AgentAdapterStatus(
            lifecycle: lifecycle,
            authState: authState,
            authMethods: [],
            steeringCapability: .unsupported,
            supportsImageInput: false,
            compatibility: [],
            credentialsSeen: credentialsSeen,
            generation: 0,
            lastError: lastError
        ),
        adapterPackage: adapterPackage
    )
}

@Suite("Navigation presentation")
struct NavigationPresentationTests {
    @Test("the Settings page inventory stays explicit")
    func inventories() {
        #expect(SettingsPane.allCases.map(\.rawValue) == ["app", "agents", "mcpServers", "terminal", "storage"])
    }
}

@Suite("Core recovery presentation")
struct CoreRecoveryPresentationTests {
    @Test("all connection phases map to distinct presentation")
    func phases() {
        #expect(
            CoreRecoveryPresentation(phase: .starting)
                == .progress(title: "Starting Core…")
        )
        #expect(CoreRecoveryPresentation(phase: .ready) == .content)
        #expect(
            CoreRecoveryPresentation(phase: .recovering(attempt: 2))
                == .progress(title: "Recovering Core (attempt 2)…")
        )
        #expect(
            CoreRecoveryPresentation(phase: .failed(message: "boom"))
                == .failure(
                    .init(
                        title: "Core is unavailable",
                        retryTitle: "Retry Core",
                        message: "boom"
                    )
                )
        )
    }
}

@Suite("Composer presentation")
struct ComposerPresentationTests {
    @Test("unresolved and failed loads never invent setup")
    func unresolvedLoad() {
        let presentation = composer(
            loadState: .loading,
            selectedEngine: nil,
            engines: []
        )
        #expect(presentation.mode == .preparing)
        #expect(!presentation.setupNeeded)
        #expect(presentation.placeholderOverride == nil)
        #expect(presentation.disabledReason == .preparingEngines)
    }

    @Test("a failed conversation load preserves a usable engine")
    func failedLoad() {
        let available = composer(loadState: .failed)
        #expect(available.mode == .send)
        #expect(available.actionLabel == "Send")
        #expect(available.canSubmit)

        let unavailable = composer(
            loadState: .failed,
            selectedEngine: nil,
            engines: []
        )
        #expect(unavailable.mode == .send)
        #expect(!unavailable.setupNeeded)
        #expect(unavailable.disabledReason == .noSelectedEngine)
    }

    @Test("resolved unusable engines offer setup")
    func setup() {
        let presentation = composer(
            selectedEngine: nil,
            engines: [engine(authState: .missing)]
        )
        #expect(presentation.mode == .setup)
        #expect(presentation.setupNeeded)
        #expect(presentation.placeholderOverride == "Set up an agent to start")
        #expect(presentation.disabledReason == .agentSetupRequired)
    }

    @Test("an unavailable working root says so, ahead of setup")
    func workingRootUnavailable() {
        let unavailable = "This conversation's folder is unavailable"
        #expect(composer(isWorkingRootAvailable: false).placeholderOverride == unavailable)
        let alsoSetup = composer(
            isWorkingRootAvailable: false,
            selectedEngine: nil,
            engines: [engine(authState: .missing)]
        )
        #expect(alsoSetup.placeholderOverride == unavailable)
    }

    @Test("usable engine sends and a live run queues")
    func sendAndQueue() {
        let send = composer()
        #expect(send.actionLabel == "Send")
        #expect(send.canSubmit)
        // No engine, still loading, and the composer stays live: queueing is
        // client state, so nothing about the agent can withhold it.
        let queue = composer(
            loadState: .loading,
            hasLiveRun: true,
            selectedEngine: nil,
            engines: []
        )
        #expect(queue.mode == .queue)
        #expect(queue.actionLabel == "Queue")
        #expect(queue.canSubmit)
    }

    /// The inverse of the gate this replaced: an engine that cannot be steered
    /// used to disable the composer mid-run and flip the button to Stop. Return
    /// now queues on every engine, so only Command-Return is unavailable — and
    /// that is the view's call, not the presentation's.
    @Test("an unsteerable engine still accepts a message mid-run")
    func unsteerableEngineStillQueues() {
        let queue = composer(hasLiveRun: true)
        #expect(queue.disabledReason == nil)
        #expect(queue.canSubmit)
    }

    @Test("every submit blocker maps to a typed reason in guard order")
    func disabledReasons() {
        let cases: [(ComposerPresentation, ComposerDisabledReason)] = [
            (composer(hasWorkspace: false), .workspaceUnavailable),
            (composer(isSubmitting: true), .submissionInProgress),
            (composer(hasUnresolvedDelivery: true), .unresolvedDelivery),
            (composer(isWorkingRootAvailable: false), .workingRootUnavailable),
            (
                composer(selectedEngine: nil, engines: [engine()]),
                .noSelectedEngine
            ),
            (
                composer(
                    selectedEngine: engine(authState: .missing),
                    engines: [engine()]
                ),
                .selectedEngineUnavailable
            ),
        ]
        for (presentation, expected) in cases {
            #expect(presentation.disabledReason == expected)
            #expect(!presentation.canSubmit)
        }
    }
}

@Suite("Chat run status reconciliation")
struct ChatRunStatusReconciliationTests {
    @Test("active run status never regresses to queued")
    func activeStatusIsMonotonic() {
        #expect(
            ChatRunStatusReconciler.reconcile(current: .queued, observed: .running)
                == .running
        )
        #expect(
            ChatRunStatusReconciler.reconcile(current: .running, observed: .queued)
                == .running
        )
    }

    @Test("terminal status ignores late active observations")
    func terminalStatusIsSticky() {
        for status in [
            ConversationRunStatus.completed,
            .failed,
            .canceled,
            .interrupted,
        ] {
            #expect(
                ChatRunStatusReconciler.reconcile(current: status, observed: .running)
                    == status
            )
        }
    }
}

@Suite("Chat delivery presentation")
struct ChatDeliveryPresentationTests {
    @Test("every persisted delivery status has one explicit presentation")
    func statusMatrix() {
        // Routine states say nothing: a message the agent answers needs no badge
        // asserting it arrived. `sending` included — a prompt input holds it
        // until `session/prompt` returns, so it spans the whole turn, and the
        // run's trailer is already reporting that turn.
        for status in [RunInputStatus.pending, .sending] {
            #expect(ChatDeliveryPresentation(
                status: status,
                inputKind: .initial,
                deliveryEvidence: nil
            ) == .hidden)
        }

        let marked = ChatDeliveryPresentation(
            status: .delivered,
            inputKind: .initial,
            deliveryEvidence: .userMarkedDelivered
        )
        #expect(
            marked == .status(
                .init(
                    label: "Marked delivered",
                    isWarning: false,
                    action: nil,
                    isUserMarkedDelivered: true
                )
            )
        )

        let unknown = ChatDeliveryPresentation(
            status: .deliveryUnknown,
            inputKind: .steer,
            deliveryEvidence: nil
        )
        #expect(
            unknown == .recovery(
                ChatDeliveryRecoveryPresentation(kind: .deliveryUnknown)
            )
        )
        if case .recovery(let recovery) = unknown {
            #expect(recovery.title == "Delivery could not be confirmed")
            #expect(
                recovery.detail
                    == "The agent may already have accepted this input. Retry can duplicate work; Mark Delivered records your confirmation without resending it."
            )
            #expect(recovery.primaryAction == .retry)
            #expect(recovery.secondaryAction == .markDelivered)
        }

        let failed = ChatDeliveryPresentation(
            status: .failed,
            inputKind: .initial,
            deliveryEvidence: nil
        )
        if case .status(let status) = failed {
            #expect(status.action == .sendAgain)
        } else {
            Issue.record("failed input did not produce a status presentation")
        }

        let superseded = ChatDeliveryPresentation(
            status: .superseded,
            inputKind: .retry,
            deliveryEvidence: nil
        )
        #expect(superseded == .hidden, "a superseded input is history, not a row")
    }

    @Test("a pending retry is the only pending input that offers recovery")
    func pendingRetry() {
        let presentation = ChatDeliveryPresentation(
            status: .pending,
            inputKind: .retry,
            deliveryEvidence: nil
        )
        #expect(
            presentation == .recovery(
                ChatDeliveryRecoveryPresentation(kind: .pendingRetry)
            )
        )
    }
}

@Suite("Chat auth recovery presentation")
struct ChatAuthRecoveryPresentationTests {
    @Test("signed-out copy distinguishes expired credentials")
    func signedOutCopy() {
        let fresh = ChatAuthRecoveryPresentation(
            phase: .signedOut,
            authState: .missing,
            hasUndeliveredInput: true
        )
        let expired = ChatAuthRecoveryPresentation(
            phase: .signedOut,
            authState: .expired,
            hasUndeliveredInput: true
        )
        #expect(fresh.primaryAction == .signIn)
        #expect(expired.primaryAction == .signInAgain)
        #expect(expired.title(engineDisplayName: "Claude Code").contains("again"))
    }

    @Test("resend requires a ready phase and retained input")
    func resendMatrix() {
        for phase in [ChatAuthRecoveryPhase.signedOut, .signingIn] {
            #expect(!ChatAuthRecoveryPresentation(
                phase: phase,
                authState: .ready,
                hasUndeliveredInput: true
            ).canSendAgain)
            #expect(ChatAuthRecoveryPresentation(
                phase: phase,
                authState: .ready,
                hasUndeliveredInput: true
            ).primaryAction != .sendAgain)
        }
        for phase in [ChatAuthRecoveryPhase.signedIn, .ready] {
            #expect(ChatAuthRecoveryPresentation(
                phase: phase,
                authState: .ready,
                hasUndeliveredInput: true
            ).canSendAgain)
            #expect(!ChatAuthRecoveryPresentation(
                phase: phase,
                authState: .ready,
                hasUndeliveredInput: false
            ).canSendAgain)
            #expect(ChatAuthRecoveryPresentation(
                phase: phase,
                authState: .ready,
                hasUndeliveredInput: false
            ).primaryAction == nil)
        }
    }

    @Test("login failures show only while the card is signed out")
    func failureVisibility() {
        for phase in [
            ChatAuthRecoveryPhase.signingIn,
            .signedIn,
            .ready,
        ] {
            #expect(ChatAuthRecoveryPresentation(
                phase: phase,
                authState: .ready,
                hasUndeliveredInput: true,
                signInFailure: "failed"
            ).visibleFailureMessage == nil)
        }
        #expect(ChatAuthRecoveryPresentation(
            phase: .signedOut,
            authState: .missing,
            hasUndeliveredInput: true,
            signInFailure: "failed"
        ).visibleFailureMessage == "failed")
    }
}

@Suite("Agent Settings presentation")
struct AgentSettingsPresentationTests {
    @Test("identity and pinned packages come from the agent's list entry")
    func identityAndPackage() {
        let presentation = AgentSettingsPresentation(agent: agent())
        #expect(presentation.vendorName == "Anthropic")
        #expect(presentation.accountLabel == "Claude account")
        #expect(presentation.homeFootnote.hasSuffix("pull in what it needs from ~/.claude."))
        #expect(
            presentation.package
                == .npm(name: "@zed-industries/claude-code-acp", version: "1.0.0")
        )
        #expect(presentation.nodeVersion == "v22.0.0")
        #expect(presentation.status == .stopped)
        #expect(presentation.package.needsNode)
    }

    @Test("a prebuilt binary needs no Node")
    func binaryPackage() {
        let presentation = AgentSettingsPresentation(
            agent: agent(
                id: "opencode",
                nodeVersion: nil,
                adapterPackage: AgentAdapterPackage(
                    name: "@opencode/cli",
                    version: "2.2.5",
                    source: .binary,
                    installState: .notInstalled
                )
            )
        )
        #expect(presentation.package == .binary(name: "@opencode/cli", version: "2.2.5"))
        #expect(!presentation.package.needsNode)
        #expect(presentation.runtimeProblem == nil)
        #expect(presentation.installState == .notInstalled)
    }

    @Test("a profile's id names its CLI provider")
    func cliProvider() {
        #expect(CLIProvider(agentID: "opencode") == .opencode)
        #expect(CLIProvider(agentID: "omp.work") == .omp)
        #expect(CLIProvider(agentID: "claude.work")?.displayName == "Claude Code")
        #expect(CLIProvider(agentID: "test.native") == nil)
    }

    /// No id switch anywhere: an agent the app has never heard of renders
    /// from its own data, and an absent vendor home drops the hint rather
    /// than guessing one.
    @Test("an unfamiliar agent renders from its own data")
    func unfamiliarAgent() {
        let presentation = AgentSettingsPresentation(
            agent: agent(
                id: "newcomer",
                adapterPackage: nil,
                vendorName: "Newcomer Labs",
                vendorHome: nil,
                accountLabel: "Newcomer providers",
                accountKind: .multiProvider
            )
        )
        #expect(presentation.vendorName == "Newcomer Labs")
        #expect(presentation.accountLabel == "Newcomer providers")
        #expect(!presentation.homeFootnote.contains("old setup"))
        #expect(presentation.package == .builtIn)
    }

    @Test("sign-in copy follows the account kind")
    func signInCopyFollowsAccountKind() {
        let subscription = AgentSettingsPresentation(agent: agent(authState: .missing))
        #expect(subscription.accountFootnote?.contains("browser") == true)

        for kind: AgentAccountKind in [.apiKey, .multiProvider] {
            let other = AgentSettingsPresentation(
                agent: agent(authState: .missing, accountKind: kind)
            )
            #expect(other.accountFootnote?.contains("browser") == false)
            #expect(other.accountFootnote?.contains("press Done") == true)
        }
    }

    /// A multi-provider agent can always take another provider, so its
    /// terminal setup stays offered after the first one is in.
    @Test("a multi-provider agent keeps offering its terminal setup")
    func multiProviderOffersTerminalSetup() {
        var omp = agent(id: "omp", authState: .ready, accountKind: .multiProvider)
        omp.adapter.authMethods = [
            AgentAuthMethod(methodID: "agent", name: "Use existing local credentials", kind: .agent),
            AgentAuthMethod(methodID: "terminal", name: "Set up omp in terminal", kind: .terminal),
        ]
        omp.modelProviders = ["anthropic", "opencode-go"]
        let configured = AgentSettingsPresentation(agent: omp)
        #expect(configured.providerSetupMethodID == "terminal")
        #expect(configured.accountFootnote == "Set up: anthropic, opencode-go.")

        omp.modelProviders = []
        #expect(AgentSettingsPresentation(agent: omp).accountFootnote?.contains("Set Up Providers") == true)

        // Your CLI's own setup takes providers the same way.
        omp.homeKind = .existing
        #expect(AgentSettingsPresentation(agent: omp).providerSetupMethodID == "terminal")

        var claude = agent(authState: .ready)
        claude.adapter.authMethods = omp.adapter.authMethods
        #expect(AgentSettingsPresentation(agent: claude).providerSetupMethodID == nil)
    }

    /// The catalog keeps an agent's sign-in methods after Remove, and a dead
    /// adapter cannot start a login, so neither offers setup.
    @Test("setup is offered only on an installed, healthy agent")
    func providerSetupNeedsAWorkingAdapter() {
        let installed = AgentAdapterPackage(name: "omp", version: "18.16.0", source: .binary, installState: .installed)
        var omp = agent(id: "omp", authState: .ready, adapterPackage: installed, accountKind: .multiProvider)
        omp.adapter.authMethods = [AgentAuthMethod(methodID: "terminal", name: "Set up omp", kind: .terminal)]
        #expect(AgentSettingsPresentation(agent: omp).providerSetupMethodID == "terminal")

        for state: AgentAdapterInstallState in [.notInstalled, .installing] {
            var removed = omp
            removed.adapterPackage?.installState = state
            #expect(AgentSettingsPresentation(agent: removed).providerSetupMethodID == nil)
        }
        var dead = omp
        dead.adapter.lifecycle = .dead
        #expect(AgentSettingsPresentation(agent: dead).providerSetupMethodID == nil)
    }

    /// A background reinstall after an app update still means the user has
    /// the agent; a seeded profile with nothing installed does not.
    @Test("an agent counts as present while installed or installing")
    func hasAdapterFollowsInstallState() {
        func package(_ state: AgentAdapterInstallState) -> AgentAdapterPackage {
            AgentAdapterPackage(name: "omp", version: "18.16.0", source: .binary, installState: state)
        }
        #expect(AgentSettingsPresentation.hasAdapter(agent(adapterPackage: package(.installed))))
        #expect(AgentSettingsPresentation.hasAdapter(agent(adapterPackage: package(.installing))))
        #expect(!AgentSettingsPresentation.hasAdapter(agent(adapterPackage: package(.notInstalled))))
        #expect(AgentSettingsPresentation.hasAdapter(agent(adapterPackage: nil)), "built-in agents install nothing")
    }

    @Test("the Agents pane answers to each listed agent's names")
    func agentsSearchUsesTheList() {
        let generic = SettingsPane.agents.searchText()
        #expect(!generic.localizedCaseInsensitiveContains("Newcomer"))

        let listed = SettingsPane.agents.searchText(agents: [
            agent(id: "newcomer", vendorName: "Newcomer Labs"),
        ])
        #expect(listed.localizedCaseInsensitiveContains("newcomer labs"))
        #expect(SettingsPane.app.searchText(agents: [agent()]).contains("Anthropic") == false)
    }

    @Test("runtime, lifecycle, and auth problems derive actions and copy")
    func stateMatrix() {
        let runtime = AgentSettingsPresentation(
            agent: agent(runtime: .missing, component: "node", authState: .ready)
        )
        #expect(runtime.runtimeProblem == .missing(component: "node"))
        #expect(!runtime.wantsSignIn)

        let dead = AgentSettingsPresentation(
            agent: agent(lifecycle: .dead, lastError: "adapter failed")
        )
        #expect(dead.accountFootnote == "adapter failed")

        for credentialsSeen in [false, true] {
            let signedOut = AgentSettingsPresentation(
                agent: agent(
                    authState: .missing,
                    credentialsSeen: credentialsSeen
                )
            )
            #expect(
                signedOut.status == (credentialsSeen ? .expired : .signedOut)
            )
            #expect(signedOut.wantsSignIn)
            #expect(signedOut.accountFootnote?.contains("login terminal") == true)
        }
    }
}

private func feature(
    _ name: String,
    group: AgentFeatureGroup = .agentCapability,
    sdk: AgentFeatureSdkStatus = .stable,
    sdkDetail: String? = nil,
    agent: Bool? = nil,
    client: AgentFeatureClientSupport = .supported,
    clientDetail: String? = nil,
    verdict: AgentFeatureVerdict = .unknown
) -> AgentFeatureCompatibility {
    AgentFeatureCompatibility(
        feature: name,
        group: group,
        sdkStatus: sdk,
        sdkDetail: sdkDetail,
        agentAdvertises: agent,
        clientSupport: client,
        clientDetail: clientDetail,
        verdict: verdict
    )
}

@Suite("Agent compatibility presentation")
struct AgentCompatibilityPresentationTests {
    @Test("the summary counts supported and partial rows, with no denominator")
    func summaryCounts() {
        let presentation = AgentCompatibilityPresentation(compatibility: [
            feature("loadSession", client: .supported),
            feature("usage_update", client: .partial, clientDetail: "persisted only"),
            feature("fs.readTextFile", client: .unsupported),
        ])
        #expect(presentation.summary == "2 ACP features supported")
        #expect(presentation.isUnprobed)

        let single = AgentCompatibilityPresentation(compatibility: [
            feature("loadSession", client: .supported),
        ])
        #expect(single.summary == "1 ACP feature supported")

        let empty = AgentCompatibilityPresentation(compatibility: [])
        #expect(empty.summary == "0 ACP features supported")
        #expect(empty.sections.isEmpty)
    }

    @Test("sections follow protocol order and drop empty groups")
    func sectionOrder() {
        let presentation = AgentCompatibilityPresentation(compatibility: [
            feature("terminal", group: .clientCapability, agent: true),
            feature("loadSession", group: .agentCapability, agent: true),
            feature("session/cancel", group: .agentMethod, agent: false),
            feature("jetbrains.air.asyncTasks", group: .extension, sdk: .extension, sdkDetail: "jetbrains"),
        ])
        #expect(presentation.sections.map(\.title) == [
            "Agent capabilities", "Agent methods", "Client capabilities", "Extensions",
        ])
        #expect(!presentation.isUnprobed)
        #expect(presentation.sections.last?.rows.first?.sdkText == "jetbrains")
    }

    @Test("each verdict has its own symbol and label")
    func verdictRows() {
        let presentation = AgentCompatibilityPresentation(compatibility: [
            feature("a", agent: true, verdict: .works),
            feature("b", agent: true, client: .unsupported, verdict: .agentOnly),
            feature("c", agent: false, verdict: .clientOnly),
            feature("d", agent: false, client: .unsupported, verdict: .neither),
            feature("e", sdk: .unstableOff, sdkDetail: "unstable_elicitation", verdict: .unknown),
        ])
        let rows = presentation.sections.flatMap(\.rows)
        #expect(rows.map(\.verdictLabel) == ["Works", "Agent only", "Ottam only", "Neither", "Unknown"])
        #expect(Set(rows.map(\.verdictSymbol)).count == 5)
        #expect(rows.map(\.agentText) == ["Yes", "Yes", "No", "No", "—"])
        #expect(rows.map(\.clientText) == ["Yes", "No", "Yes", "No", "Yes"])
        #expect(rows.last?.sdkText == "Unstable, off")
        #expect(rows.last?.agentDetail?.contains("once a session runs") == true)
        #expect(rows.first?.agentDetail == nil)
    }

    @Test("empty cells say why, and partial cells keep the detail for the tooltip")
    func cellDetails() {
        let unprobed = AgentCompatibilityPresentation(compatibility: [
            feature("usage_update", client: .partial, clientDetail: "persisted only"),
        ])
        let row = unprobed.sections.first?.rows.first
        #expect(row?.clientText == "Partial")
        #expect(row?.clientDetail == "persisted only")
        #expect(row?.agentText == "—")
        #expect(row?.agentDetail?.contains("not been started") == true)
    }
}
