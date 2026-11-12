---
kind: spec
status: active
---

# Context Compaction

When an agent compacts its context, the chat should say so: a transcript row
showing when it happened, how much it saved, and the agent's summary where
the agent sends one. Today Ottam shows Claude's and Codex's compactions
as a "Compact conversation" tool card. It shows OpenCode's and Oh-My-Pi's
not at all.

This is [M10 of the ACP work plan](README.md#m10-see-when-an-agent-compacts-its-context).

## Current Status

Not started; its Try it in the work plan awaits approval. The wire evidence
below was recorded at the current pins (Claude 0.100.0, Codex 2.5.0,
OpenCode 2.2.5, Oh-My-Pi 18.16.0) with a direct ACP client sending Ottam's
`initialize`, once as sent today and once with
`clientCapabilities.session.compaction: {}` added. Oh-My-Pi forwards only
automatic compaction, so its threshold was lowered to force one.

ACP 1.12 (SDK 3.5, pinned) defines compaction as stable, not behind an SDK
feature:

- `compaction_update` carries `compactionId` and `status` (`in_progress`,
  `completed`, `failed` or `cancelled`), plus `summary`, `error` and
  `_meta`. Those three are patches: an omitted field is left unchanged,
  and `null` clears it.
- `compaction_summary_chunk` carries `compactionId` and `content`.
- Agents send either only when the client advertises `session.compaction`.

Core does not advertise it. The `SessionCompaction` row in
`agent/acp/features.rs` still says `Unstable { feature:
"unstable_session_compaction" }` and is `Unsupported`, and the doc comment
on `agent/acp/protocol.rs` says compaction sits behind an SDK feature. Both
are stale.

| Agent | Today (no capability) | With `session.compaction` |
|---|---|---|
| Claude 0.100.0 | `tool_call` "Compact conversation", `kind: think`. The update carries `_meta.jetbrains.air.contextCompaction` with `trigger`, `preTokens`, `postTokens` and `durationMs`. | `compaction_update` `in_progress`, then `completed` with `summary`, then a second `completed` patch with the same AIR meta (24,651 → 6,329 tokens). |
| Codex 2.5.0 | The same `tool_call`, with AIR meta `{version}` only. | `compaction_update` `in_progress`, then `completed`. No summary and no tokens. |
| OpenCode 2.2.5 | `session_info_update` with `_meta["opencode/compaction"]` set to `started`, then `completed`. Nothing is shown. | `compaction_update` `in_progress`, 548 `compaction_summary_chunk`s, then `completed` with the full `summary` as markdown. |
| Oh-My-Pi 18.16.0 | `_omp/compaction/start` and `_omp/compaction/end`. Core does not claim them, so they log as unhandled agent traffic. | Same as today. The pinned binary contains no `compaction_update`. A manual `/compact` comes back only as agent text. |

Oh-My-Pi's notifications cover only automatic maintenance, and they can
arrive outside a prompt (oh-my-pi#17384):

- `start` is `{sessionId, reason, action}`.
- `end` is `{sessionId, action, aborted, willRetry, skipped?, errorMessage?, tokensBefore?, tokensAfter?}`.

One compaction is a chain of attempts in preference order (snapcompact,
handoff, shake, context-full), and each attempt is its own start/end pair.
An attempt that hands over to the next ends with `errorMessage` "…trying
the next preferred compaction method.", sometimes with `skipped: true`. A
success has no error and carries `tokensBefore`. Nothing ties the attempts
of one chain together; there is no pass id.

Items are `cmp:<n>`. Items 1–4 ship as one PR, core and app together. The
fixtures in item 1 are the regression evidence; re-record them when a pin
bump changes compaction.

## Active

1. [ ] **Core: advertise compaction and map the standard updates.**
   - Context: Claude, Codex and OpenCode already implement the standard
     updates and wait only for the capability. Advertising it also stops
     Claude and Codex from sending the fake tool card.
   - Scope:
     - Advertise `session.compaction: {}`. Mark its `CLIENT_FEATURES` row
       Supported and its SDK status stable, and fix the `protocol.rs`
       comment.
     - Add a `HarnessUpdateKind::Compaction` carrying id, status,
       summary, error and tokens before/after.
     - Apply patch semantics per id, and append summary chunks to a live
       summary that a later `summary` replaces.
     - Read Claude's tokens from `_meta.jetbrains.air.contextCompaction`.
     - Commit the scrubbed recordings as fixtures, and claim both update
       kinds in `unhandled_nothing_in_the_recorded_sessions`.
   - Done when: each fixture decodes to one compaction with the right
     status, summary and tokens, and nothing in them is unhandled.
   - Verify with: `./scripts/test-rust-core.sh --lib --filter agent::acp`.
   - Source: the table above; `agent/acp/features.rs`, `agent/types.rs`
     (`HarnessUpdateKind`).

2. [ ] **Core: translate Oh-My-Pi's `_omp/compaction/*`.**
   - Context: Oh-My-Pi will not send the standard updates, and it sends
     one start/end pair per attempt, with no pass id.
   - Scope:
     - Claim both methods.
     - Open a pass on the first `start` with no pass already open for that
       session. Give it a core-made id.
     - Keep the pass open while each `end` hands over to the next method.
     - Complete the pass on an `end` with no error. Its `tokensBefore`
       becomes the before figure.
     - Cancel on `aborted`. Fail on any other final error.
     - Drop a pass whose every attempt was skipped or "would not reduce
       context". It changed nothing.
     - Attach the pass to the session's open run if there is one;
       otherwise between runs.
   - Done when: the recorded threshold run yields one completed compaction
     per chain and nothing for the no-op chains.
   - Verify with: a fixture test over `omp-compaction` frames.
   - Source: Current Status. The hand-over test matches the error text,
     which is the fragile part; see Decisions.

3. [ ] **Protocol and persistence: a `compaction` transcript item.**
   - Scope:
     - Add `TranscriptItemKind::Compaction` with status and token
       counts.
     - Store the summary in the payload store, the way reasoning is
       stored, so it pages like other payloads.
     - An in-progress item is `Open`. One still open when its run ends
       becomes `Partial`.
     - Regenerate the protocol artifacts. Update
       [persistence](../../../architecture/backend/persistence.md) and
       [ACP](../../../architecture/backend/acp.md).
   - Done when: a compaction survives a relaunch with its summary.
   - Verify with: `./scripts/check-protocol-alignment.sh` and
     `./scripts/test-rust-core.sh --test chat_storage`.

4. [ ] **App: the transcript row and the gauge.**
   - Scope:
     - A divider-style row, "Context compacted · 24.7K → 6.3K tokens".
       Leave out the counts the agent doesn't send.
     - While in progress, the row reads "Compacting context…" with a
       spinner. On failure it shows the agent's error.
     - A disclosure shows the summary as markdown, where there is one.
     - When an after-count arrives, set the gauge's `used` right away;
       otherwise the next `usage_update` sets it.
     - Old chats keep their "Compact conversation" tool cards.
   - Done when: the Try it passes in the real window.
   - Verify with: `./scripts/test-app.sh --filter Transcript` and a new
     `chat.compaction` UI scenario on the test agent.

## Next

5. [ ] **Ask Oh-My-Pi upstream for the standard updates**, or at least a
   pass id on `_omp/compaction/*`. Either one retires item 2's heuristic.
   Ask the user before posting.

The draft Try it is in the
[work plan](README.md#m10-see-when-an-agent-compacts-its-context).

## Decisions

- 2026-11-03: Advertise the standard capability rather than parsing each
  agent's fallback (AIR tool card, `opencode/compaction` meta). Three of
  the four agents implement it, and the fallbacks carry less.
- 2026-11-03: Oh-My-Pi is translated in core, not left as unhandled
  traffic. The error-text match is accepted until upstream sends a pass id
  (item 5). A wording change upstream degrades to one row per attempt, not
  to a crash.
- 2026-11-03: No separate "Compacting…" trailer from the activity signal.
  The row's own in-progress state covers it.
