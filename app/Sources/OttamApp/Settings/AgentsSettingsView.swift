import AppKit
import OttamBackendClient
import OttamPresentationLogic
import OttamWorkspaceLogic
import SwiftUI

/// Settings lists providers, then their instances, then each instance's
/// maintenance page. Sign-in and home ownership belong to the instance;
/// adapter installation belongs to its provider.
struct AgentsSettingsSection: View {
    let runtime: OttamAppRuntime
    /// Owned by `SettingsView`, whose sidebar search matches the listed
    /// agents' names; loading stays this pane's job.
    let model: AgentsSettingsModel
    @State private var signIn = AgentSignInController()
    @State private var setupHarness: String?
    @State private var creatingHarness: String?
    @State private var navigationPath = NavigationPath()
    @State private var namedAgents: NamedAgentsSettingsModel?
    @State private var editingNamedAgent: NamedAgentDraft?

    var body: some View {
        NavigationStack(path: $navigationPath) {
            agentList
                .navigationTitle("Agents")
                .navigationDestination(for: String.self) { harnessID in
                    instanceList(harnessID: harnessID)
                }
                .navigationDestination(for: AgentID.self) { agentID in
                    AgentSettingsDetail(
                        model: model,
                        install: runtime.runtimeInstall,
                        adapterInstall: runtime.agentInstall,
                        agentID: agentID,
                        signIn: signIn
                    )
                }
        }
        .task {
            await model.observeEngineChanges()
        }
        // Models and modes come from a workspace's engine list; any open one
        // reports the same agents.
        .task {
            let named = namedAgents ?? NamedAgentsSettingsModel(client: runtime.client)
            namedAgents = named
            await named.load(workspaceID: runtime.activeWorkspaces.first?.workspace.workspaceID)
        }
        .sheet(item: $editingNamedAgent) { draft in
            if let namedAgents {
                NamedAgentSheet(model: namedAgents, draft: draft)
            }
        }
        // No workspace: core runs the login in the agent's isolated home.
        .task { [weak model] in
            await signIn.configure(workspaceID: nil, backend: runtime.client)
            signIn.onSignedIn = { [weak model] _ in
                model?.requestReload()
            }
        }
        .onDisappear { signIn.detach() }
        // A finished install changes every agent's runtime status, and core has
        // already dropped its caches by then — a plain reload is enough.
        .task { [weak model] in
            await runtime.runtimeInstall.start()
            runtime.runtimeInstall.onInstalled = { [weak model] in
                model?.requestReload()
            }
            await runtime.agentInstall.start()
            runtime.agentInstall.onInstalled = { [weak model] _ in
                model?.requestReload()
            }
        }
        // Keyed on the generation so Check Again cancels an in-flight load
        // rather than racing it. Attached here, not to SettingsView — an open
        // Settings window sitting on another pane must not fire Node probes
        // (its sidebar search loads once, and only when someone types).
        .task(id: model.refreshGeneration) {
            await model.load()
        }
        // Returning to the app after `brew install node` or a browser login
        // should refresh without a click. A plain reload: the resolver's
        // unresolved cache expires on its own, and Check Again remains the
        // explicit hard invalidation.
        .onReceive(
            NotificationCenter.default.publisher(
                for: NSApplication.didBecomeActiveNotification
            )
        ) { _ in
            model.requestActivationRefresh()
        }
    }

