---
kind: architecture
status: active
---

# MCP Proxy and Managed Servers

Core is the MCP client to every configured MCP server, and an MCP server to
every agent session. Agents never connect to a configured server directly:
each session reaches all of them through one loopback proxy in core
(Streamable HTTP, per-session bearer token), so sign-in, secrets, process
ownership and failure state live in one place for every harness, profile and
agent. Ottam's own tools ([Delegation](#delegation)) are served from the same
proxy on its reserved `ottam` route. Open work:
[managed servers](../../specs/backend/mcp/managed-servers.md),
[Ottam tools](../../specs/backend/mcp/ottam-tools.md).

Code: `ottam-core/src/mcp/` (`McpService` keeps config, runtime and proxy
paths in step); types in `ottam-protocol/src/mcp.rs`. Swift presents state and
actions only. A proxy that cannot listen costs sessions their MCP servers and
nothing else; at shutdown it stops after the agent bridge.

## Sessions, Entries and Tokens

Before `session/new` or `session/load`, the conversation runner asks for a
grant (`ConversationRunCoordinator::grant_mcp`): one token and one ACP
`McpServerHttp` entry per server on for the run's workspace. A run continuing
a held session keeps its token. The model probe and replay loads send no
entries. A server that cannot be leased is left out; it
never fails the run.

Agents connect and list tools when the session opens, not on first call.
Oh-My-Pi failed `session/new` on a connect error within 250 ms and OpenCode
blocked the session request up to 30 s, so core answers `initialize` itself
whatever state the upstream is in.

- **Entries** (`mcp/entries.rs`): servers off in the workspace are left out.
  For existing-setup Claude Code, Codex and OpenCode instances, so is any
  server that setup's own config already starts (matched by name after
  whitespace→`_`, codex-acp's rule, or by `url:`/`stdio:` signature). Ottam
  profiles and Oh-My-Pi read no MCP config under ACP and are never filtered.
- **Token format:** 256 random bits in base64url, which can never start with
  `!`; Oh-My-Pi runs such header values as shell commands.
- **Token scope and lifetime:** a token reaches only the paths its sessions
  were given; adding or removing a server applies to new sessions only. It is
  bound to the session id and refused once the adapter restarts or drops the
  session, when the conversation is deleted, and at shutdown. Tokens are
  memory-only. A live token holds a lease on each slot its entries name.
- **OpenCode** keeps one MCP connection per server and directory with the
  latest session's headers, so its token is shared per adapter process and
  directory; calls name their session in `_meta["ai.opencode/sessionID"]`.
- **New headers on `session/load`:** Claude and Oh-My-Pi rebuild their MCP
  clients, OpenCode replaces the connection for every session in that
  directory, Codex applies them only on a cold resume. So core sends
  `session/close`, when advertised, before reloading a held session
  (`close_held_session`).

A request with no live token gets a bare 401 with no OAuth resource metadata,
so no agent starts a sign-in against core. `Host`/`Origin` checks accept only
loopback hosts and refuse any request with an `Origin`.

## Calls

Upstream failures reach the agent as MCP errors, never HTTP errors. A call to
a signed-out server tells the agent the user must sign in and sends the chat
`mcp.signInNeeded`. Cancelling a call cancels it upstream; the agent's own
`_meta` is not forwarded. Calls are attributed by token, then by `tools/call`
`_meta`; core reads only Claude's `claudecode/toolUseId` (`mcp/calls.rs`), so
Codex and OpenCode subagent calls are attributed to the parent session.
Oh-My-Pi subagents get no MCP tools under ACP.

Adapter quirks the proxy absorbs: MCP `2026-07-28` clients (Claude Code) have
no `initialize`, so the proxy serves `subscriptions/listen` and adds
`resultType: "complete"` to tool lists and finished calls. OpenCode appends
`?codemode=false` and retries without it on 400/404; the proxy ignores query
strings. OpenCode and Oh-My-Pi send `DELETE` with old headers when replacing a
connection, so revoked-token 401s there are logged, not surfaced.

A server's `elicitation/create` has no link to a call: core routes it to the
chat with calls in flight on that slot and declines it when more than one
chat does. Core declares form elicitation and does not advertise sampling.
`list_changed` fans out to every session on the path, and core sends one on
every restart of a connection after its first start.

## Upstream Connections

Each server has slots (`mcp/runtime.rs`): one per workspace for a local
server, one shared for a remote server, so a stateful server shares state
across a workspace's chats. Connections use the `initialize` handshake, not
rmcp's `server/discover` probe, which can wait 10 s on a server that ignores
unknown methods.

- **Local:** own process group; the workspace's first root as cwd, its roots
  as MCP roots, the managed Node runtime's bin first on `PATH`. The last
  stderr line is kept in memory for the failed state and never logged.
- **Remote:** declares no roots and follows same-origin redirects only, so
  configured headers never reach another host.
- **Slots:** a slot with no lease stops when idle; a removed server, an
  archived workspace and quitting stop slots at once. Editing a server stops
  its connections but keeps the slots open sessions hold; a local/remote move
  drops them. A failed edit restores the secrets it wrote.

## Sign-in

`mcp.signIn` runs rmcp's `auth` client (`mcp/oauth.rs`); core and app both
refuse an authorization URL that is not https or loopback http. The callback
is a fixed port and path, `127.0.0.1:47391/oauth/callback`
(`OTTAM_MCP_REDIRECT_PORT`; tests use 0), so a hand-registered redirect URI
never changes; it is bound only while one sign-in waits, and a request without
that sign-in's `state` leaves it waiting. Signing in does not repeat the
failed call. A 401 that one refresh does not cure marks the server signed out
until a sign-in or an edit clears that; each clearing starts a new sign-in
epoch, and a 401 from a connection begun in an earlier epoch does not count.

