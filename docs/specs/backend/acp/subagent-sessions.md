---
kind: spec
status: active
---

# Subagent Sessions

## Current Status

Shipped as [ACP work plan](README.md) M2 and M3; the contract is
[acp.md](../../../architecture/backend/acp.md) § Subagent Sessions and
[persistence](../../../architecture/backend/persistence.md) § Transcript
Ownership And Order. What remains is follow-up work on upstream's merged
shape and on history core never saw live.

Adapter claims come from the installed bundles under
`$OTTAM_HOME/agents/<agent>/adapter/<pin>/`. Re-check them at each pin bump
against the recordings in `agent/fixtures/`
(`claude-native-subagents-0.85.1.json`, `codex-native-subagents-2.1.1.json`,
`omp-subagent-sessions-pr15043.json`).

## Adapter Contract

Adapter behavior the architecture doc does not record:

- `subagent_state_update.state` is `completed | failed | cancelled |
  disconnected`. Child ids are Claude's task id and Codex's thread id.
  Adapters buffer child updates until the spawn.
- Claude holds the prompt while its children are live. Codex waits up to
  10 minutes, then marks them `failed`.
- `session/load`: Claude replays children on the root under
  `<root>:replay-subagent:<toolUseId>` ids that differ from the live ones,
  and has replayed a cancelled child as `completed`. Codex reuses live
  thread ids. Replay states are not trusted.
- Claude resumes a finished subagent through the parent's `SendMessage
  { to, message }`; a `SendMessage` to a running subagent starts no new run.
  Claude Code sometimes has a subagent report through `SubagentHandback
  { message }` instead of its last message; the server chooses this, not
  the CLI.
- Upstream's merged shape (`subagent_update` upsert plus
  `session_message[_chunk]`, ACP `unstable_subagents`) is decoded by
  `agent/acp/subagent_sessions.rs`. No released adapter sends it yet;
  oh-my-pi will once oh-my-pi#15043 ships.

## Next

- Let non-terminal `subagent_update` states refresh the thread (a woken
  child, `requires_action`), and show later `session_message`s. Today only
  announce, assignment, and end are handled.
- Drop the AIR `nativeSubagentSessions` flag once no pin needs it; at the
  pins it gates the same path as `clientCapabilities.subagents`.
- Replace the hand-built omp fixture with a recording once oh-my-pi#15043
  ships.
- Import history core never saw live (conversations from before M3, or a
  child session core never saw): replay option C in Decisions. Trigger:
  imported or external sessions.

## Backlog

- Adapter subagent gaps, deliberately not chased (2026-10-11), because
  Ottam's own delegation ([Ottam as an MCP server](../mcp/ottam-tools.md))
  carries every message itself. Trigger: one starts to matter to users.
  Drafted issues, not filed:
  1. claude-agent-acp: a subagent resumed by `SendMessage` forwards none of
     its work, only its `prompt` and terminal state (0.85.1, 0.89.1). A run
     resumed by a background task's `<task-notification>` does forward its
     work.
  2. codex-acp: a follow-up to a finished subagent brings no spawn and no
     child frames. A follow-up to a running child forwards its work but
     hides the message text (2.1.1, 2.2.2).
  3. codex-acp: a subagent's `send_message` to its parent is not forwarded
     (2.2.2).
- The thread filter and the follow-up lookup read
  `json_extract(adapter_metadata_json, '$.subagentThreadID')` with no
  expression index. Trigger: thread pages or follow-up lookups show up in
  transcript timing for long conversations; then add expression indexes in
  a migration.

## Decisions

- 2026-09-13: child-thread rendering is built once and fed by every source
  (Claude legacy parent ids, native sessions, OpenCode's tagged child
  sessions). The UI never distinguishes them.
- 2026-10-09: a thread is rows inside the parent conversation, not a
  conversation of its own, so cancel, permissions, attribution, and
  `session/load` keep one path.
- 2026-10-09: a legacy asynchronous Claude thread's state follows its run,
  not its control call, which completes at launch.
- 2026-10-10: replay keeps nothing (option A). Threads persisted live stay
  authoritative. Rejected: B, reconciling replay into stored threads, which
  risks duplicates because Claude's replay ids don't map to live ones. C,
  importing only unmatched threads, is in Next.
- 2026-10-11: a native subagent's state is its own, not its run's. It may
  outlive the run it spawned in; only Stop or a lost adapter ends it from
  core's side.
- 2026-10-10: a native thread's record is a synthetic anchor tool item, not
  a table: no migration, and the tool revision path carries state.
- 2026-10-11: one thread per subagent. Each run, resumes included, gets its
  own anchor in that thread, carrying the parent's message as `prompt`.
- 2026-10-11: messages an adapter forwards only as a call's input are named
  per harness in the registry (`AgentDefinition.subagent_messages`), not
  special-cased in the runner.
- 2026-10-10: a subagent opens in one reusable, read-only tools-pane tab, a
  view over the parent's rows. It reuses the main transcript renderer
  without a composer.
- 2026-10-15: the merged `subagent_update` shape folds onto the draft's two
  kinds: a child is announced once and ended once, on its first `idle`.
  Rejected: an omp-only shim parsing `rawOutput.details.progress[]`.