    private var agentList: some View {
        Form {
            // Above the agents, because Node is app-global: every adapter runs
            // on the same toolchain, so a per-agent-only presentation would
            // suggest three separate things to fix.
            if model.didLoadOnce && (model.hasRuntimeProblem || model.managedRuntimeUpdate != nil) {
                NodeRuntimeSection(
                    nodeVersion: model.nodeVersion,
                    source: model.runtimeSource,
                    needsRuntime: model.hasRuntimeProblem,
                    update: model.managedRuntimeUpdate,
                    install: runtime.runtimeInstall
                )
            }

            Section {
                if let errorText = model.errorText {
                    Label {
                        Text("Could not read agent status")
                        Text(errorText)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                    }
                } else if !model.didLoadOnce {
                    HStack {
                        Text("Checking agents…")
                        Spacer()
                        ProgressView()
                            .controlSize(.small)
                    }
                } else {
                    ForEach(harnessIDs, id: \.self) { harnessID in
                        let provider = instances(harnessID).first
                        NavigationLink(value: harnessID) {
                            HStack(spacing: 12) {
                                AgentIconView(brand: provider?.brand, name: providerName(harnessID), size: 36)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(providerName(harnessID))
                                    Text(provider?.vendorName ?? "")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                let count = instances(harnessID).filter(AgentSettingsPresentation.hasAdapter).count
                                Text(count == 0 ? "Set Up" : String(count))
                                    .monospacedDigit().foregroundStyle(.secondary)
                                    .accessibilityLabel(count == 0 ? "Set Up" : "\(count) instances")
                            }
                        }
                    }
                }
            } header: {
                HStack {
                    Text("Agents")
                    Spacer()
                    if model.isLoading {
                        ProgressView()
                            .controlSize(.small)
                    }
                    Button("Check Again") {
                        model.requestRuntimeRefresh()
                    }
                    .disabled(model.isLoading)
                }
            }

            if let namedAgents {
                NamedAgentsSection(model: namedAgents, editing: $editingNamedAgent)
            }
        }
        .formStyle(.grouped)
    }

    private var harnessIDs: [String] {
        Array(Set(CLIProvider.allCases.map(\.rawValue) + model.agents.map { $0.harnessID ?? $0.agentID.rawValue })).sorted()
    }

    private func providerName(_ harnessID: String) -> String {
        instances(harnessID).first?.displayName ?? CLIProvider(harnessID: harnessID)?.displayName ?? harnessID
    }

    private func instances(_ harnessID: String) -> [AgentSummary] {
        model.agents.filter { ($0.harnessID ?? $0.agentID.rawValue) == harnessID }
    }

    private func instanceList(harnessID: String) -> some View {
        Form {
            Section {
                ForEach(instances(harnessID), id: \.agentID) { agent in
                    AgentsSettingsListRow(agent: agent)
                }
                if instances(harnessID).isEmpty {
                    Text("No \(providerName(harnessID)) instances set up yet.").foregroundStyle(.secondary)
                }
            } header: {
                Text("Instances")
            } footer: {
                Text("Each instance keeps its own home and conversations. Choose an instance to manage its sign-in and settings.")
            }
            Section {
                Button("Set Up \(providerName(harnessID))…") { setupHarness = harnessID }
                Button("Create another profile…") { creatingHarness = harnessID }
                    .accessibilityIdentifier("agent.profile.create")
            }
        }
        .formStyle(.grouped)
        .navigationTitle(providerName(harnessID))
        .sheet(isPresented: Binding(get: { creatingHarness != nil }, set: { if !$0 { creatingHarness = nil } })) {
            AgentProfileLabelSheet(title: "Create \(providerName(harnessID)) Profile", actionTitle: "Create", initialLabel: "", existingLabels: instances(harnessID).compactMap(\.label)) { label in
                let id = try await model.createProfile(harnessID: harnessID, label: label)
                navigationPath.append(id)
            }
        }
        .sheet(isPresented: Binding(get: { setupHarness != nil }, set: { if !$0 { setupHarness = nil; model.requestReload() } })) {
            AgentSetupSheet(runtime: runtime, harnessID: harnessID) { agentID in
                model.requestReload()
                navigationPath.append(agentID)
            }
        }
    }
}

/// The app-global Node runtime: what is being used, where it came from, and
/// the one action that fixes it when nothing usable is installed.
struct NodeRuntimeSection: View {
    let nodeVersion: String?
    let source: AgentRuntimeSource?
    let needsRuntime: Bool
    /// An app update replacing the managed runtime; the agents keep running
    /// on `nodeVersion` meanwhile, so this is shown as an update, not a fix.
    var update: AgentUpdateStatus?
    let install: RuntimeInstallController

