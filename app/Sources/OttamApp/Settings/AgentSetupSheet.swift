import AppKit
import OttamBackendClient
import OttamPresentationLogic
import OttamWorkspaceLogic
import SwiftUI

/// The same native setup flow is used by the empty workspace and Settings.
struct AgentSetupSheet: View {
    let runtime: OttamAppRuntime
    let firstRun: Bool
    var onFinished: ((AgentID) -> Void)?
    @Environment(\.dismiss) private var dismiss
    @State private var model: AgentsSettingsModel
    @State private var signIn = AgentSignInController()
    @State private var harnessID: String
    @State private var layout = AgentHomeKind.managed
    @State private var selectedID: AgentID?
    @State private var errorText: String?
    @State private var isAdding = false

    init(runtime: OttamAppRuntime, harnessID: String = "codex", firstRun: Bool = false, onFinished: ((AgentID) -> Void)? = nil) {
        self.runtime = runtime
        self.firstRun = firstRun
        self.onFinished = onFinished
        _model = State(initialValue: AgentsSettingsModel(client: runtime.client))
        _harnessID = State(initialValue: harnessID)
    }

    private var detected: AgentDetectedSetup? {
        model.detectedSetups.first { $0.harnessID == harnessID }
    }

    private var providerName: String {
        detected?.displayName ?? CLIProvider(harnessID: harnessID)?.displayName ?? harnessID
    }
    /// Whether this provider's agent runs under Node; a prebuilt binary doesn't.
    private var usesNode: Bool {
        model.agents.first { ($0.harnessID ?? $0.agentID.rawValue) == harnessID }?.adapterPackage?.source != .binary
    }
    private var adapterID: AgentID { AgentID(harnessID) }
    private var selectedAgent: AgentSummary? { model.agents.first { $0.agentID == selectedID } }
    private var selectedInstallState: AgentAdapterInstallState? { selectedAgent?.adapterPackage?.installState }
    private var managedAgent: AgentSummary? {
        model.agents.first { ($0.harnessID ?? $0.agentID.rawValue) == harnessID && $0.homeKind == .managed }
    }
    private var canUseManagedProfile: Bool {
        managedAgent != nil || model.agents.contains {
            $0.agentID == adapterID && $0.canSetLayout == true
        }
    }
    private var needsRuntime: Bool {
        model.agents.contains { ($0.harnessID ?? $0.agentID.rawValue) == harnessID && EngineRuntimeProblem(agent: $0) != nil }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(firstRun && selectedID == nil ? "Set Up an Agent" : "Set Up \(providerName)").font(.title2.bold())
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(isAdding)
            }
            .padding()
            Divider()
            Form {
                if needsRuntime {
                    NodeRuntimeSection(nodeVersion: model.nodeVersion, source: model.runtimeSource,
                                       needsRuntime: true, update: nil, install: runtime.runtimeInstall)
                }
                if selectedID == nil {
                    profileChoice
                } else if let agent = selectedAgent {
                    setupResult(agent)
                }
                if let errorText = errorText ?? model.errorText {
                    Section { Text(errorText).foregroundStyle(.red).textSelection(.enabled) }
                }
            }
            .formStyle(.grouped)
            HStack {
                if !needsRuntime, usesNode, let nodeVersion = model.nodeVersion {
                    Label("Node.js \(nodeVersion) ready", systemImage: "checkmark.circle")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                if let selectedID {
                    Button(firstRun ? "Use in Chat" : "Done") { onFinished?(selectedID); dismiss() }
                        .buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                        .disabled(runtime.agentInstall.state(for: adapterID).isInFlight || needsRuntime || selectedInstallState == .notInstalled || selectedInstallState == .installing)
                } else {
                    Button(isAdding ? "Adding…" : "Continue") { add() }
                        .buttonStyle(.borderedProminent)
                        .disabled(isAdding || !model.didLoadOnce || needsRuntime || (layout == .existing && detected?.available != true) || (layout == .managed && !canUseManagedProfile))
                        .keyboardShortcut(.defaultAction)
                }
            }.padding()
        }
        .frame(width: 560, height: 520)
        .interactiveDismissDisabled(isAdding)
        .onChange(of: harnessID) { _, _ in chooseDefault() }
        .onChange(of: runtime.runtimeInstall.state.phase) { _, phase in
            if phase == .ready { model.requestRuntimeRefresh() }
        }
        .task {
            await runtime.runtimeInstall.start()
            await runtime.agentInstall.start()
            await signIn.configure(workspaceID: nil, backend: runtime.client)
            signIn.onSignedIn = { _ in model.requestReload() }
            await model.load()
            // First run defaults to the agent this Mac already has an
            // install of, rather than the picker's first entry.
            if firstRun, let installed = model.agents.first(where: { $0.adapterPackage?.installedVersion != nil }),
               let harness = installed.harnessID, CLIProvider(harnessID: harness) != nil {
                harnessID = harness
            }
            chooseDefault()
            do { try await model.detectSetups() }
            catch { errorText = error.localizedDescription }
        }
        .onDisappear { signIn.detach() }
        .task(id: model.refreshGeneration) {
            if model.didLoadOnce { await model.load() }
        }
        .task { await model.observeEngineChanges() }
        .task(id: runtime.agentInstall.state(for: adapterID).phase) {
            if runtime.agentInstall.state(for: adapterID).isInFlight { await model.monitorInstallRefresh() }
            else if selectedID != nil { model.requestActivationRefresh() }
        }
        .sheet(isPresented: Binding(get: { model.installPreparation.waiting }, set: { if !$0 { runtime.agentInstall.cancel(agentID: adapterID) } })) {
            AgentShutdownWaitingView { runtime.agentInstall.cancel(agentID: adapterID) }
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in model.requestActivationRefresh() }
        .sheet(item: Binding(get: { signIn.revealed }, set: { if $0 == nil { signIn.dismissRevealedTerminal() } })) { revealed in
            AgentSignInTerminalSheet(revealed: revealed, signIn: signIn)
        }
    }

    private func chooseDefault() { layout = model.didLoadOnce && !canUseManagedProfile ? .existing : .managed }
    private func count(_ value: UInt64?) -> String { value.map(String.init) ?? "Unknown" }

    @ViewBuilder private var profileChoice: some View {
        Section {
            if firstRun {
                Picker("Agent", selection: $harnessID) {
                    ForEach(CLIProvider.allCases, id: \.self) { provider in
                        Text(provider.displayName).tag(provider.rawValue)
                    }
                }
                .disabled(isAdding)
            }
            Picker("Profile", selection: $layout) {
                profileOption(
                    title: managedAgent == nil ? "Ottam profile" : "Use existing Ottam profile", recommended: true,
                    explanation: managedAgent == nil
                        ? "Start with a clean profile, separate from your CLI’s hooks, MCP servers, and skills. Sign in separately; your CLI setup stays unchanged."
                        : "Continue with your Ottam profile and its sign-in. This reuses the profile; it doesn’t create another one. Your CLI setup stays unchanged."
                ).tag(AgentHomeKind.managed).disabled(!canUseManagedProfile)
                profileOption(
                    title: "Use my CLI setup",
                    explanation: "Reuse your CLI’s sign-in, hooks, MCP servers, and skills. Custom configuration may affect how the agent works in Ottam."
                ).tag(AgentHomeKind.existing)
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()
            .disabled(isAdding)
            if model.didLoadOnce && !canUseManagedProfile {
                Text("Your CLI instance already has conversations and must keep its home. Use Create another profile… on the provider page for a new Ottam profile.")
                    .font(.callout).foregroundStyle(.secondary)
            }
        } header: {
            Text("Choose a profile")
        }
        if layout == .existing {
            Section("Your CLI setup") {
                if let detected {
                    LabeledContent("CLI home", value: detected.path).textSelection(.enabled)
                    if let detail = detected.detail {
                        if detected.available {
                            Label(detail, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                        } else {
                            Text(detail).foregroundStyle(.secondary)
                        }
                    }
                    if detected.available {
                        DisclosureGroup("Detected configuration") {
                            if let version = detected.cliVersion { LabeledContent("Installed CLI", value: version) }
                            LabeledContent("Skills", value: count(detected.skills))
                            LabeledContent("MCP servers", value: count(detected.mcpServers))
                            LabeledContent("Hooks", value: count(detected.hooks))
                            LabeledContent("Plugins", value: count(detected.plugins))
                        }
                    }
                } else {
                    ProgressView("Detecting your CLI setup…")
                }
            }
        }
    }

    private func profileOption(title: String, recommended: Bool = false, explanation: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(title).font(.headline)
                if recommended {
                    Text("Recommended").font(.caption.weight(.medium))
                        .foregroundStyle(.secondary)
                }
            }
            Text(explanation).font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(recommended ? "\(title), Recommended" : title)
        .accessibilityHint(explanation)
    }

    @ViewBuilder private func setupResult(_ agent: AgentSummary) -> some View {
        let presentation = AgentSettingsPresentation(agent: agent)
        let installation = runtime.agentInstall.state(for: adapterID)
        Section {
            Label(agent.label ?? providerName, systemImage: "person.crop.rectangle")
                .font(.headline)
            Text(agent.homeKind == .existing ? "Your CLI setup is connected." : "Your Ottam profile is selected.")
                .foregroundStyle(.secondary)
        }
        if presentation.installState == .notInstalled || installation.isInFlight || installation.phase == .failed || installation.phase == .cancelled {
            Section("Connect to Ottam") {
                Text("Install Ottam’s support for \(providerName) to use this profile here.")
                let installPresentation = AgentAdapterInstallPresentation(installState: presentation.installState, install: installation)
                if let title = installPresentation.actionTitle {
                    Button(title == "Get" ? "Install Support" : title) { runtime.agentInstall.install(agentID: adapterID) }
                        .disabled(!installPresentation.isActionEnabled)
                } else if let status = installPresentation.statusText {
                    Label(status, systemImage: "checkmark.circle")
                }
                if installation.isInFlight {
                    ProgressView("Installing…")
                    Button("Cancel Installation") { runtime.agentInstall.cancel(agentID: adapterID) }
                }
                if let detail = installPresentation.detail { Text(detail).foregroundStyle(.orange) }
            }
        }
        if presentation.installState == .installed {
            Section("Sign-in") {
                if presentation.status == .ready {
                    Label("\(providerName) is ready", systemImage: "checkmark.circle.fill")
                } else if presentation.wantsSignIn {
                    Button("Sign In to \(providerName)") { signIn.signIn(agentID: agent.agentID, displayName: agent.displayName) }
                        .disabled(!signIn.canStartOrResume(agent.agentID))
                    if let failure = signIn.failure(for: agent.agentID) { Text(failure).foregroundStyle(.red) }
                } else {
                    Text(presentation.status == .running ? "Checking sign-in…" : "Sign-in will be checked when you start a conversation.")
                        .foregroundStyle(.secondary)
                    if case .adapterProblem(let detail) = presentation.status { Text(detail ?? "Could not start the agent.").foregroundStyle(.orange) }
                }
                Button("Check Again") { model.requestActivationRefresh() }
            }
        }
    }

    private func add() {
        guard !isAdding else { return }
        isAdding = true
        Task {
            defer { isAdding = false }
            do {
                if layout == .existing {
                    if firstRun {
                        do { selectedID = try await model.useLayout(agentID: AgentID(harnessID), layout: layout) }
                        catch {
                            let message = error.localizedDescription
                            guard message.contains("conversations") || message.contains("already added") else { throw error }
                            selectedID = try await model.addExistingSetup(harnessID: harnessID)
                        }
                    } else { selectedID = try await model.addExistingSetup(harnessID: harnessID) }
                }
                else if let managed = managedAgent {
                    selectedID = managed.agentID
                } else {
                    // Never silently select an existing home after the user
                    // chose a separate profile. An unclaimed first-run instance
                    // may switch back; claimed conversations keep their home.
                    selectedID = try await model.useLayout(agentID: AgentID(harnessID), layout: .managed)
                }
                errorText = nil
            } catch { errorText = error.localizedDescription }
        }
    }
}
