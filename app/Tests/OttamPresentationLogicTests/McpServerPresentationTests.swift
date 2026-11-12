import OttamBackendClient
import Testing

@testable import OttamPresentationLogic

@Suite("MCP server presentation")
struct McpServerPresentationTests {
    private let local = McpServerConfig(
        name: "docs",
        transport: .stdio(
            command: "npx",
            args: ["-y", "docs server", "--root=/work"],
            env: [
                McpConfigValue(name: "MODE", value: "test", secret: false),
                McpConfigValue(name: "API_TOKEN", secret: true),
            ]
        )
    )

    @Test("each state reads as Settings shows it, a failure with its error")
    func states() {
        let failed = McpServerStatePresentation(state: McpServerState(status: .failed, error: "exited with status 1"))
        #expect(failed.text == "Failed")
        #expect(failed.tone == .problem)
        #expect(failed.detail == "exited with status 1")
        #expect(McpServerStatePresentation(state: McpServerState(status: .notStarted)).text == "Not started")
        #expect(McpServerStatePresentation(state: McpServerState(status: .running)).tone == .good)
        #expect(McpServerStatePresentation(state: McpServerState(status: .signedOut)).text == "Signed out")
    }

    @Test("a server's row reads its command line or URL, and its value names")
    func summaries() {
        #expect(local.isLocal)
        #expect(local.connectionText == #"npx -y "docs server" --root=/work"#)
        #expect(local.valueNames == ["MODE", "API_TOKEN (secret)"])
        let remote = McpServerConfig(name: "search", transport: .http(url: "https://search.example.com/mcp", headers: []))
        #expect(!remote.isLocal)
        #expect(remote.connectionText == "https://search.example.com/mcp")
    }

    @Test("an unchanged saved secret is sent without a value, so core keeps it")
    func savedSecretsStay() {
        var draft = McpServerDraft(config: local)
        #expect(draft.environment[1].keepsStoredSecret)
        #expect(draft.config == local, "an untouched draft sends what it was given")

        draft.environment[1].value = "new-token"
        guard case .stdio(_, _, let replaced) = draft.config.transport else {
            Issue.record("still local")
            return
        }
        #expect(replaced[1] == McpConfigValue(name: "API_TOKEN", value: "new-token", secret: true))

        // Renamed, it is a new secret, and needs a value of its own.
        draft.environment[1].value = ""
        draft.environment[1].name = "TOKEN"
        #expect(!draft.environment[1].keepsStoredSecret)
        guard case .stdio(_, _, let renamed) = draft.config.transport else {
            Issue.record("still local")
            return
        }
        #expect(renamed[1] == McpConfigValue(name: "TOKEN", value: "", secret: true))
    }

    @Test("blank rows are dropped, fields are trimmed, and switching kind keeps both")
    func drafts() {
        var draft = McpServerDraft()
        #expect(!draft.canSubmit)
        draft.name = " docs "
        #expect(!draft.canSubmit, "a local server needs a command")
        draft.command = " uvx "
        draft.arguments = [.init("mcp-docs"), .init("")]
        draft.environment = [.init(), .init(name: "MODE", value: "test")]
        #expect(draft.canSubmit)
        #expect(
            draft.config == McpServerConfig(
                name: "docs",
                transport: .stdio(
                    command: "uvx",
                    args: ["mcp-docs"],
                    env: [McpConfigValue(name: "MODE", value: "test", secret: false)]
                )
            )
        )

