import AppKit
import OttamAccessibility
import OttamBackendClient
import OttamPresentationLogic
import OttamWorkspaceLogic
import SwiftUI

/// The first-launch window: the brand intro, then choose agents, install,
/// sign in, and a first named agent. Opens instead of the Welcome window until
/// it finishes.
struct OnboardingView: View {
    static let windowID = "onboarding"
    static let windowSize = CGSize(width: 620, height: 620)

    let runtime: OttamAppRuntime
    @State private var model: OnboardingModel
    @State private var window = OnboardingWindowReference()
    @Environment(\.openWindow) private var openWindow

    init(runtime: OttamAppRuntime) {
        self.runtime = runtime
        _model = State(initialValue: OnboardingModel(runtime: runtime))
    }

    private var isCoreReady: Bool {
        if case .ready = runtime.connectionState { true } else { false }
    }

    /// Adapter install phases for the chosen agents; a change means
    /// `agent.list` has news.
    private var installPhases: [String] {
        model.selected.map { "\($0):\(String(describing: runtime.agentInstall.state(for: AgentID($0)).phase))" }
    }

    var body: some View {
        ZStack {
            if model.step == .intro {
                OnboardingIntro { Task { await model.finishIntro() } }
                    .transition(.opacity)
            } else if !isCoreReady {
                // Onboarding suppresses the Welcome window, so it carries the
                // same way back from a core that failed to start.
                WorkspaceChooserRecoveryView(
                    state: runtime.connectionState,
                    exitError: runtime.coreExitError,
                    retry: runtime.retryConnection
                )
            } else {
                OnboardingStepPage(model: model, finish: finish)
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.5), value: model.step)
        .frame(width: Self.windowSize.width, height: Self.windowSize.height)
        .background(OnboardingWindowProbe(reference: window))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(AccessibilityID.onboardingWindow)
        .task { await model.start() }
        .task { await model.agents.observeEngineChanges() }
        .task(id: model.agents.refreshGeneration) {
            if model.agents.didLoadOnce { await model.agents.load() }
            // A catalog probe landing changes the models, not the agents.
            if model.step == .namedAgent { await model.reloadNamedAgents() }
        }
        .onChange(of: model.agents.agents) {
            model.drive()
            if model.step == .signIn { model.probeUnknownSignIns() }
        }
        .onChange(of: isCoreReady) { _, ready in
            if ready { Task { await model.coreBecameReady() } }
        }
        .onChange(of: runtime.runtimeInstall.state.phase) { _, phase in model.runtimePhaseChanged(phase) }
        .onChange(of: installPhases) { model.agents.requestReload() }
        .onDisappear { model.signIn.detach() }
        .sheet(item: Binding(
            get: { model.signIn.revealed },
            set: { if $0 == nil { model.signIn.dismissRevealedTerminal() } }
        )) { revealed in
            AgentSignInTerminalSheet(revealed: revealed, signIn: model.signIn)
        }
    }

    private func finish() {
        guard model.finish() else { return }
        openWindow(id: "workspaceChooser")
        // `dismissWindow` honors `.windowDismissBehavior(.disabled)`;
        // AppKit's close does not, and only finishing gets here.
        window.window?.close()
    }
}

/// The hosting window, for closing it once onboarding finishes.
@MainActor
private final class OnboardingWindowReference {
    weak var window: NSWindow?
}

private struct OnboardingWindowProbe: NSViewRepresentable {
    let reference: OnboardingWindowReference

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { reference.window = view.window }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        DispatchQueue.main.async { reference.window = view.window }
    }
}

/// The thread draws in, the wordmark fades in below it, and the pair holds
/// for a moment before onboarding starts. Reduce Motion shows both at once.
private struct OnboardingIntro: View {
    let onFinished: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var showsWordmark = false

    private var skipsIntro: Bool {
        ProcessInfo.processInfo.environment["OTTAM_UI_TEST_MODE"] == "1"
    }