## Config and Secrets

Server names are the proxy path (1–64 of `[A-Za-z0-9_-]`, unique). Every
server is on for every workspace unless that workspace turned it off
(`workspace_mcp_disabled`). Config refuses headers the MCP client sets itself.
Secret env values and headers, and OAuth credentials, live in the login
Keychain under service `dev.ottam.Ottam.mcp` (`mcp/secrets.rs`); they never
come back out through the protocol, and tokens and codes are never logged.
Off macOS, and in test builds told `OTTAM_SECRET_STORE=memory`, a
process-local store forgets them at exit; release builds ignore the variable.

Import (`mcp/import.rs`) reads Claude Code, Codex or `.mcp.json` definitions
without saving; nothing is merged into an existing name. `${VAR}` references
and harness-specific fields are imported as written, never expanded or run.
Every header value, and env values named like credentials, are marked secret.
Core never reads a CLI's credentials, so imported OAuth servers arrive signed
out.

## Delegation

An agent delegates work to another agent through Ottam's own MCP tools on the
`ottam` route (`mcp/ottam.rs`, `conversation_runner/delegation.rs`,
`ottam-protocol/src/delegation.rs`); each child is a separate Ottam
conversation linked to its parent.

### Tools And Roles

Parent-only: `delegate`, `message_child`, `list_children`, `stop_child`
(`PARENT_ONLY`). Children get `message_parent`. Both get the history tools and
`get_conversation_info`. Delegation is one level deep; a call outside the
caller's role is refused. Every tool returns at once and none waits on a
child, so no harness MCP timeout applies. Tool text says children are
unreachable through the agent's own agent tools (`NOT_PEERS`): Claude sessions
sharing a config home see one another through `ListAgents`/`SendMessage`, and
the switches that turn that off are too broad, so Ottam steers by text.

`delegate` takes a [named agent](#named-agents) or an engine/model/mode, never
both; neither means the caller's own agent and latest model. A parent has at
most `MAX_WORKING_CHILDREN` working children. `message_child` runs an idle
child at once and a working child at its turn end. `stop_child` cancels the
child with stop cause `parentAgent` and wakes no one.

### Push-Back And Notes

When a child run ends, or it calls `message_parent`, core adds a note to the
parent. If the parent is idle and no other child works, core starts a run
from a hidden `noteDelivery` message with origin `user` (autonomous runs never
prompt). Otherwise notes batch until the parent's run ends, a user message
arrives (notes go before its text), or the user presses Continue
(`conversation.continueDelegation`). `message_parent` wakes an idle parent
even while other children work. After `MAX_WAKE_UPS` automatic follow-ups in
a row caused by one child since the user's last message, that child's notes
wait for the user and the chat says so.

The delivery is a core-written `session/prompt` marked as from Ottam; the
agent sees a user turn that counts toward context and replays as user text.
User messages are never altered. A note counts delivered (`note_delivered_at`)
only once a run carrying it has started; after relaunch, waiting notes wait
for Continue or the user. A reply that is only `::ottam-ack{}`
(`DELEGATION_ACK_DIRECTIVE`) shows nothing and the history tools omit it.

### Deferred Reports

A child run started by the user's own message, and autonomous runs after it,
post a `deferred` report: it wakes no one, counts no wake-up, is never held by
the cap, and rides on the parent's next run of any kind.
`ConversationSummary.deferredReportRunID` names the child turn that shows Send
to Parent; `conversation.sendToParent` un-defers it and resets the child's
wake count, and a working parent refuses with `conversationBusy`.

### Stopping, Approvals And Modes

Stop on a parent's turn cancels its children's running runs; a turn that
simply ends stops none. At quit, children's interrupted turns are not resumed
and their notes wait for the user. `ConversationDeleteParams.children` is
`keep` (default; children become top-level) or `delete`.

At creation a child copies the parent's auto-approval; afterwards the setting
is the child's own, and core never changes a child's mode. A child created in
its agent's read-only mode copies no auto-approval. Per-agent modes live on
`AgentDefinition` (`agent/instances.rs`). Oh-My-Pi children start in
`default`, not `plan`, because its `plan` mode ends every turn with a question
or plan proposal. A parent that asks before acting may hand a child only one
of its agent's `asking_modes`, or its own current mode on the same agent.

### Named Agents

`named_agents` rows (`server/named_agents.rs`) hold name (unique ignoring
case), engine, model, mode, instructions and avatar seed; the mode defaults to
the agent's read-only mode. Instructions are prepended to the child's first
prompt, since ACP has no instructions field; compaction can summarize them
away. A child's title is `"<agent display name>: <task title>"`
(`conversations.delegated_task_title`); an agent's own session title never
replaces it. OpenCode runs MCP tools in code mode inside `execute`, so a
`delegate` card no call claims is matched to its child by prompt key.

### History Tools

`get_session_messages` (`server/chat/history.rs`) reads a child, the caller's
parent (a child's default) or the caller's own conversation (the default when
not delegated). `get_session_toolcall_output` pages input, file changes, then
output. `get_conversation_info` gives identity, lineage, per-turn token usage
and last reported context. Every wrapped prompt names its conversation
(`Ottam conversation: <id>`) so these default correctly; see [ACP › Engine
Switch Handoff](acp.md#engine-switch-handoff).

## Current Limits

- Only tools are served; resources and prompts are not relayed.
- `signedIn` means core holds tokens; `status` turns `signedOut` only once the
  server refuses them, so the two can disagree until the next call.
- A shared OpenCode token whose sessions span workspaces reaches the servers
  of the workspace it was last minted for.
- Two cores on one Mac share port 47391, so only one can wait for a sign-in.
