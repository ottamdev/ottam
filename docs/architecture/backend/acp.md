---
kind: architecture
status: active
---

# ACP and Agent Runtimes

Core speaks Agent Client Protocol to adapters pinned in `agent/registry.rs`;
Swift presents state and never speaks ACP. This doc records adapter behavior
seen on the wire, cross-boundary rules, and decisions not to undo. A version
named with an observation is where it was taken, not rechecked at the pin.

## Ownership

- One ordered actor per adapter (`agent/acp/actor.rs`) owns connection
  commands and completion events. Its shutdown acknowledgement is the teardown
  commit: after it, cancellation never restores a false Ready. Shutdown sends
  `session/cancel` and waits briefly so a turn ends on the agent's `stopReason`.
- `CLIENT_FEATURES` (`agent/acp/features.rs`) is the only source of what
  `initialize` advertises. Capability handling keys off the agent's
  `initialize` answer, never its id; verdicts are derived, never stored.
- `session/request_permission` waits for the user in a `ConnectionTo::spawn`
  task: SDK handlers run on the dispatch loop, so one that awaited the user
  would queue every later message, including replies to core's own requests.

### Token And Plan Usage

`PromptResponse.usage` needs the SDK's `unstable_end_turn_token_usage`
feature. What a turn's usage counts differs per adapter
(`AgentDefinition::turn_usage_scope`): Claude and OpenCode count the main
agent without subagents, Codex only the turn's last model request, others the
whole turn. `cost` is not surfaced. Plan usage is account-wide
(`agent/plan_usage.rs`): live from Claude's `_claude/rateLimit.unifiedWindows`,
else a probe of the bundled CLI (Claude `get_usage`, Codex
`account/rateLimits/read`) that makes no transcript, rollout or model call.
Per-model weekly caps come only from the probe; windows key by kind and model,
not label. Live Claude values are 0–1, the probe's 0–100.

### Unhandled Agent Traffic

The SDK's `SessionUpdate` is a closed enum whose typed handler logs a failed
frame payload included, so core claims every `session/update` raw
(`decode_session_update`, `agent/acp/protocol.rs`). The SDK's default handler
parks any unclaimed message carrying a `sessionId` until a dynamic handler
exists; core adds none, so a catch-all registered last answers unclaimed
requests `method_not_found` and drops unclaimed notifications. It runs before
dynamic handlers: adopting the SDK's session helpers means removing it.
Declines use `retry: false`; a retry parks the frame forever.

Dropped traffic logs as `Unhandled agent traffic` (`crate::diagnostics`) with
a label and the payload's shape, never values. Labels: `update:<tag>`,
`malformed:<tag>`, `field:<tag>` (debug builds: dropped field, defaulted
value, skipped list item ending `[]`), `unmodeled:<kind>`, `extension`
(unknown `_meta` key), `notification:<method>`, `request:<method>`
(catch-all), `elicitation:mode|scope|field` (form declined).
`unhandled_nothing_in_the_recorded_sessions` (`agent/acp/tests.rs`) replays
`agent/fixtures/`; adding an agent is one registry entry plus a recording. The
ACP wire log misses a Claude agent on an existing home: `RefreshSafeTransport`
spawns it outside the SDK's debug hook.

## Engine Switch Handoff

