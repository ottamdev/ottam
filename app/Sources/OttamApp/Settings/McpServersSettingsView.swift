import OttamBackendClient
import OttamPresentationLogic
import SwiftUI
import UniformTypeIdentifiers

/// Settings lists the configured MCP servers with their state, then each
/// server's page: what it runs, where it is on, and Edit and Remove.
struct McpServersSettingsSection: View {
    let runtime: OttamAppRuntime
    @State private var model: McpServersSettingsModel
    @State private var adding = false
    @State private var importing: McpImportRequest?
    @State private var choosingFile = false
    @State private var navigationPath = NavigationPath()

    init(runtime: OttamAppRuntime) {
        self.runtime = runtime
        _model = State(initialValue: McpServersSettingsModel(client: runtime.client))
    }

    var body: some View {
        NavigationStack(path: $navigationPath) {
            serverList
                .navigationTitle("MCP Servers")
                .navigationDestination(for: McpServerID.self) { serverID in
                    McpServerDetail(runtime: runtime, model: model, serverID: serverID) {
                        navigationPath = NavigationPath()
                    }
                }
        }
        // Subscribed before the list is read, so no change falls between.
        .task { await model.observeStateChanges() }
        .task { await model.load() }
        // The per-workspace switches read the app's list, which another
        // client or the CLI may have changed since it was read.
        .task { await runtime.refreshWorkspaces() }
        .sheet(isPresented: $adding) {
            McpServerSheet(title: "Add MCP Server", actionTitle: "Add", draft: McpServerDraft()) { config in
                let server = try await model.add(config)
                navigationPath.append(server.serverID)
                // A server that needs a sign-in gets one as it is added.
                if server.state.status == .signedOut {
                    runtime.mcpSignIn.signIn(server.serverID)
                }
            }
        }
        .sheet(item: $importing) { request in
            McpImportSheet(source: request.source, model: model)
        }
        // `.mcp.json` starts with a dot, so hidden files are shown.
        .fileImporter(isPresented: $choosingFile, allowedContentTypes: [.folder, .json]) { result in
            if case .success(let url) = result {
                importing = McpImportRequest(source: .file(path: url.path))
            }
        }
        .fileDialogBrowserOptions(.includeHiddenFiles)
        .fileDialogMessage("Choose a project folder or its .mcp.json.")
        .fileDialogConfirmationLabel("Read")
    }

    private var serverList: some View {
        Form {
            Section {
                if let errorText = model.errorText {
                    Label {
                        Text("Could not read MCP servers")
                        Text(errorText)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                    }
                } else if !model.didLoadOnce {
                    HStack {
                        Text("Loading servers…")
                        Spacer()
                        ProgressView()
                            .controlSize(.small)
                    }
                } else if model.servers.isEmpty {
                    Text("No MCP servers yet.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(model.servers, id: \.serverID) { server in
                        NavigationLink(value: server.serverID) {
                            McpServerRow(server: server)
                        }
                    }
                }
            } header: {
                HStack {
                    Text("Servers")
                    Spacer()
                    Group {
                        Menu("Import") {
                            Button(McpImportSource.setup(harnessID: "claude").title) {
                                importing = McpImportRequest(source: .setup(harnessID: "claude"))
                            }
                            Button(McpImportSource.setup(harnessID: "codex").title) {
                                importing = McpImportRequest(source: .setup(harnessID: "codex"))
                            }
                            Button("Import from .mcp.json…") { choosingFile = true }
                        }
                        .fixedSize()
                        Button("Add Server…") { adding = true }
                    }
                    .disabled(!model.didLoadOnce)
                }
            } footer: {
                Text("New chats with any agent get these servers through Ottam, which keeps their secrets. A local server runs once per workspace, while chats there use it.")
            }
        }
        .formStyle(.grouped)
    }
}

private struct McpServerRow: View {
    let server: McpServerSummary

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: server.config.isLocal ? "terminal" : "globe")
                .foregroundStyle(.secondary)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(server.config.name)
                Text(server.config.connectionText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            McpServerStatusLabel(state: McpServerStatePresentation(state: server.state))
        }
    }
}

struct McpServerStatusLabel: View {
    let state: McpServerStatePresentation

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
            Text(state.text)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private var color: Color {
        switch state.tone {
        case .idle: .secondary
        case .busy: .yellow
        case .good: .green
        case .problem: .red
        }
    }
}

private struct McpServerDetail: View {
    let runtime: OttamAppRuntime
    let model: McpServersSettingsModel
    let serverID: McpServerID
    let onRemoved: () -> Void
    @State private var editing = false
    @State private var confirmingRemoval = false
    @State private var error: String?

    var body: some View {
        if let server = model.server(serverID) {
            form(server)
        } else {
            ContentUnavailableView("Server Removed", systemImage: "server.rack")
        }
    }