    var body: some View {
        VStack(spacing: 0) {
            OttamThreadMark(motion: reduceMotion || skipsIntro ? .still : .drawIn)
                .frame(width: 160, height: 160)
                .padding(.vertical, -24)
            OttamWordmark()
                .frame(height: 32)
                .padding(.top, 20)
                .opacity(showsWordmark ? 1 : 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task {
            guard !skipsIntro else {
                onFinished()
                return
            }
            // The mark's draw-in: 0.2 s delay and 1.1 s travel.
            if !reduceMotion { try? await Task.sleep(for: .milliseconds(1300)) }
            withAnimation(reduceMotion ? nil : .easeIn(duration: 0.6)) { showsWordmark = true }
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            onFinished()
        }
    }
}

private struct OnboardingStepPage: View {
    @Bindable var model: OnboardingModel
    let finish: () -> Void
    @State private var editingNamedAgent: NamedAgentDraft?

    private var title: String {
        switch model.step {
        case .intro, .chooseAgents: "Choose your agents"
        case .install: "Installing"
        case .signIn: "Sign in"
        case .namedAgent: "Add named agents"
        }
    }

    private var subtitle: String {
        switch model.step {
        case .intro, .chooseAgents:
            "Ottam installs what each agent needs, including Node.js if your Mac doesn’t have it."
        case .install:
            "Installing Ottam’s support for the agents you chose."
        case .signIn:
            "Sign in to each agent with its own account."
        case .namedAgent:
            "Roles your conversations can hand tasks to. Add at least one; change them later in Settings › Agents."
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Step \(model.step.rawValue) of 4")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Text(title).font(.title.bold())
                Text(subtitle)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 28)
            .padding(.top, 12)
            .padding(.bottom, 8)

            Form {
                switch model.step {
                case .intro, .chooseAgents: OnboardingChooseAgents(model: model)
                case .install: OnboardingInstall(model: model)
                case .signIn: OnboardingSignIn(model: model)
                case .namedAgent: OnboardingNamedAgents(model: model, editing: $editingNamedAgent)
                }
                if let agentListError = model.agentListError {
                    Section {
                        Text(agentListError).foregroundStyle(.red).textSelection(.enabled)
                        Button("Try Again") { Task { await model.retryAgentList() } }
                    }
                }
                if let errorText = model.errorText {
                    Section { Text(errorText).foregroundStyle(.red).textSelection(.enabled) }
                }
            }
            .formStyle(.grouped)
            .sheet(item: $editingNamedAgent) { draft in
                NamedAgentSheet(model: model.namedAgents, draft: draft)
            }

            HStack {
                Spacer()
                if model.step > .chooseAgents {
                    Button("Back") { model.goBack() }
                        .buttonStyle(.bordered)
                        .disabled(!model.canGoBack)
                        .accessibilityIdentifier(AccessibilityID.onboardingBack)
                }
                primaryButton
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier(AccessibilityID.onboardingContinue)
            }
            // The Welcome window's action buttons.
            .buttonBorderShape(.capsule)
            .controlSize(.extraLarge)
            .padding(20)
        }
    }

    @ViewBuilder private var primaryButton: some View {
        switch model.step {
        case .intro, .chooseAgents:
            Button("Continue") { model.continueFromChoose() }
                .disabled(!model.canContinueFromChoose)
        case .install:
            Button("Continue") { model.continueFromInstall() }
                .disabled(!model.installComplete)
        case .signIn:
            Button("Continue") { model.continueFromSignIn() }
                .disabled(!model.signInComplete)
        case .namedAgent:
            Button("Finish", action: finish)
                .disabled(!model.canFinish)
        }
    }
}

// MARK: Choose

private struct OnboardingChooseAgents: View {
    let model: OnboardingModel

    var body: some View {
        Section {
            ForEach(OnboardingModel.providers, id: \.self) { provider in
                let harnessID = provider.rawValue
                Toggle(isOn: Binding(
                    get: { model.isSelected(harnessID) },
                    set: { model.setSelected(harnessID, $0) }
                )) {
                    HStack(spacing: 10) {
                        AgentIconView(brand: model.brand(harnessID), name: provider.displayName, size: 28)
                        Text(provider.displayName)
                    }
                }
                .toggleStyle(.switch)
                .accessibilityLabel(provider.displayName)
                .accessibilityIdentifier("onboarding.agent.\(harnessID)")
                if model.isSelected(harnessID), let detected = model.detected(harnessID) {
                    Picker(selection: Binding(
                        get: { model.usesCLISetup(harnessID) },
                        set: { model.setUsesCLISetup(harnessID, $0) }
                    )) {
                        Text("Ottam profile").tag(false)
                        Text("My CLI setup").tag(true)
                    } label: {
                        Text("Profile")
                        Text(Self.detail(detected, usesCLISetup: model.usesCLISetup(harnessID)))
                    }
                    .pickerStyle(.menu)
                    // Under the agent's name, past its 28pt icon and spacing.
                    .padding(.leading, 38)
                    .accessibilityLabel("\(provider.displayName) profile")
                    .accessibilityIdentifier("onboarding.profile.\(harnessID)")
                }
            }
        } footer: {
            Text("Ottam profiles keep each agent separate from your CLI’s hooks, MCP servers, and skills.")
        }
    }

