---
name: core-inspect
description: Query a ottam-core headlessly over JSONL — attach to the app's running core or spawn a scratch one. Use for backend state questions (agents, installs, workspaces, git) without driving the UI.
---

# Core Inspect

Use this skill to answer backend questions directly against ottam-core:
live agent/adapter/install state, workspace and conversation listings, git
snapshots, protocol behavior. One blessed entry point — do not hand-roll
JSONL clients or Python pipes:

```sh
./scripts/inspect-core.sh <command>
```

## Attach or spawn?

**Attach** (the default) connects to the core the running app owns, via
`$OTTAM_HOME/core.sock`, or `/tmp/ottam-<uid>/<hash>.sock` when that
path is too long for a socket (worktree instances; `status` prints which).
It sees the app's real, live state — this is
the mode for "what does the app think right now?" questions.

**`--spawn`** starts a fresh core against a scratch temp home. Use it when
no app is running, when you need a mutation you don't want touching real
data, or for protocol experiments. `--copy-agents` clones the real home's
installed adapters into the scratch home so install-state questions answer
like the real home would.

Safety: attach talks to **real data**. The socket exposes the full method
surface, so prefer read-only methods (`*.list`, `*.get`, `status`) when
attached; do mutations against `--spawn`.

## Commands

```sh
./scripts/inspect-core.sh status                        # is a core running, which
./scripts/inspect-core.sh call agent.list '{}'          # one request, pretty result
./scripts/inspect-core.sh call workspace.list --spawn   # same, on a scratch core
./scripts/inspect-core.sh conversation <id>             # conversation.get, workspace resolved
./scripts/inspect-core.sh conversation <id> listEvents '{}'   # any conversation.* method
./scripts/inspect-core.sh listen --methods conversation.,agent.   # notifications as JSONL until ^C
./scripts/inspect-core.sh repl                          # raw pipe: JSONL in, everything out
```

Flags: `--home <path>` (inspect a specific home), `--timeout <secs>`,
`--raw` (full response envelope), and for `--spawn`: `--core-bin`,
`--keep-home`, `--copy-agents`.

Home resolution: `--home` > `$OTTAM_HOME` > this worktree's running
instance (`.build/run-app/*/owner.json`, live pid wins) > `~/.ottam`.

## Debugging a conversation by ID

The sidebar's **Copy Conversation ID** gives a UUIDv7 that is unique across
workspaces. When the user says "look at ottam `<id>`", start with
`conversation <id>`. It finds the owning workspace and injects `workspaceID`
and `conversationID`, so pass only the method's other params. Bare method
names expand to `conversation.<name>`. Useful follow-ups: `listAgentSessions`,
`listRuns`, `listEvents`, `listMessages`, `listTranscript`.

If content is missing or a turn looks wrong, check whether core dropped
adapter traffic before digging further: grep the backend log for
`Unhandled agent traffic` from that agent (command and labels under
`AGENTS.md` Runtime Logs).

`listen` shows what core published, not what the agent sent: a run's text
arrives as merged `conversation.transcriptDelta` appends. To see raw adapter
frames (chunk sizes and timing, session IDs and
sequences, updates dropped before the transcript), relaunch with the ACP wire
log, reproduce, read `.build/run-app/<instance>/logs`, then delete those logs:

```sh
OTTAM_ACP_WIRE_LOG=1 OTTAM_LOG=ottam_core=info,acp.agent.wire=trace,warn ./scripts/run-app.sh
```

Method names and parameter shapes live in `backend/crates/ottam-protocol/src/`
(`METHOD_*` constants and `*Params` types, one module per area).

## Extending the tool

`backend/crates/ottam-inspect` may grow when inspection genuinely needs
it — but **stop and ask for approval before adding anything**. Present a
concise plan: what you'd add, and why the existing surface can't do it
(what warrants net-new code?). Most gaps are not gaps: arbitrary methods go
through `call`, scripted multi-request sessions through `repl` with piped
stdin, event capture through `listen`, and filtering/joining belongs in
`jq`, not the crate.

## Troubleshooting

- **"no core is running"** — the app isn't running, or its core predates
  the attach transport (relaunch via `./scripts/run-app.sh`), or the socket
  is disabled (`OTTAM_ATTACH=off`).
- **Timeouts on attach** — requests share the app's serial dispatcher;
  a busy core answers slowly. Raise `--timeout` or use `--spawn`.
- **Spawn can't find the core binary** — the wrapper builds it; if calling
  the binary directly, pass `--core-bin` or set `$OTTAM_CORE_PATH`.