A message that is exactly `/name` or `/name args` with no images goes to the
adapter as typed, without the preamble: adapters parse commands only from a
prompt starting with `/` (`is_slash_command`), and it goes out unseeded.
Sessions are scoped to one engine, so a new session's first wrapped prompt
replays earlier turns (`server/chat/history.rs`) after the user request, so
`is_ottam_prompt_fallback_title` still works. The seed follows delivery: a
rejected prompt undoes it; an in-turn error, lost adapter or stop keeps it; a
stop while waiting out a busy agent undoes it though the input reads
delivered. A → B → A resumes A's session with no catch-up; replay is text only
([persistence](persistence.md#transcript-ownership-and-order)).

## Agent Questions (Form Elicitation)

Core advertises `elicitation.form` only and claims `elicitation/create` raw so
AIR markers survive (`agent/acp/elicitation.rs`). A select pairs with its
free-text companion by `_meta.jetbrains.air.customAnswer` (Claude, Codex), else
`<key>_custom` (OpenCode, Claude) or `<key>__other` (oh-my-pi); Codex marks
secrets `_meta.codex.isSecret`. The broker (`server/agents/permission.rs`) sets
no timeout. Skip is `decline`, except `FormSkip::EmptyAccept` sends `accept {}`
(OpenCode 2.0.27 treated `decline` as cancel). Cancellation travels both ways:
`$/cancel_request` in, `$/cancelRequest {id}` out. A form with no `toolCallId`
(MCP; Codex `request_user_input` at codex-acp 2.1.1) leaves no transcript
record ([persistence](persistence.md#agent-questions)).

## Runtime Resolution And Installation

`OTTAM_NODE` is authoritative even when broken. A complete system toolchain at
Node 22+ beats the managed runtime; per-engine floors otherwise decide. The
pi-family slot is oh-my-pi, not upstream pi: pi refuses in-tree ACP. Managed
runtime and adapter installs are user-initiated: an explicit Get or Install,
or the agents a user ticks in first-launch onboarding. An installed older
runtime is the standing consent for a new pin
(`RuntimeInstallService::reconcile`). Only an older
runtime stands in; a newer one left by a rolled-back app never runs.

Child PATH (decided 2026-11-16, because a Finder launch hands core launchd's
PATH and agents could not find `gh` or `brew`): children start from the PATH
of the user's interactive login shell, captured by the same run that finds
Node. Only PATH is imported; the rest of a shell's environment is session
state, or exports such as API keys that would quietly change an agent.
Resolved Node goes last on an adapter's PATH so the user's own `node` wins in
agent shells, and first wherever npm or npx re-resolve it from their shebang
(`agent/node_resolver.rs`).

## Managed Adapter Lifecycle And Setup UI

- Adapters install at an exact pin; `require_runtime` refuses an uninstalled
  adapter rather than installing on demand. Install completes only after an
  ACP handshake and catalog probe (`.pending-<version>` marker; startup deletes
  any still pending). Standing-consent and older-only rules match the runtime
  (`ManagedAdapterInstalls::launchable`). Core refuses an npm adapter install
  without a ready Node, so Settings disables every npm install action while
  Node has a problem and points at the Runtime section rather than installing
  Node itself; prebuilt binaries never wait
  (`AgentAdapterInstallPresentation`). A pin upgrade stops no running
  adapter. Registration order is the default-pick order; the app keys icons by
  brand and renders agents from `agent.list` with no per-agent Swift.
- Each instance gets an isolated home via `home_variables`, or
  `home_arguments` for a CLI with no variable for it; core never overrides
  `HOME` (agent shells and Git would inherit it) and never falls back to the
  vendor home. Isolation exists because sharing Claude's rotating keychain
  refresh token with the terminal CLI forced daily re-login. Oh-My-Pi needs
  `OMP_PROFILE=""` (a named profile overrides the dir). A definition's
  `launch_unset` applies to the adapter, the catalog probe, and the terminal
  sign-in alike. Under `CLAUDECODE`, adapter launches also unset inherited
  `CLAUDE_*`.
- Deletion tombstones the instance (survives restart, no fallback to a
  sibling). Purge removes only an unredirected managed home, never vendor state
  (Claude keychain entries, `~/.omp`). Existing-setup homes: Ottam never
  creates, purges, signs out, cleans locks in, or queries credentials of them;
  detection (`agent/setup_detection.rs`) never launches or writes.
- Sign-in: `_auth/status_update` `none` means missing credentials, a
  recognized kind means ready, anything else leaves the verdict alone. Claude
  creates and loads sessions while signed out, so those never prove sign-in.
  Claude also sends the auth-status RFD's `auth/status_update`; core claims it
  unread once the connection has sent the `_` form
  (`STANDARD_AUTH_STATUS_UPDATE` in `agent/acp/actor.rs`). Sent alone, it logs
  as unhandled traffic, since core does not read its verdict.
  Adapters read API keys only at launch, so secret-only auth methods are
  hidden. Under `SignInProof::ModelSelector` a clean login exit proves nothing.
- Claude OAuth refresh: `<home>/.oauth_refresh.lock` or legacy
  `<realpath(home)>.lock` means a refresh is running unless
  `.oauth_refresh.lock.owner` proves the holder gone (same host, matching birth
  times, pid absent, zombie or reused). Cleanup never uses lock age and only
  touches Ottam-owned homes; killing the CLI mid-refresh loses the rotated
  token. Existing CLI homes' locks fence Quit, restart and removal.
- OpenCode is outside `ACP_ADAPTERS` (no terminal output, `_auth/status_update`
  or steering); sign-in is validated by session creation. Every launch loads
  `agent/plugins/opencode-foreground.js`, which strips `background` from
  `shell` (OpenCode resumes the session for its result with no task
  lifecycle, so nothing arms a follow-up) and restores core's environment
  from `OTTAM_SHELL_ENVIRONMENT`. Subagents always run in the background:
  core advertises `opencode/child-session-updates`, and
  `agent/acp/child_sessions.rs` threads each child from the run statuses in
  `opencode/session/child_update`, not from its control call, which
  completes as the child starts. A `subagent` call naming a finished child's
  `sessionID` is a resumed run (`<child>:generation:<n>`) whose prompt it
  carries; one naming a running child joins that run and shows as a
  follow-up. The parent's turn woken by a child's report arrives with no
  prompt and lands in a follow-up run (the child's end arms it).
  `session/cancel` reaches neither a background child nor a woken turn, and
  a child's report, even a cancelled one, wakes the parent. So Stop on a
  root that has had a child sends `session/close` to the open children,
  waits a second for the wake their reports cause, then closes the root,
  which interrupts it (`actor.rs::close_background_tree`); a root closed
  before the wake leaves the woken turn running. The close runs beside the
  actor's command loop, so other sessions keep being served while that root
  refuses new prompts. Until then the woken turn's own output is dropped, so
  none of it reaches the stopped run. The next run loads the root again. A
  failed or timed-out close fails the Stop; a child it missed stays open
  for the next Stop, and a root it missed stays held. A stopped follow-up
  run ends with that error. An in-turn Stop's run still ends cancelled, as
  Stop never waits on the cancelled prompt
  ([Stop And Steer](persistence.md#stop-and-steer)); the actor logs the
  failure. The whole close gets one budget (`OUT_OF_TURN_STOP_BUDGET`)
  below the runner's out-of-turn wait, itself below the cancel grace that
  aborts the run's worker, so the result reaches the run. A managed
  profile disables the CLI's shared background service (`config/service.json`;
  fixed port 49374 would mix profiles' sign-ins). At 2.0.27: plan mode not
  enforced, write/patch emit no diffs, slash output doesn't stream.
- Oh-My-Pi mounts MCP tools as devices (`write xd://<tool>`);
  `agent/acp/xd_devices.rs` rewrites them to `mcp__<server>_<tool>` calls
  stamped `ottamDeviceCall` so they never count as file changes. It disables
  its own MCP config under ACP, is not forked despite advertising
  `session/fork`, sends no `async_task_*`, and rejects a prompt while busy
  (`-32003`, `session_busy`) instead of queueing, so the runner resends.
- Antigravity (`agy acp`) has no home variable: `--gemini-dir` moves its
  state, and its Google sign-in stays in the Keychain for every home, so
  managed profiles share the Mac's login. Core never sends
  `_meta.antigravity.systemPrompt`, though agy advertises
  `systemPrompt.append`. A forum answer, not confirmed as Google's, says a
  harness that injects system prompts on a consumer sign-in risks the
  account. Launches unset `GEMINI_API_KEY` and `GOOGLE_API_KEY` (either
  switches agy to key auth) and set `AGY_CLI_DISABLE_AUTO_UPDATE`. The
  default sign-in is its `terminal-login`: the launch plus `--login`. Models
  arrive as `configOptions` selects (family, thinking level), which core
  reads, and again as legacy `models` ids with the effort baked in, which it
  ignores. At 1.5.16 a bare `session/new` writes a conversation, so the probe
  runs on a scratch `--gemini-dir`.

## Permission Configuration Limits

Core refuses an unverifiable, rejected or substituted selection. A live switch
is strict: a value the session doesn't list, a failed request, or an answer
naming another value fails the run `selectionUnavailable`, so a run never
records a model that didn't answer. Switches apply in `session/new`'s order:
model, then thought level (checked against the switch's answer, which can move
it), then mode. An agent without the select ignores the pick; a restored
session keeps a value its load doesn't list. Only confirmed values persist
(`agent_sessions.current_mode_id`, `current_model_id`,
`current_thought_level_id`, from `SessionMetadata`: the acknowledgment at
new/load/switch or the agent's own `config_option_update`); a historical run's
request proves nothing about the live session. A run that requests none of
them keeps the confirmed ones on restore or fresh fallback
(`resolve_conversation_session`); a run that picks a model keeps no level
from the old one, and a different model confirmed without a level clears the
stored one (`RunRepository::set_session_model`). `EngineCapability.liveModeChange` (Claude)
marks adapters whose change reaches the running turn; codex-acp reads its mode
at turn start. A mode change never answers or clears a pending permission card.

### Always Allow Tool Actions

The broker auto-answers before asking, and before the client-support check,
when: the agent has no bypass mode (`permission_bypass_mode`; OpenCode and
Oh-My-Pi qualify, Claude `bypassPermissions` and Codex `agent-full-access` do
not), the owning conversation's `always_allow_tool_actions` is on (subagents
use the parent's), and the request offers `allow_once` (never `allow_always`)
and is not a plan approval. Elicitation still asks. A plan is a permission
whose tool call carries `rawInput.plan` and whose `kind` is `switch_mode`
(Codex) or absent (Claude sends an AIR client none); `plan_markdown` decides.
At codex-acp 2.1.1 / claude-agent-acp 0.85.1: Codex also streams the plan as
text; Claude's "keep planning" ends the turn cancelled, keeping Plan mode.

## Model Catalog Freshness

Models, modes, thought levels and image support exist only in `session/new`,
so core probes and caches (`model-catalog.json`, no version field: an absent
`Option` on `CatalogEntry` means re-probe). A probe can be stale on arrival
(Claude refreshes its model config in the background), so live `session/new`
and `session/load` observations beat probe data for the TTL while the pin
matches; after expiry or a pin change a probe replaces, never unions. Thought
levels belong to the model: kept per model as observed live (`session/new` and
a model switch's answer), newest wins, dropped when the pin changes or the
model leaves. A loaded session's level is a past pick, not a default:
`session/load` records no per-model levels, and engine-wide selects only after
a `session/new` in the same adapter generation, keeping that one's thought
defaults. `engine.changed` is a hint;
clients re-read `engine.list`. Core never emits it for a refreshed but
unchanged entry: every `engine.list` folds the live answer into the cache, so
each poll would trigger another.

The probe opens one session in a throwaway adapter and must leave no vendor
history. What a bare `session/new` stores differs per adapter, so each
definition declares its cleanup beside the pin (`ProbeCleanup`,
`agent/catalog_probe.rs`); it rests on adapter internals, so recheck it with
`scripts/check-probe-leftovers.py` at every pin bump. A failed cleanup logs
"model catalog probe left a session behind" and still records the catalog.
CLI-setup homes are probed too (decided 2026-11-13: onboarding needs their
models): the throwaway process runs the user's settings and hooks, is never
stopped while the home's sign-in refresh lock is held, and counts as a live
child for Quit; one per home waits at a time. Recheck on a CLI setup re-probes
whatever the catalog's age, since one already signed in never signs in
through Ottam and its own terminal can change what it offers.
ACP's unstable `session/options` replaces the probe for an agent that
advertises `sessionCapabilities.options`.

## Runtime Probe Budget

A slow Node probe costs its own row a verdict, never the list: `engine.list`
and `agent.list` answer with `checking` once the budget is spent, and core
emits one `engine.changed` per harness when that probe lands (the blocking work
cannot be cancelled, so its answer warms the cache for the re-read). `checking`
is not a problem state: the engine stays selectable, because a prompt waits on
the same probe in `require_runtime`, and disabling it would move a cold launch
off the user's engine. Real probe errors still fail the list
(`RuntimeProbeSweep`).

## AIR Tool-Call Contract

Core declares `clientCapabilities._meta.jetbrains.air` v1. Both pinned
adapters omit unchanged fields and `_meta` keys from tool-call updates, so
transcript assembly merges updates and a fact sent once stays known. Command
output arrives only as `_meta.terminal_output[_delta]` with
`_meta.terminal_exit`; edits as `diff` blocks (Claude's `rawInput` has no
old/new strings); Codex phase as `_meta.jetbrains.air.phase`.

Subagents: core declares AIR `nativeSubagentSessions` and
`clientCapabilities.subagents` by hand (`protocol::initialize_wire`) because
the schema hides them behind `unstable_subagents`, which core leaves off.
Without native sessions Claude withholds subagent text and thoughts. A child
binds to the parent's run and threads its transcript; a nested subagent
inherits its parent subagent's run; a child's non-content updates never touch
the parent; a subagent's end arms the parent's follow-up. `SendMessage` serves
Claude's own subagents only; delegated conversations use
`message_child`/`message_parent` ([MCP › Delegation](mcp.md#delegation)). At
the pins, Claude forwards no work of a resumed run; codex-acp forwards neither
a follow-up to a finished subagent nor collaboration text.

### Background Tasks

Both adapters report `async_task_*` (shapes in
`agent/fixtures/async-tasks.json`). Codex's `asyncTaskId` is the tool call id,
Claude's the SDK task id; Claude also reports a self-started turn as
`taskType: "turn"` with no `toolCallId`. Lifecycle goes to the run that
started the task, even after it ends, and only core declares a task `lost`;
Claude's unprompted answer lands in a follow-up run `origin: autonomous`. See
[attribution](acp-attribution.md#attribution-and-lifetime).

## Steering Extension

`_session/steering` is advertised as `initialize._meta.steering.supported`;
silence means interrupt-and-continue. Adapters disagree about the idle case,
so core keys on the outcome: `injected` (all) joined the turn;
`promptRequired` (claude-agent-acp, omp) message untouched, still the host's;
`startedNewTurn` (codex-acp) a detached turn holds the message;
`failed`/unknown (codex-acp) delivery unknowable. claude honours
`_meta.steering.idleBehavior = "promptRequired"`; codex ignores it, so core's
local prompting check is load-bearing: without it an idle codex session
answers a steer with a detached turn. Known-undelivered (`promptRequired`,
settled session, `MethodNotFound`) is resent the ordinary way; unknown
(`startedNewTurn`, `failed`, dropped reply, dead adapter) is the user's call,
since core's cancel of the detached turn races what it already ran.
See [Stop And Steer](persistence.md#stop-and-steer).
