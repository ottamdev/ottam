---
kind: spec
status: active
---

# Ottam-Managed MCP Servers

Goal: configure MCP servers once in Ottam. Core connects to them, signs in
to them, and serves each to every agent through a loopback proxy, so one
list and one sign-in work across harnesses, profiles, and agents that
ignore their own MCP config under ACP. How it works now is in
[MCP](../../../architecture/backend/mcp.md).
[Ottam's own tools](ottam-tools.md) share the proxy.

## Current Status

Managed servers ship: proxy, secrets, OAuth sign-in, Settings › MCP Servers,
the Sign in card, and import. Only the walkthrough handoff (`mcp:9`) is
open, waiting on agents not yet installed in the isolated home.

## Active

### `mcp:9`: walkthrough and handoff

Run in an isolated build (`scripts/run-app.sh --isolated`) against the
`ottam-test-mcp` fixture: stdio for the local server, `--http --oauth` for
the remote one.

- [ ] Run the open steps: 3 (Oh-My-Pi half), 4, and step 9's duplicate
  filtering and Codex and Oh-My-Pi copies. Needs OpenCode, Oh-My-Pi and
  Codex installed in the isolated home, each signed in to a model
  provider. Done when they pass or their limits are stated. Until then
  their entries are covered only by `mcp/tests.rs` and
  `tests/jsonl_protocol/mcp.rs` (duplicate filtering by
  `sessions_get_their_workspaces_servers_less_their_existing_setups_own`).

Try it:

1. In Settings › MCP Servers, add a local server (command, arguments, env
   values, one secret). It shows Not started.
2. Add a remote OAuth server by URL. Sign-in opens in the browser; after
   approving, Settings shows it signed in. Cancel, or a closed tab after
   the timeout, leaves it signed out and frees the redirect port. A server
   without dynamic registration asks for a client ID and shows the
   redirect URI. Another app holding the port fails sign-in saying so.
3. In workspace W, start an Oh-My-Pi chat and a Claude profile chat. Both
   list and call tools from both servers without signing in, and Settings
   shows the local server running. A Claude subagent can call a tool.
   Stop during a long call cancels it; the server keeps running for the
   other chat.
4. Start two OpenCode chats in W. Both call the tools; ending one does not
   break the other.
5. Sign out, or revoke at the provider. The next call fails clearly and
   the chat shows a Sign in card; signing in lets the next call succeed.
6. Kill the local server. Settings shows Failed with its error; the next
   call restarts it.
7. Quit: no server process outlives the app. Relaunch: restored chats call
   tools with new tokens, and the remote server is still signed in.
8. Turn the local server off for W. New chats in W don't get it, open
   chats keep it until they end, and workspace X still gets it.
9. Import from a detected Claude Code setup: each command is shown for
   approval before anything starts, and OAuth servers arrive signed out.
   A server Claude's own config has is not passed to the existing Claude
   setup again, while Codex and Oh-My-Pi get the proxied copy. A
   repository's `.mcp.json` imports the same way.

## Next

None.

## Backlog

Unscheduled; each is a current limit in the architecture doc.

- Serve resources and prompts, not only tools.
- One notion of "signed in": `signedIn` (tokens held) and the `signedOut`
  status can disagree until the next call.
- Attribute a shared OpenCode token's calls by workspace when its sessions
  span workspaces, instead of the latest mint's.
- Attribute Codex (`_meta.threadId`) and OpenCode subagent calls to the
  subagent; today only Claude's `claudecode/toolUseId` is read.
- Give local servers a clean environment rather than core's own.
- A sign-in port that two cores on one Mac do not share.
- Lower rmcp's expected-401 log lines from error level.
- A sign-in whose saved client ID the server has forgotten can wait at
  "Finish signing in…" until Cancel; signing in again registers anew.
- Client ID metadata documents, once Ottam has a stable public domain.

## Decisions

- **2026-10-21: core proxies every server.** Agents never connect to a
  configured server, so sign-in, secrets, process ownership and failure
  state live in one place and no harness authenticates an MCP server.
- **2026-10-21: agents reach the proxy over Streamable HTTP; no SSE.**
  Every pinned agent accepts http, MCP deprecated HTTP+SSE, and codex-acp
  and OpenCode don't accept SSE. No stdio shim.
- **2026-10-21: per-session bearer tokens on a random loopback port.**
  Agents copy ACP `McpServerHttp` headers onto every request, so no
  protocol support is needed. A token lives no longer than its session
  because it can leak (codex-acp saves it in thread config).
- **2026-10-21: one global list, off per workspace.** A repository's
  `.mcp.json` is offered for import, never loaded automatically.
- **2026-10-21: one process per local server per workspace; one shared
  connection per remote server.** Core multiplexes (user's choice,
  2026-10-22), accepting that a stateful server shares state across a
  workspace's chats.
- **2026-10-21: an existing setup's own servers are not passed again.**
  Core never writes the user's home, so it cannot suppress the native copy.
- **2026-10-21: one endpoint per server.** Tool names match what the agent
  would see natively, so per-server rules keep working.
- **2026-10-21: OAuth ships in the first milestone.** Core runs the flow
  and holds the tokens; harness tokens are not portable, so imported OAuth
  servers sign in again.
- **2026-10-21: system browser and a fixed loopback redirect** (user's
  choice over a native sheet): works with servers that reject
  custom-scheme redirects, and one fixed port and path lets a
  hand-registered client ID keep working.
- **2026-10-22: OpenCode tokens per adapter process and directory.**
  OpenCode keeps one connection per directory with the latest session's
  headers, so per-session tokens would let one chat's end revoke another's.
  Rejected: reloading other sessions with fresh tokens, and one OpenCode
  session per workspace.
- **2026-10-22: token never starts with `!`.** Oh-My-Pi runs such header
  values as shell commands.
- **2026-10-22: crates.** `rmcp =3.7.0` on both sides (Codex uses the same
  crate), with `auth` for OAuth; `axum =0.8.9` routes `/mcp/<server>` and
  the OAuth redirect. The upstream HTTP client is rmcp's reqwest 0.13
  native-tls backend beside core's reqwest 0.12; re-implementing it over
  0.12 isn't worth the duplicate build. `security-framework` 3 backs the
  Keychain `SecretStore` as a macOS-only dependency, with an in-memory
  store elsewhere.
