---
kind: architecture
status: active
---

# Background Task Attribution

How core routes an agent's updates to runs once background tasks and
autonomous follow-ups exist. The wire contract is in
[ACP](acp.md#background-tasks). Code in `server/agents/attribution.rs`,
`task_index.rs`, and `conversation_runner/autonomous.rs` cites these rules by
number, so keep the headings and numbering stable. Paths are under
`backend/crates/ottam-core/src/`.

## Attribution And Lifetime

### Rules

1. **The forwarder owns attribution.** A fence channel beside the update
   channel carries ordered operations: `Claim`, `Release`, `Forget`,
   `MarkReplay`, `AbandonReplay`, `SteerPending`, `SteerSettled`,
   `MarkDetached`, and a plain flush. Each drains the updates queued at its
   fence, then applies. A claim therefore follows every update queued
   before it, and a run's `Release` fences its own output. Callers wait for
   the acknowledgment, except drop guards (which post `AbandonReplay` and
   `SteerSettled`) and a steer answered with a detached turn (which posts
   `MarkDetached`). A native steer waits for `SteerPending` before it
   sends, holding the steering lock. `PromptSettled` travels in the update
   channel itself. With no forwarder, or one replaced before it
   acknowledges, the caller applies the operation itself; operations
   compare run ids, so a second application changes nothing. A posted
   operation is lost if its forwarder is replaced first. That happens only
   on adapter restart or exit, and the next claim then sweeps the older
   generation, so the loss changes at most how a stale update is labeled. A
   claim made inside the forwarder (a follow-up run) applies directly and
   never waits on itself. Never wait on an operation while holding
   `active_runs`. The forwarder never takes the steering lock.
2. **Key states.** Three, each recording the adapter generation it was set
   under.

   | State | Set by | Ends at |
   |---|---|---|
   | `Running(run)` | `Claim` | the run's `Release`, or `Forget` when its session is gone or it never prompted |
   | `Replaying(run)` | `MarkReplay` in `resolve_conversation_session`, before `session/load` (the only load site) | the run's `Claim`, or `AbandonReplay` from a drop guard on cancel, abort, or a failed load |
   | `Released(tombstone)` | `Release` at run end, or `AbandonReplay` | the next `Claim`, an adapter restart, conversation delete, project removal |

   The tombstone holds the run and how it ended: completed, cancelled,
   failed, detached steer, or abandoned replay. The run's tool calls live in
   the task index (rule 3). A detached ending never arms a follow-up.

   A `Running` key also records whether the agent has answered the run's
   prompt (`PromptSettled`) and whether a native steer is in flight
   (`SteerPending` until `SteerSettled`). A steer that reaches a finished
   turn gets `startedNewTurn`: codex-acp starts a turn for it once the
   prompt's answer is out, and that turn can stream before the steer is
   answered. So an answered prompt plus a steer in flight drops updates as
   `detached_steer`; `MarkDetached` keeps them dropping after the steer is
   answered, and the run's release records that ending. An answered prompt
   alone changes nothing, so late titles and Codex's post-`end_turn` stamp
   still reach the run. The ACP actor publishes the marker from the SDK's
   ordered response callback, so it falls after the turn's updates and
   before anything the agent sends after its answer; no client sees it.
   Restored replay has no marker (rule 10). The marker shares the update
   channel's bounded buffer. The actor also records the marker's sequence,
   and a forwarder that sees a gap covering it applies `PromptSettled`
   before the next update.

   Release compares the run id. The retry path's stale key and a storage
   failure after binding `Forget` instead, leaving no tombstone. A
   tombstone from an earlier generation is discarded at lookup, and a
   `Claim` in a newer generation sweeps the agent's older ones; a `Running`
   key is ended only by its own run.

   An agent can announce a session before `session/new` answers, so from
   just before any `session/new` until a run claims that session, a key with
   no state is not conversation traffic: neither recorded nor counted.
3. **Ownership routes lifecycle.** The task index maps `asyncTaskId` and
   `toolCallId` to the run that owns them. The forwarder indexes every
   `ToolCall` it routes to a `Running` run whose output is its own, so a
   detached steer turn's calls never index. `async_task_*` and a
   `ToolCallUpdate` for an indexed id go to the owning run, whoever holds
   the key, and never open a run. This carries Codex's post-`end_turn`
   `backgrounded` stamp, spawn, and terminal state to the run that made the
   call, and keeps run N's tasks updating run N after run N+1 claims the
   session. A write to any owner other than the key's holder logs its error
   and never fails the holding run. Entries record session and adapter
   generation; ACP ids are unique only per session. A lookup in a newer
   generation misses, a `Replaying` key or a raw or opening session
   bypasses the index (rule 10), and an absent key falls back to an
   agent-wide lookup that takes the newest match. A Claude spawn without
   `toolCallId` is held for the run whose output it is (the holder, or the
   tombstone's run if it completed, failed, or was cancelled), coalesced to
   the latest state and `outputFilePath`, and replayed as one frame when a
   frame names the call. A stamp, spawn, or progress starts a task only on
   a tool that shows none; a state sets it; loss ends it. A call id another
   run reuses in the session is that run's. A run's ended entries survive
   one claim of the session by another user run; the next user claim, or a
   claim in a newer generation, removes them. An autonomous run's claim
   does not count toward that grace. Live entries survive every claim and
   project removal; conversation delete removes all of its entries. Each
   agent's entries are capped, evicting its oldest ended one; a full
   index of live tasks indexes nothing more.
4. **Closed runs take lifecycle only.** `TranscriptCoordinator::update`
   decides open or closed under the conversation lock that `finish` takes.
   A closed run takes `async_task_*` and a `ToolCallUpdate` carrying the
   `backgrounded` stamp (only the stamp); anything else, or a frame naming
   no tool of the run, counts as `closed_run`. While memory holds the tool,
   the revision is made in memory and the checkpoint writer persists it.
   Once the tool or conversation is evicted, or storage reports the run
   finished, it is revised in storage (`revise_stored`) under the
   checkpoint writer's lock as a metadata-only save. The live set takes the
   change before the commit. The delta takes the conversation's next
   sequence, or storage's when memory no longer holds it. Either way only
   `tool.backgroundTask` changes, the item revision bumps, and a transcript
   delta is published.
5. **Armed.** A `Released` key is armed while its session has a live task,
   or for `ARMED_GRACE` after any task event. It is computed at
   lookup, with no timers, and a user claim ends it. A live child (rule 9)
   counts as a live task.
6. **Routing.**

   | Key state | Lifecycle: `async_task_*`, indexed `ToolCallUpdate` | Content: chunks, `ToolCall`, unindexed `ToolCallUpdate`, plan, permission | Metadata: signals, title, usage, mode, commands |
   |---|---|---|---|
   | `Running` | owning run | the run | the run |
   | `Running`, detached | owning run | drop `detached_steer` | drop, debug |
   | `Replaying` | discard, silent | discard, silent | discard, silent |
   | `Released`, armed | owning run | open a follow-up run | drop, debug |
   | `Released`, not armed | owning run | drop by how the run ended: `after_end`, `after_cancel`, `detached_steer`, or silent `replay` | drop, debug |
   | absent while the agent opens one | not conversation traffic | not conversation traffic | not conversation traffic |
   | absent | agent-wide lookup, else drop `unattributed` | drop `unattributed` | drop, debug |

   When the conversation already has an active run, the follow-up cannot
   open yet. The forwarder holds that key's content, in order, in a buffer
   bounded by frames (after merging one message's chunks), bytes, and age.
   When the blocking run leaves `active_runs`
   (`remove_conversation_run`, which every completion path reaches), the
   follow-up opens and drains the buffer. This covers the releasing run's
   own window, since its `Release` precedes finalization. Overflow drops as
   `conversation_busy`. Every drop increments a bounded counter keyed by
   agent and reason (`crate::diagnostics`); `closed_run` is keyed by the
   transcript. `after_end`, `unattributed`, and `closed_run` content warns
   at powers of two; `after_cancel`, `detached_steer`, and metadata log at
   debug; replay is silent.
7. **Task loss.** Only core declares a task `lost`; the decoder refuses it
   from an adapter. A live task is written `lost` through rule 4 when its
   adapter exits (`Stopped` or `Dead`) or restarts, which the forwarder sees
   on the adapter's status after draining queued updates, or when its
   session is gone (the next run finds it not live, or the retry path
   forgets a stale key). Loss writes run on their own task, sets only the
   state, and never overwrites a terminal one. An engine change keeps the
   index. Core shutdown writes nothing; it clears the key states and the
   index. A stored `running` or `paused` task that this core instance never
   saw live reads as `lost` in `conversation.listTranscript`; there is no
   startup scan.

   Each run's live count rides the transcript stream as `backgroundTasks:
   [{runID, live}]`. A page lists the runs whose tasks this instance has
   seen, zeros included; a delta lists the runs whose count that write
   moved. A page freezes its counts with its live overlay, before it reads
   storage, so the delta that follows carries a newer count.
8. **Permissions.** For a key that is not `Running`, the broker sends a
   flush, then resolves. Armed: it opens or joins the follow-up run,
   single-flight with the forwarder's opener, with a bounded wait, because
   the SDK runs request handlers inside its dispatch loop. Otherwise the
   request goes through unattributed and counts as `conversation_busy`. A
   follow-up never closes with a pending permission or an open tool call.
9. **Child keys.** A `subagent_spawned` adds a child key bound to the run
   its parent key routes content to at that moment (rule 6), or to the
   tombstone's run when the parent key is released. The binding never moves
   to a later user run. While the bound run is `Running`, the child's
   updates go there; once it closes, a live child counts as a live task for
   it (rule 5), and the child's content opens or joins a follow-up run,
   which takes over the binding. Nested children bind through their
   parent's child key. A resumed `<id>:generation:<n>` is a new child key.
   A child announced while its root replays, or by a replay child, drops as
   `replay` (rule 10). Stop marks the cancelled run's children ended. Child
   updates take the root's key and the child's thread (`attribution.rs`,
   `native_subagents.rs`), and a permission request on a child key carries
   its thread and name.
10. **Replay keeps nothing.** Lifecycle updates inside a `session/load`
    replay are discarded with the rest, including codex-acp's background
    terminal reconcile and the subagents a replay announces. Threads
    persisted live stay authoritative
    ([subagent sessions](../../specs/backend/acp/subagent-sessions.md#decisions)).
    Core loads only sessions the adapter lost, usually along with their
    terminals.
11. **Accepted gaps.**
    - Content held behind a user run (rule 6) lands in a follow-up after
      it, so the transcript can show it after the user's turn.
    - A child's update that arrives while a later user run holds its root
      drops as `after_end`; a child's state update on a closed run is
      refused as `closed_run`.
    - A timed-out availability wait still claims, with a warning.
      Late titles are not adopted outside a run.
    - Continuations with no spawn (Claude's peer, coordinator, and observer
      origins) drop as `after_end`; the counter shows whether arming should
      widen.
    - A session's own announcements before its run claims it are not
      recorded. While an agent opens a session, a stray update for some
      other unknown session is not counted.
    - A steer injected into a turn that ends before core reads the steer's
      answer drops that turn's post-answer metadata as `detached_steer`.
    - A terminal frame lost to a lagging forwarder leaves its task live
      until its adapter or session ends; it then reads `lost`.
    - A storage revision (rule 4) waits for the checkpoint writer inline,
      so the forwarder can stall behind one checkpoint write.
    - A Claude spawn that never names its tool call is lost.
    - A closed run's in-memory revision evicted before a checkpoint saves
      it is gone, so a task it ended reads `lost` after a reload.
    - A failed loss or terminal write leaves its task in this instance's
      live set until core restarts.
    - A user message sent while an autonomous run streams preempts that run
      at once, but claude-agent-acp only queues the prompt in the CLI, so
      the rest of the agent's own cycle lands in the user's run.

### Follow-Up Runs

- An autonomous run is created `running`, with no input, already bound to
  the continued run's session (`RunRepository::create_autonomous_run`,
  `conversation_runner/autonomous.rs`).
- It closes on the earliest of: a settle window after Claude's
  autonomous-origin `usage_update` (`_meta["_claude/origin"]`), an idle gap
  with no open tool call or pending permission, or the next user prompt.
  Placeholder results can close it early; that yields a second follow-up,
  not a loss.
- Cancelling it sends `session/cancel` through
  `HarnessAdapter::cancel_out_of_turn`, only while no prompt is in flight,
  with a bounded wait, and disarms the key until the next user run.
- A user start or steer preempts it under the per-conversation launch lock
  and ends it `completed`; see
  [persistence](persistence.md#stop-and-steer).

## Deliberate Absences

- OpenCode and oh-my-pi send no `async_task_*`. Ottam's OpenCode plugin
  strips `background`, and omp holds late results for the next prompt.
  Revisit when an upstream starts forwarding its resumed turns.

## Decisions

- 2026-09-13: background tasks are tool-item metadata plus a run-level live
  count, not a new transcript item kind, because the tool card is the
  user-facing anchor.
- 2026-09-13: follow-up work is its own run (`origin: autonomous`), not an
  extension of the completed run. Reopening a completed run would let a
  restored session's replay be misattributed.
- 2026-10-05: lifecycle routes by ownership and only content reads the
  key's state. A timed pending state would have needed a linger for
  Codex's post-`end_turn` frames.
- 2026-10-05: a follow-up that cannot open waits for the blocking run
  instead of dropping, because a continuation that arrives at once always
  meets its own run still active.
- 2026-10-05: child keys bind to the run they spawned in, not the root
  key's current holder, so a surviving child never writes into a later,
  unrelated user run.
- 2026-10-05: replay lifecycle is discarded and core sends no resync;
  codex-acp reconciles its own background terminals on `session/load`.
- 2026-10-06: three key states, not five. `Detached` became a tombstone
  ending plus a flag.
- 2026-10-07: the live count rides the transcript stream, not
  `ConversationRunSummary`, because counts change after a run closes, when
  nothing republishes its summary.
- 2026-10-08: a per-conversation launch lock, not one storage transaction,
  makes preemption atomic. Ending the run inside `begin_run` would skip its
  `Release` fence. The forwarder never takes the lock.
- 2026-10-08: a preempted autonomous run ends `completed`, not `canceled`:
  it gave way. A steer against one is a preempting start.
- 2026-10-08: Claude acknowledges a stop with an out-of-turn chunk, which
  opens a follow-up that closes after the idle gap. Advertising `notices`
  would turn it into a notice instead.