    private func form(_ server: McpServerSummary) -> some View {
        let state = McpServerStatePresentation(state: server.state)
        let config = server.config
        return Form {
            Section("Status") {
                LabeledContent("Status") {
                    McpServerStatusLabel(state: state)
                }
                if let detail = state.detail {
                    Text(detail)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if !config.isLocal {
                McpSignInSection(
                    server: server,
                    signIns: runtime.mcpSignIn,
                    redirectURI: model.redirectURI,
                    onSignedOut: { model.signedOut($0) }
                )
            }

            Section {
                LabeledContent(config.isLocal ? "Command" : "URL") {
                    Text(config.connectionText)
                        .textSelection(.enabled)
                        .multilineTextAlignment(.trailing)
                }
                if !config.valueNames.isEmpty {
                    LabeledContent(config.isLocal ? "Environment" : "Headers") {
                        Text(config.valueNames.joined(separator: ", "))
                            .multilineTextAlignment(.trailing)
                    }
                }
            } header: {
                HStack {
                    Text(config.isLocal ? "Local Server" : "Remote Server")
                    Spacer()
                    Button("Edit…") { editing = true }
                }
            }

            Section {
                if runtime.activeWorkspaces.isEmpty {
                    Text("No workspaces yet.")
                        .foregroundStyle(.secondary)
                }
                ForEach(runtime.activeWorkspaces) { item in
                    Toggle(item.workspace.displayName, isOn: enabled(server, in: item.workspace.workspaceID))
                }
            } header: {
                Text("Workspaces")
            } footer: {
                Text("Turning a server off applies to new chats in that workspace. Chats already open keep it until they end.")
            }

            Section {
                Button("Remove Server…", role: .destructive) { confirmingRemoval = true }
            }

            if let error {
                Text(error)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }
        }
        .formStyle(.grouped)
        .navigationTitle(config.name)
        .sheet(isPresented: $editing) {
            McpServerSheet(title: "Edit \(config.name)", actionTitle: "Save", draft: McpServerDraft(config: config)) { edited in
                try await model.update(serverID, config: edited)
            }
        }
        .confirmationDialog("Remove \(config.name)?", isPresented: $confirmingRemoval) {
            Button("Remove Server", role: .destructive) {
                Task {
                    do {
                        try await model.remove(serverID)
                        runtime.mcpSignIn.forget(serverID)
                        onRemoved()
                    } catch {
                        self.error = error.localizedDescription
                    }
                }
            }
        } message: {
            Text("Its processes stop and its secrets and sign-in are deleted from the Keychain. Chats already open lose its tools.")
        }
    }

    private func enabled(_ server: McpServerSummary, in workspaceID: WorkspaceID) -> Binding<Bool> {
        Binding(
            get: { !server.disabledWorkspaceIDs.contains(workspaceID) },
            set: { enabled in
                Task {
                    do {
                        error = nil
                        try await model.setEnabled(serverID, workspaceID: workspaceID, enabled: enabled)
                    } catch {
                        self.error = error.localizedDescription
                    }
                }
            }
        )
    }
}

/// A remote server's sign-in: Sign In… or Sign Out, the browser wait with
/// Cancel, the last failure, and a client ID for a server that cannot
/// register Ottam itself, with the redirect URI to register.
private struct McpSignInSection: View {
    let server: McpServerSummary
    let signIns: McpSignInController
    let redirectURI: String?
    let onSignedOut: (McpServerSummary) -> Void
    @State private var clientID = ""
    @State private var signingOut = false
    @State private var error: String?

    private var phase: McpSignInPhase? { signIns.state.phase(server.serverID) }
    private var isSignedIn: Bool { server.signedIn && server.state.status != .signedOut }

    var body: some View {
        Section {
            LabeledContent("Account") {
                Text(statusText)
                    .foregroundStyle(.secondary)
            }
            if phase == .waiting {
                HStack {
                    ProgressView()
                        .controlSize(.small)
                    Text("Finish signing in in your browser…")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel") { signIns.cancel(server.serverID) }
                }
            } else if case .needsClientID(let message) = phase {
                Text(message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                TextField("Client ID", text: $clientID)
                    .onSubmit(signInWithClientID)
                HStack {
                    Spacer()
                    Button("Sign In…", action: signInWithClientID)
                        .disabled(clientID.trimmingCharacters(in: .whitespaces).isEmpty)
                        .keyboardShortcut(.defaultAction)
                }
            } else {
                if let failure = failureText {
                    Text(failure)
                        .font(.callout)
                        .foregroundStyle(.orange)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack {
                    Spacer()
                    if isSignedIn {
                        Button("Sign Out", action: signOut)
                            .disabled(signingOut)
                    } else {
                        Button("Sign In…") { signIns.signIn(server.serverID) }
                    }
                }
            }
        } header: {
            Text("Sign-In")
        } footer: {
            if let redirectURI {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Sign-in opens in your browser and returns to Ottam at this redirect URI. Register it with a server that asks for a client ID.")
                    HStack {
                        Text(redirectURI)
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                        Button("Copy") { WorkspacePasteboard.copy(redirectURI) }
                            .buttonStyle(.link)
                            .font(.caption)
                    }
                }
            }
        }
    }

    private var statusText: String {
        if phase == .waiting { return "Signing in…" }
        return isSignedIn ? "Signed in" : "Not signed in"
    }

    private var failureText: String? {
        if let error { return error }
        if case .failed(let message) = phase { return message }
        return nil
    }

    private func signInWithClientID() {
        let trimmed = clientID.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        signIns.signIn(server.serverID, clientID: trimmed)
    }

    private func signOut() {
        signingOut = true
        error = nil
        Task {
            defer { signingOut = false }
            do {
                onSignedOut(try await signIns.signOut(server.serverID))
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