    /// How the chosen profile relates to the detected CLI setup, so the
    /// caption describes the selection rather than only the detection.
    static func detail(_ detected: AgentDetectedSetup, usesCLISetup: Bool) -> String {
        func count(_ value: UInt64?, _ noun: String) -> String? {
            guard let value else { return nil }
            return "\(value) \(noun)\(value == 1 ? "" : "s")"
        }
        let counts = [
            count(detected.skills, "skill"), count(detected.mcpServers, "MCP server"),
            count(detected.hooks, "hook"), count(detected.plugins, "plugin"),
        ].compactMap(\.self).joined(separator: ", ")
        let path = (detected.path as NSString).abbreviatingWithTildeInPath
        if usesCLISetup {
            return counts.isEmpty ? "Shares \(path)" : "Shares \(path): \(counts)"
        }
        return counts.isEmpty ? "Separate from \(path)" : "Separate from \(path) (\(counts))"
    }
}

// MARK: Install

private struct OnboardingInstall: View {
    let model: OnboardingModel

    var body: some View {
        Section {
            if model.showsNodeRow {
                OnboardingNodeRow(model: model)
            }
            ForEach(model.selected, id: \.self) { harnessID in
                OnboardingInstallRow(model: model, harnessID: harnessID)
            }
        }
    }
}

private struct OnboardingNodeRow: View {
    let model: OnboardingModel

    var body: some View {
        let install = model.runtime.runtimeInstall.state
        OnboardingStatusRow(name: "Node.js") {
            Image(systemName: "shippingbox")
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(width: 28, height: 28)
        } caption: {
            if let error = install.error, install.phase == .failed {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            } else {
                Text("Runs Codex and Claude Code").foregroundStyle(.secondary)
            }
        } status: {
            if install.isInFlight {
                if let total = install.bytesTotal, total > 0 {
                    ProgressView(value: Double(install.bytesReceived ?? 0), total: Double(total))
                        .frame(width: 140)
                } else {
                    ProgressView().controlSize(.small)
                }
            } else if !model.needsNode {
                Label("Ready", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            } else if install.phase == .failed || install.phase == .cancelled {
                Button("Retry") { model.retryNode() }
            } else {
                Text("Waiting").foregroundStyle(.secondary)
            }
        }
    }
}

private struct OnboardingInstallRow: View {
    let model: OnboardingModel
    let harnessID: String