    /// Offer the install when there is a problem to solve, and keep offering
    /// it while one is running or has just finished — a button that vanishes
    /// the moment it works leaves the user unsure whether it did.
    private var showsAction: Bool {
        needsRuntime || install.state.phase != nil
    }

    var body: some View {
        Section {
            LabeledContent("Node.js") {
                if let nodeVersion {
                    Text(nodeVersion)
                } else {
                    Text("Not found")
                        .foregroundStyle(.secondary)
                }
            }
            if let sourceName {
                LabeledContent("Provided by") {
                    Text(sourceName)
                }
            }
            if let update, !needsRuntime {
                let presentation = ManagedRuntimeUpdatePresentation(update: update, install: install.state)
                LabeledContent("Update") {
                    HStack(spacing: 10) {
                        if install.state.isInFlight || update.state == .updating {
                            if let fraction = install.state.fractionCompleted {
                                ProgressView(value: fraction)
                                    .controlSize(.small)
                                    .frame(width: 80)
                            } else {
                                ProgressView()
                                    .controlSize(.small)
                            }
                        }
                        if let statusText = presentation.statusText {
                            Text(statusText)
                                .foregroundStyle(.secondary)
                        }
                        if let actionTitle = presentation.actionTitle {
                            Button(actionTitle) { install.install() }
                                .disabled(!presentation.isActionEnabled)
                        }
                        if install.state.isInFlight {
                            Button("Cancel") { install.cancel() }
                        }
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("settings.agents.runtimeUpdate")
                if let detail = presentation.detail {
                    Text(detail)
                        .font(.callout)
                        .foregroundStyle(update.state == .failed || install.state.phase == .failed
                                         ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                }
            } else if showsAction {
                HStack {
                    Button(install.state.actionTitle) {
                        install.install()
                    }
                    .disabled(install.state.isInFlight || install.state.phase == .ready)
                    if let fraction = install.state.fractionCompleted {
                        ProgressView(value: fraction)
                            .controlSize(.small)
                    } else if install.state.isInFlight {
                        ProgressView()
                            .controlSize(.small)
                    }
                    Spacer()
                    if install.state.isInFlight {
                        Button("Cancel") { install.cancel() }
                    }
                }
            }
        } header: {
            Text("Node Runtime")
        } footer: {
            if update != nil, !needsRuntime {
                EmptyView()
            } else if let detail = install.state.detail {
                Text(detail)
                    .foregroundStyle(install.state.phase == .failed ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
            } else if needsRuntime {
                Text(
                    "Agents run as npm packages. Ottam can install its own copy of Node.js, "
                        + "which leaves anything already on your Mac alone."
                )
            }
        }
    }

    private var sourceName: String? {
        guard nodeVersion != nil else { return nil }
        return switch source {
        case .managed: "Ottam"
        case .system: "Your Mac"
        case .override: "OTTAM_NODE"
        case nil: nil
        }
    }
}

/// One list row: tile, name over vendor, status, chevron. Details are the
/// detail page's job.
private struct AgentsSettingsListRow: View {
    let agent: AgentSummary

    private var presentation: AgentSettingsPresentation {
        AgentSettingsPresentation(agent: agent)
    }

    var body: some View {
        NavigationLink(value: agent.agentID) {
            HStack(spacing: 10) {
                AgentIconView(brand: agent.brand, name: agent.displayName, size: 36)

                VStack(alignment: .leading, spacing: 1) {
                    let label = agent.label ?? agent.displayName
                    Text(label == agent.displayName ? (agent.homeKind == .existing ? "Your CLI setup" : "Ottam profile") : label)
                    Text(agent.homeKind == .existing ? "Shares sign-in and sessions with your CLI" : "Separate sign-in and sessions")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Spacer()

                // No manual chevron: the grouped Form draws its own
                // disclosure indicator for a NavigationLink row.
                // A stopped adapter has no label: cold is the normal state,
                // and a status chip there would read as something to fix.
                if let label = presentation.status.label,
                   let symbol = presentation.status.symbolName
                {
                    Label(label, systemImage: symbol)
                        .foregroundStyle(presentation.status.trailingStyle)
                        .labelStyle(.titleAndIcon)
                        .font(.callout)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// The drill-in page for one agent: identity header, then adapter, runtime,
/// and account sections — Xcode's Intelligence-agent layout. Looks the agent
/// up by ID on each render so Check Again refreshes the open page in place.
struct AgentSettingsDetail: View {
    let model: AgentsSettingsModel
    let install: RuntimeInstallController
    let adapterInstall: AgentInstallController
    let agentID: AgentID
    let signIn: AgentSignInController

    private var agent: AgentSummary? {
        model.agents.first { $0.agentID == agentID }
    }

    private var adapterID: AgentID { AgentID(agent?.harnessID ?? agentID.rawValue) }

    var body: some View {
        Form {
            if let agent {
                let presentation = AgentSettingsPresentation(agent: agent)

                Section {
                    HStack(spacing: 12) {
                        AgentIconView(brand: agent.brand, name: agent.displayName, size: 40)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(agent.label ?? agent.displayName)
                            Button("Rename…") { isRenaming = true }
                                .buttonStyle(.link)
                            Button("Recheck Profile") { Task { await model.probeProfile(agentID: agentID) } }
                                .buttonStyle(.link)
                            Text(agent.homeKind == .existing ? "Your CLI setup" : "Ottam profile")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                let adapterPresentation = AgentAdapterInstallPresentation(
                    installState: presentation.installState,
                    install: adapterInstall.state(for: adapterID),
                    installedVersion: agent.adapterPackage?.installedVersion,
                    update: agent.adapterPackage?.update
                )
                Section {
                    switch presentation.package {
                    case .npm(let name, let version), .binary(let name, let version):
                        LabeledContent("Package") {
                            Text(name)
                                .textSelection(.enabled)
                        }
                        LabeledContent("Version") {
                            // While an app update's pin installs, the older
                            // version is what runs; say both.
                            if let update = agent.adapterPackage?.update,
                               let installed = agent.adapterPackage?.installedVersion {
                                Text("\(installed) → \(update.version)")
                            } else {
                                Text(version)
                            }
                        }
                        if case .binary = presentation.package {
                            LabeledContent("Source", value: "Prebuilt binary")
                        }
                        // The whole install lifecycle lives in one slot:
                        // Get → Installing… → Verifying… → Installed.
                        if adapterPresentation.actionTitle != nil || adapterPresentation.statusText != nil {
                            let update = agent.adapterPackage?.update
                            LabeledContent(update == nil ? "Install" : "Update") {
                                HStack(spacing: 10) {
                                    if adapterInstall.state(for: adapterID).isInFlight {
                                        ProgressView()
                                            .controlSize(.small)
                                        Button("Cancel") {
                                            adapterInstall.cancel(agentID: adapterID)
                                        }
                                    } else if update?.state == .updating {
                                        // Core's background update, started
                                        // before this window was watching.
                                        ProgressView()
                                            .controlSize(.small)
                                    }
                                    if let actionTitle = adapterPresentation.actionTitle {
                                        Button(actionTitle) {
                                            adapterInstall.install(agentID: adapterID)
                                        }
                                        .disabled(!adapterPresentation.isActionEnabled)
                                    } else if let statusText = adapterPresentation.statusText {
                                        Text(statusText)
                                            .foregroundStyle(.secondary)
                                        // The same `agent.install` as Get: on an
                                        // installed agent core replaces the tree
                                        // and re-verifies the handshake, which
                                        // also refreshes the model catalog.
                                        Button("Reinstall") {
                                            adapterInstall.install(agentID: adapterID)
                                        }
                                        .disabled(isRemoving)
                                    }
                                }
                                .accessibilityElement(children: .contain)
                            }
                        }
                    case .builtIn:
                        LabeledContent("Package") {
                            Text("Built in")
                        }
                    }
                    if agent.homeKind == .existing,
                       let detected = model.detectedSetups.first(where: { $0.harnessID == agent.harnessID }),
                       let version = detected.cliVersion {
                        LabeledContent("Terminal CLI", value: version)
                        if let adapterVersion = agent.adapter.agentVersion {
                            LabeledContent("Ottam agent", value: adapterVersion)
                        }
                        Text("Ottam uses a pinned adapter. A different terminal CLI version does not prevent using your setup.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    // One line of the ACP matrix; the sheet has the rows.
                    let compatibility = AgentCompatibilityPresentation(
                        compatibility: agent.adapter.compatibility
                    )
                    LabeledContent("Compatibility") {
                        HStack(spacing: 10) {
                            Text(compatibility.summary)
                                .foregroundStyle(.secondary)
                            Button("Details…") {
                                showingCompatibility = true
                            }
                            .disabled(compatibility.sections.isEmpty)
                        }
                        .accessibilityElement(children: .contain)
                    }
                } header: {
                    Text("Adapter")
                } footer: {
                    if let detail = adapterPresentation.detail {
                        Text(detail)
                            .foregroundStyle(.orange)
                    }
                }

                // A prebuilt binary runs without Node, so there is no
                // runtime to report or fix.
                if presentation.package.needsNode {
                    Section {
                        LabeledContent("Node.js") {
                            // The fix is the same app-global install the Node
                            // Runtime section offers, so this is the same button
                            // rather than a second route to a browser.
                            if presentation.runtimeProblem != nil {
                                Button(install.state.actionTitle) {
                                    install.install()
                                }
                                .disabled(install.state.isInFlight || install.state.phase == .ready)
                            } else if let nodeVersion = presentation.nodeVersion {
                                Text(nodeVersion)
                            }
                        }
                    } header: {
                        Text("Runtime")
                    } footer: {
                        if let problem = presentation.runtimeProblem {
                            Text(problem.explanation)
                        }
                    }
                }

                Section {
                    // Titled with what the sign-in connects, so the row reads
                    // "ChatGPT account … Signed in" without a vendor switch here.
                    LabeledContent(presentation.accountLabel) {
                        HStack(spacing: 10) {
                            if let methodID = presentation.providerSetupMethodID {
                                Button("Set Up Providers…") {
                                    signIn.signIn(
                                        agentID: agentID,
                                        displayName: agent.displayName,
                                        methodID: methodID
                                    )
                                }
                                .disabled(!signIn.canStartOrResume(agentID))
                            } else if presentation.wantsSignIn {
                                Button("Sign In") {
                                    signIn.signIn(
                                        agentID: agentID,
                                        displayName: agent.displayName
                                    )
                                }
                                .disabled(!signIn.canStartOrResume(agentID))
                            }
                            if let label = presentation.status.label,
                               let symbol = presentation.status.symbolName
                            {
                                Label(label, systemImage: symbol)
                                    .foregroundStyle(presentation.status.trailingStyle)
                                    .labelStyle(.titleAndIcon)
                            } else {
                                Text("Not running")
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                } header: {
                    Text("Account")
                } footer: {
                    if agent.homeKind == .existing {
                        Text("This is your CLI’s account. Signing in here signs in the CLI in Terminal too.")
                    } else if let footnote = presentation.accountFootnote {
                        Text(footnote)
                    }
                }

                // No entry means the agent has no Ottam-managed home
                // (the request errors for it), so there is nothing to show.
                if let home = model.homeInfo[agent.agentID] {
                    Section {
                        LabeledContent("Location") {
                            Text(abbreviatedPath(home.path))
                                .textSelection(.enabled)
                                .multilineTextAlignment(.trailing)
                        }
                        LabeledContent("Size") {
                            Text(
                                Int64(clamping: home.sizeBytes),
                                format: .byteCount(style: .file)
                            )
                        }
                        Button("Reveal in Finder") {
                            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: home.path)])
                        }
                    } header: {
                        Text("Home")
                    } footer: {
                        Text(agent.homeKind == .existing ? "This home belongs to your CLI. Ottam does not delete or clean it." : presentation.homeFootnote)
                    }
                }

                // Settings-app placement for the destructive action: its own
                // section at the foot of the page, never beside the controls
                // that keep the agent working. The HIG asks for more than
                // standard spacing between a destructive control and the safe
                // ones; a separate group at the end is how a Form spells that.
                if presentation.installState == .installed {
                    Section {
                        // Borderless, not bordered: this is the one control on
                        // the page that takes something away, and the pushed
                        // chrome of Sign In or Reinstall would file it with
                        // them. Settings gives a standalone destructive action
                        // plain red text instead.
                        //
                        // The red has to be asked for. On macOS
                        // `role: .destructive` paints nothing outside alerts,
                        // dialogs and context menus — it carries ordering and
                        // semantics, which is why the role stays. `.borderless`
                        // draws its label in the tint, so that is the lever.
                        //
                        // Colour is never the only signal: the verb and the
                        // ellipsis (a confirmation follows) carry it too, for
                        // anyone who cannot separate red from grey.
                        Button("Remove \(agent.displayName)…", role: .destructive) {
                            isConfirmingRemoval = true
                        }
                        .buttonStyle(.borderless)
                        .tint(.red)
                        .disabled(isRemoving || adapterInstall.state(for: adapterID).isInFlight)
                    }
                }
                Section {
                    Button("Delete Profile…", role: .destructive) { isDeleting = true }
                        .buttonStyle(.borderless).tint(.red)
                        .disabled(isRemoving || adapterInstall.state(for: adapterID).isInFlight)
                    if isDeletingProfile {
                        ProgressView("Deleting profile…")
                            .accessibilityIdentifier("agent.profile.deleting")
                        Text("Waiting for profile activity to finish safely. You can cancel before the profile is removed.")
                            .font(.callout).foregroundStyle(.secondary)
                        Button("Cancel Deletion", role: .cancel) { removalTask?.cancel() }
                            .accessibilityIdentifier("agent.profile.cancelDeletion")
                    }
                }
            } else {
                Text("This agent was removed").foregroundStyle(.secondary)
            }
        }
        // Recomputed on each visit; the cached answer shows meanwhile.
        .task(id: agentID) {
            model.activeProfileID = agentID
            await model.loadHomeInfo(agentID: agentID)
            await model.probeProfile(agentID: agentID)
            if agent?.homeKind == .existing { try? await model.detectSetups() }
        }
        .onDisappear { if model.activeProfileID == agentID { model.activeProfileID = nil } }
        .onChange(of: agent?.adapterPackage?.installState) { _, state in
            if state == .installed { Task { await model.probeProfile(agentID: agentID) } }
        }
        .formStyle(.grouped)
        .navigationTitle(agent?.label ?? agent?.displayName ?? "Agent")
        // The same sheet chat uses. A login that needs to be seen — one that
        // reveals from the first frame, or any login that goes wrong — is put
        // on screen here rather than sending the user to find a workspace.
        // Presented from this page, where Sign In lives: a sheet on the
        // enclosing NavigationStack never appears while a page is pushed.
        .sheet(item: Binding(get: { signIn.revealed }, set: { revealed in
            if revealed == nil { signIn.dismissRevealedTerminal() }
        })) { revealed in
            AgentSignInTerminalSheet(revealed: revealed, signIn: signIn)
        }
        .sheet(isPresented: $isRenaming) {
            AgentProfileLabelSheet(title: "Rename Profile", actionTitle: "Rename", initialLabel: agent?.label ?? "", existingLabels: model.agents.filter { $0.harnessID == agent?.harnessID && $0.agentID != agentID }.compactMap(\.label)) { label in
                try await model.renameProfile(agentID: agentID, label: label)
            }
        }
        .sheet(isPresented: $isDeleting) {
            AgentProfileDeleteSheet(label: agent?.label ?? "this profile", managed: agent?.homeKind == .managed) { purge in
                isRemoving = true
                isDeletingProfile = true
                removalTask = Task {
                    defer { isRemoving = false; isDeletingProfile = false; removalTask = nil }
                    do { try await model.deleteProfile(agentID: agentID, purge: purge) }
                    catch { if !Task.isCancelled { removalError = error.localizedDescription } }
                }
            }
        }
        .task(id: adapterInstall.state(for: adapterID).isInFlight) {
            if adapterID.rawValue == "claude", adapterInstall.state(for: adapterID).isInFlight {
                await model.monitorInstallRefresh()
            }
        }
        .sheet(isPresented: $showingCompatibility) {
            AgentCompatibilitySheet(
                title: "\(agent?.displayName ?? "Agent") Compatibility",
                presentation: AgentCompatibilityPresentation(
                    compatibility: agent?.adapter.compatibility ?? []
                )
            )
        }
        .confirmationDialog(
            "Remove \(agent?.displayName ?? "this agent")?",
            isPresented: $isConfirmingRemoval,
            titleVisibility: .visible
        ) {
            // Only the second option is marked destructive, and the split is
            // the point: the adapter is one click from coming back, while the
            // home takes sign-in and sessions with it for good. Marking both
            // red would flatten the difference the user is here to weigh.
            Button("Remove Adapter") { remove(removeHome: false) }
            if agent?.homeKind != .existing {
                Button("Remove Adapter and Home", role: .destructive) { remove(removeHome: true) }
            }
            // Explicit rather than left to the automatic one, so the safe way
            // out is a stated part of the dialog.
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(removalMessage)
        }
        .alert(
            "Couldn't Remove \(agent?.displayName ?? "Agent")",
            isPresented: Binding(
                get: { removalError != nil },
                set: { if !$0 { removalError = nil } }
            )
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(removalError ?? "")
        }
        .sheet(isPresented: Binding(get: { model.removalPreparation.waiting || model.installPreparation.waiting }, set: { if !$0 { cancelWaitingOperation() } })) {
            AgentShutdownWaitingView { cancelWaitingOperation() }
        }
    }

    @State private var isConfirmingRemoval = false
    @State private var isRenaming = false
    @State private var isDeleting = false
    @State private var showingCompatibility = false
    @State private var isRemoving = false
    @State private var isDeletingProfile = false
    @State private var removalError: String?
    @State private var removalTask: Task<Void, Never>?

    private func cancelWaitingOperation() {
        if model.installPreparation.waiting { adapterInstall.cancel(agentID: adapterID) }
        else { removalTask?.cancel() }
    }

    /// Spells out what each choice deletes; the home's size is the one number
    /// worth knowing before agreeing to lose it.
    private var removalMessage: String {
        if agent?.homeKind == .existing {
            return "This removes Ottam’s adapter for every \(agent?.displayName ?? "agent") setup. Your CLI homes and conversations are preserved. You can reinstall the adapter later."
        }
        var text = "Removing the adapter uninstalls the package Ottam runs this agent with; "
            + "you can get it again any time. Removing the home as well deletes the agent's "
            + "sessions and sign-in inside Ottam"
        if let home = model.homeInfo[agentID] {
            let size = ByteCountFormatter.string(
                fromByteCount: Int64(clamping: home.sizeBytes),
                countStyle: .file
            )
            text += " (\(size))"
        }
        return text + ". Your terminal CLI's own setup is not affected."
    }

    private func remove(removeHome: Bool) {
        isRemoving = true
        removalTask = Task {
            defer { isRemoving = false; removalTask = nil }
            do {
                try await model.remove(agentID: agentID, removeHome: removeHome)
                // A stale "Installed" from an earlier install flow would
                // otherwise outrank the list's `notInstalled`.
                adapterInstall.forgetOutcome(agentID: adapterID)
            } catch is CancellationError {
                return
            } catch {
                if !Task.isCancelled { removalError = error.localizedDescription }
            }
        }
    }

    /// `~`-abbreviate for display; a copied `~` path still expands in a shell.
    private func abbreviatedPath(_ path: String) -> String {
        (path as NSString).abbreviatingWithTildeInPath
    }
}

extension AgentSetupStatus {
    fileprivate var trailingStyle: AnyShapeStyle {
        switch self {
        case .runtimeProblem, .adapterProblem:
            AnyShapeStyle(.orange)
        case .stopped, .signedOut, .expired, .providers([]):
            AnyShapeStyle(.secondary)
        case .running, .ready, .providers:
            AnyShapeStyle(.green)
        }
    }
}