        draft.kind = .remote
        #expect(!draft.canSubmit, "a remote server needs a URL")
        draft.url = "https://docs.example.com/mcp"
        draft.headers = [.init(name: "Authorization", value: "Bearer abc", secret: true)]
        #expect(
            draft.config.transport == .http(
                url: "https://docs.example.com/mcp",
                headers: [McpConfigValue(name: "Authorization", value: "Bearer abc", secret: true)]
            )
        )
        draft.kind = .local
        #expect(draft.command == " uvx ", "kept as typed; trimmed only when sent")
    }

    @Test("an import ticks each new server, and a name already added cannot be chosen")
    func importSelection() {
        let search = McpServerConfig(name: "search", transport: .http(url: "https://search.example.com/mcp", headers: []))
        var selection = McpImportSelection(
            McpImportCandidatesResult(
                path: "/work/.mcp.json",
                candidates: [
                    McpImportCandidate(config: local, existingServerID: McpServerID("mcp_1"), notes: []),
                    McpImportCandidate(config: search, sourceName: "my search", notes: ["${TOKEN} is not expanded"]),
                ],
                skipped: [McpImportSkipped(name: "legacy", reason: "SSE servers are not supported")]
            )
        )
        #expect(selection.rows.map(\.isSelected) == [false, true])
        #expect(selection.rows[0].isSettled)
        #expect(selection.rows[0].status == "Already added")
        #expect(selection.rows[1].details == ["https://search.example.com/mcp", "Named “my search” there"])
        #expect(selection.chosen == [search])
        #expect(!selection.isFinished)

        // Ticking the existing one changes nothing: it is not sent again.
        selection.rows[0].isSelected = true
        #expect(selection.chosen == [search])

        selection.record(.failed("mcpServerNameTaken"), for: "search")
        #expect(selection.rows[1].status == "mcpServerNameTaken")
        #expect(selection.chosen == [search], "a failure can be tried again")
        selection.record(.imported, for: "search")
        #expect(selection.chosen.isEmpty)
        #expect(selection.isFinished)
        #expect(McpImportSource.setup(harnessID: "codex").title == "Import from Codex")

        // Turned off where it was read: offered, but not chosen by default.
        let off = McpImportSelection(
            McpImportCandidatesResult(
                path: "/home/.codex/config.toml",
                candidates: [McpImportCandidate(config: search, notes: [], disabledInSource: true)],
                skipped: []
            )
        )
        #expect(off.rows.map(\.isSelected) == [false])
        #expect(!off.rows[0].isSettled)
        #expect(off.chosen.isEmpty)
    }

    @Test("a sign-in waits, then shows how it ended; a replaced one's cancel does not count")
    func signInPhases() {
        let linear = McpServerID("mcp_linear")
        let docs = McpServerID("mcp_docs")
        var state = McpSignInState()
        state.started(linear)
        #expect(state.phase(linear) == .waiting)
        #expect(state.isWaiting)

        // Starting it again replaces it; core finishes the first as cancelled.
        state.started(linear)
        state.finished(McpSignInFinishedParams(serverID: linear, outcome: .cancelled))
        #expect(state.phase(linear) == .waiting)

        // Core runs one at a time, so another server's leaves this one behind.
        state.started(docs)
        #expect(state.phase(linear) == nil)
        state.cancelRequested(docs)
        state.finished(McpSignInFinishedParams(serverID: docs, outcome: .cancelled))
        #expect(state.phase(docs) == .cancelled)

        state.started(linear)
        state.finished(McpSignInFinishedParams(serverID: linear, outcome: .failed, error: "denied"))
        #expect(state.phase(linear) == .failed("denied"))
        state.started(linear)
        state.finished(McpSignInFinishedParams(serverID: linear, outcome: .signedIn))
        #expect(state.phase(linear) == .signedIn)
        #expect(!state.isWaiting)
    }

    @Test("a started sign-in opens the browser only while it still waits")
    func browserOpens() {
        let linear = McpServerID("mcp_linear")
        let docs = McpServerID("mcp_docs")
        var state = McpSignInState()
        state.started(linear)
        #expect(state.opensBrowser(linear))
        state.cancelRequested(linear)
        #expect(!state.opensBrowser(linear), "cancelled while core found the sign-in")
        state.started(linear)
        state.started(docs)
        #expect(!state.opensBrowser(linear), "replaced by another server's")
        #expect(state.opensBrowser(docs))
    }

    @Test("only a web page opens for a sign-in")
    func signInPages() {
        #expect(McpSignInPage.url("https://auth.example.com/authorize?state=x") != nil)
        #expect(McpSignInPage.url("http://127.0.0.1:48101/authorize") != nil)
        #expect(McpSignInPage.url("http://localhost/authorize") != nil)
        #expect(McpSignInPage.url("http://[::1]:8080/authorize") != nil)
        #expect(McpSignInPage.url("http://auth.example.com/authorize") == nil)
        #expect(McpSignInPage.url("file:///Applications/Calculator.app") == nil)
        #expect(McpSignInPage.url("smb://share.example.com/x") == nil)
        #expect(McpSignInPage.url("vscode://open?file=/etc/passwd") == nil)
    }

    @Test("a chat's card follows the server's sign-in until the user writes again")
    func signInCards() {
        let linear = McpServerID("mcp_linear")
        let chat = ConversationID("chat-1")
        var state = McpSignInState()
        let needed = McpSignInNeededParams(serverID: linear, serverName: "linear", conversationID: chat)
        state.needed(needed)
        state.needed(needed)
        var cards = state.cards(for: chat)
        #expect(cards.count == 1, "one card per server")
        #expect(cards[0].title == "Sign in to linear")
        #expect(cards[0].action == .signIn)
        #expect(state.cards(for: ConversationID("chat-2")).isEmpty)

        state.started(linear)
        cards = state.cards(for: chat)
        #expect(cards[0].title == "Signing in to linear…")
        #expect(cards[0].action == .cancel)

        state.finished(McpSignInFinishedParams(serverID: linear, outcome: .signedIn))
        cards = state.cards(for: chat)
        #expect(cards[0].isSignedIn)
        #expect(cards[0].detail == "Ask the agent to try again.")
        #expect(cards[0].action == nil)

        // Lost again: the old success no longer holds.
        state.needed(needed)
        #expect(state.cards(for: chat)[0].action == .signIn)

        state.needsClientID(linear, message: "Register Ottam.")
        #expect(state.cards(for: chat)[0].action == nil, "trying again without the client ID fails the same way")
        #expect(state.cards(for: chat)[0].failure?.contains("Settings › MCP Servers") == true)
        state.startFailed(linear, message: "unreachable")
        #expect(state.cards(for: chat)[0].action == .tryAgain)

        // A message that fails to send puts its cards back, ahead of new ones.
        let docs = McpSignInNeededParams(serverID: McpServerID("mcp_docs"), serverName: "docs", conversationID: chat)
        let cleared = state.clearNeeds(chat)
        #expect(state.cards(for: chat).isEmpty)
        state.needed(docs)
        state.needed(needed)
        state.restoreNeeds(cleared, for: chat)
        #expect(state.cards(for: chat).map(\.serverName) == ["linear", "docs"])

        state.clearNeeds(chat)
        #expect(state.cards(for: chat).isEmpty)
        state.needed(needed)
        state.forget(linear)
        #expect(state.cards(for: chat).isEmpty)
        #expect(state.phase(linear) == nil)
    }

    @Test("the pane answers to MCP words")
    func search() {
        let text = SettingsPane.mcpServers.searchText()
        #expect(text.localizedCaseInsensitiveContains("model context protocol"))
        #expect(text.localizedCaseInsensitiveContains("secret"))
        #expect(SettingsPane.mcpServers.title == "MCP Servers")
    }
}