    var body: some View {
        let agent = model.instance(harnessID)
        let install = model.runtime.agentInstall.state(for: AgentID(harnessID))
        let name = CLIProvider(harnessID: harnessID)?.displayName ?? harnessID
        OnboardingStatusRow(name: name) {
            AgentIconView(brand: model.brand(harnessID), name: name, size: 28)
        } caption: {
            // Always one line, so the row keeps its height as the install moves on.
            if install.phase == .failed, let error = install.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            } else if install.phase == .cancelled {
                Text("Cancelled").foregroundStyle(.secondary)
            } else if install.isInFlight {
                Text(install.phase == .verifying ? "Verifying…" : "Installing…").foregroundStyle(.secondary)
            } else {
                Text(model.usesCLISetup(harnessID) ? "Your CLI setup" : "Ottam profile").foregroundStyle(.secondary)
            }
        } status: {
            if let agent, OnboardingProgress.isInstalled(agent),
               !OnboardingProgress.needsLayout(agent, usesCLISetup: model.usesCLISetup(harnessID)) {
                Label("Installed", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            } else if install.isInFlight {
                ProgressView().controlSize(.small)
                Button("Cancel") { model.cancel(harnessID) }
            } else if install.phase == .failed || install.phase == .cancelled {
                Button("Retry") { model.retry(harnessID) }
                Button("Remove") { model.remove(harnessID) }
            } else if let agent, OnboardingProgress.usesNode(agent), EngineRuntimeProblem(agent: agent) != nil {
                Text("Waiting for Node.js").foregroundStyle(.secondary)
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .accessibilityIdentifier("onboarding.install.\(harnessID)")
    }
}

// MARK: Sign in

private struct OnboardingSignIn: View {
    let model: OnboardingModel

    var body: some View {
        Section {
            ForEach(model.chosenInstances, id: \.agentID) { agent in
                OnboardingSignInRow(model: model, agent: agent)
            }
        } footer: {
            Text("Some agents finish signing in in your browser; others open a login terminal here.")
        }
    }
}

private struct OnboardingSignInRow: View {
    let model: OnboardingModel
    let agent: AgentSummary

    var body: some View {
        let status = AgentSetupStatus(agent: agent)
        OnboardingStatusRow(name: agent.displayName) {
            AgentIconView(brand: agent.brand, name: agent.displayName, size: 28)
        } caption: {
            if let failure = model.signIn.failure(for: agent.agentID) {
                Text(failure).foregroundStyle(.red).textSelection(.enabled)
            } else if case .adapterProblem(let detail) = status {
                Text(detail ?? "Could not start the agent.").foregroundStyle(.orange)
            } else {
                Text(agent.homeKind == .existing ? "Your CLI setup · \(agent.accountLabel)" : agent.accountLabel)
                    .foregroundStyle(.secondary)
            }
        } status: {
            if OnboardingProgress.isSignedIn(agent) {
                Label("Signed in", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
            } else {
                switch status {
                case .running:
                    ProgressView().controlSize(.small)
                case .stopped where model.rechecking.contains(agent.agentID):
                    ProgressView().controlSize(.small)
                case .stopped, .runtimeProblem, .adapterProblem:
                    Button("Check Again") { model.recheck(agent.agentID) }
                default:
                    let isProviders = if case .providers = status { true } else { false }
                    Button(model.signIn.isSigningIn(agent.agentID) ? "Signing In…" : (isProviders ? "Set Up Providers" : "Sign In")) {
                        model.startSignIn(agent)
                    }
                    .disabled(!model.signIn.canStartOrResume(agent.agentID))
                }
            }
        }
        .accessibilityIdentifier("onboarding.signIn.\(agent.agentID.rawValue)")
    }
}

/// An Install or Sign In row: icon, name over a caption, and the status
/// trailing, all centred. `LabeledContent` aligns its value to the label's
/// first baseline, which sits it high beside an icon and a caption.
private struct OnboardingStatusRow<Icon: View, Caption: View, Status: View>: View {
    let name: String
    @ViewBuilder let icon: Icon
    @ViewBuilder let caption: Caption
    @ViewBuilder let status: Status

    var body: some View {
        HStack(spacing: 10) {
            icon
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                caption.font(.caption)
            }
            Spacer(minLength: 12)
            HStack(spacing: 8) { status }
        }
        .accessibilityElement(children: .contain)
    }
}

// MARK: Named agent

/// Suggested roles added in one click, above the same list and edit sheet
/// Settings › Agents uses.
private struct OnboardingNamedAgents: View {
    let model: OnboardingModel
    @Binding var editing: NamedAgentDraft?

    var body: some View {
        if !model.suggestions.isEmpty {
            Section("Suggested") {
                ForEach(model.suggestions) { preset in
                    HStack(spacing: 10) {
                        AgentAvatarView(avatar: preset.avatar, size: 28)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(preset.name)
                            Text(summary(preset))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button(model.addingPreset == preset.name ? "Adding…" : "Add") {
                            Task { await model.add(preset) }
                        }
                        .disabled(!model.canAdd(preset))
                        .accessibilityLabel("Add \(preset.name)")
                    }
                    .accessibilityIdentifier("onboarding.suggestion.\(preset.name)")
                }
            }
        }
        NamedAgentsSection(model: model.namedAgents, editing: $editing, defaultEngineID: model.defaultEngine?.engineID)
    }

    /// A writing role waits for the agent's modes, which its catalog brings.
    private func summary(_ preset: OnboardingRolePreset) -> String {
        guard let engine = model.defaultEngine, !OnboardingProgress.canAdd(preset, on: engine) else {
            return preset.summary
        }
        return "Available once \(engine.displayName) lists its modes."
    }
}
