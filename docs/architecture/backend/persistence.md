---
kind: architecture
status: active
---

# Ottam Persistence

Decisions, deliberate absences and ownership for Ottam's durable storage;
the code under `ottam-core/src/storage/` and `server/chat/` shows the rest.
Terminals are process-lifetime objects and are **not** persisted
([Terminal Architecture](../terminal/README.md)).

Two SQLite databases under `OTTAM_HOME`, never per Workspace or Project:
`ottam.sqlite` is the authority for domain and transcript records;
`payloads.sqlite` holds captured bytes, of which only classified tool results
are an evictable cache. Attachment bytes are files under
`workspaces/<workspace-id>/attachments/`. One Core per `OTTAM_HOME` holds
`core.lock`; a second Core returns a typed already-running error and never
opens the database. WAL with `synchronous = NORMAL` is a deliberate tradeoff:
commits survive app crashes; the last commits may roll back after power loss.

## Ownership

| Store | Owns |
| --- | --- |
| `workspaces`, `workspace_projects`, `projects`, `project_sources` | Identity and membership. IDs are UUIDv7 strings generated in Rust, never derived from a path. |
| `conversations`, `conversation_messages`, `conversation_attachments` | Conversation, user/system Message bodies, bounded attachment metadata. Message order is a projection of the transcript spine; no separate ordering column or allocator. |
| `agent_instances`, `agent_sessions` | Agent profiles and external ACP session identity; the latest mode, commands and context usage are copied onto the active session row so they restore after relaunch. |
| `conversation_runs`, `conversation_run_inputs`, `conversation_run_sources`, `run_events` | Runs, durable input intent and delivery state, immutable per-Run working-root snapshots, bounded lifecycle events. No raw adapter updates or transcript text in `run_events`. |
| `transcript_items` | The single ordering spine: local identity, immutable `(conversation_id, position)`, revision, kind, completion, Run ownership. |
| `transcript_*` (bodies, checkpoints, tools, plans, permissions, payload descriptors, capture, gaps, cursors) | Normalized transcript content. Tool arguments are structured JSON in body chunks, not tool metadata. |
| `core_settings`, `mcp_servers`, `workspace_mcp_disabled`, `named_agents` | Core-wide settings, MCP configuration ([MCP](mcp.md#config-and-secrets)), named agents. |
| `payloads.sqlite` | Immutable compressed chunks for managed tool outputs, file-change inputs and assistant images. No cross-database foreign key; main `transcript_payloads` references protect revisions. |

## Layering

Handler (`server/*.rs`, `ChatService`) → repository → Diesel. No Diesel in
protocol modules or handlers; no SQL in handlers; no protocol types in Diesel
models; no generic repository abstraction; no `rusqlite`. Domain, Diesel row
and protocol models are three separate types; storage never returns Diesel
errors or rows across its boundary. Every multi-row invariant is one
repository transaction; `RunRepository` (`storage/repositories/runs.rs`)
keeps run creation, delivery, interrupt/follow-up, finalization and recovery
together. Diesel's SQLite connection is synchronous: `Database`
(`storage/database.rs`) runs queries through a bounded `spawn_blocking`
gateway and nothing holds a connection, row or transaction across `.await`.
Do not add `diesel-async`: it hides that boundary. Bundled SQLite must be at
least 3.51.3 (WAL reset fix).

An unknown wire enum value never becomes a persisted value. JSON columns hold
open-ended external metadata only, never relationships, status or ordering.
Where a child carries an object ID and its owner's ID, composite keys enforce
the pair in the database, not in Rust; a composite foreign key that cannot
name a parent unique index is a schema-review error, not a reason to drop it.
Every foreign key declares `ON DELETE` per
[Deletion](#deletion-and-history-policy).

## Migrations

- Migration SQL is the source of truth; `schema.rs` is generated and checked
  in. Append-only from the initial baselines; down migrations never run in
  the product. No upgrade from pre-baseline homes; such a home is reset.
- A committed migration never changes, even on an unmerged branch: Diesel
  records only the version, so an edited migration leaves an upgraded home
  drifted from `schema.rs` (`storageUnavailable`). `scripts/check-migrations.sh`
  enforces this; `OTTAM_ALLOW_MIGRATION_EDIT=1` is for an approved reset only.
  One exception, approved 2026-11-10 before the first public release: the
  baseline dropped two never-read columns, and startup drops them from homes
  that ran the earlier baseline (`drop_columns_removed_from_baseline` in
  `storage/database.rs`), because Diesel won't rerun that version.
- The former storage-generation check (a marker that forced a development
  home reset on schema change) was removed on purpose; append-only
  migrations with upgrade tests replace it. Do not reintroduce it.
- A failed migration or corrupt database refuses database-backed methods
  with a typed startup error (never SQL values or user content) and is never
  silently replaced; reset is a user action and restore is manual.

## Transcript Ownership And Order

Ottam owns the transcript locally because adapter `session/load` history is
inconsistent across adapters; it is never rebuilt from adapter replay, and
only evicted tool results use isolated replay ([Payload Storage](#payload-storage)).

- One in-memory `TranscriptCoordinator` (`server/chat/transcript/live.rs`)
  per database orders adapter updates, input admission, permission outcomes
  and Run completion. Admission reserves a position, releases the lock
  before SQLite I/O, then commits Message, RunInput and attachment claim in
  one transaction; a failure publishes no Message and may leave a hole.
- Live delivery never awaits checkpoints. Snapshot reads share a handoff gate
  with Run finalization and interrupt admission, so a snapshot never sees a
  terminal status without its final revisions.
- Contiguous text of one kind and provider message ID revises one item;
  an ID change, tool activity, input or Run completion closes it, so
  `assistant A → steer → assistant B` orders the same live and after restart
  even when the steer is rejected. Capture state is independent of execution
  status: a successful Run stays successful when capture fails.
- Run usage is written NULL-only (first report wins). All-zero usage and a
  cancelled Codex answer's usage (it may be the previous turn's) are not
  recorded; Ottam never invents an account quota from Codex token metadata.
  An AIR error in a terminal prompt response fails the Run even when ACP
  says `end_turn`.
- Subagent threads have no table: legacy Claude threads live in the tool's
  metadata JSON; native subagent sessions get a synthetic anchor tool item
  (`subagent:<child session id>`) at the spawn position and may outlive
  their Run (finalization ends them only on cancel).

### Agent Questions

A question an agent asks ([ACP](acp.md#agent-questions-form-elicitation)) is
recorded as `TranscriptTool.question` on the asking tool call, with no item
kind of its own. Answers keep option labels, never values. Pending questions
are never persisted and nothing is re-asked after relaunch: a Run that ends
with a `pending` record settles it `notAnswered`, in SQL if left by an earlier core.

## Payload Storage

Full assistant and reasoning text stays in SQLite with no aggregate cutoff,
and Ottam never expires Conversation history automatically; retention,
compaction or export needs a reviewed migration and product decision. Tool
output and file-change content use managed payload chunks and never enter
raw event JSON; a descriptor separates upstream truncation from local
capacity loss, and a requested file change does not imply it was applied.
Payload bytes are immutable chunks in `payloads.sqlite`, written by one
shared worker (`Database::payload_worker`).

- A duplicate chunk key is an error, never a content comparison or
  `INSERT OR REPLACE`. Only `staging` revisions accept chunks; `ready` means
  the worker finished capture, not that main published a descriptor.
- There is no cross-database transaction: the worker seals a manifest, then
  the transcript checkpoint publishes the descriptor in main. After restart,
  `staging` revisions are reclaimed; missing data is reported, never
  substituted. `source_path` is display metadata, never a storage path or
  authorization token; RPCs never expose row IDs or database filenames.
- Only explicitly classified tool outputs are evictable; a mixed result with
  image parts is durable as a whole. An evicted result is restored only on
  request, by replaying its session in an isolated ACP adapter (permissions
  denied, no prompts, no transcript publication); Ottam never reruns a tool.
  Replay refuses while the instance has live work or a sign-in, and retires
  an idle Codex adapter first because an idle codex-acp holds the session
  lock.

## Run Input Delivery Contract

Adapter I/O cannot join a SQLite transaction, so delivery is an explicit
state machine on `conversation_run_inputs.status`. The database guarantees
exactly-once input intent (one Message and RunInput per idempotency key;
Retry is a new key and a linked row), not exactly-once external delivery: if
Core exits after adapter acceptance but before the `delivered` commit, SQLite
says `sending` and ACP has no durable receipt. Hence: `sending` is set
atomically before adapter I/O and `delivered` only on adapter-confirmed
acceptance; a definitive rejection is `failed`; a transport error after
`sending` without a negative acknowledgement is `deliveryUnknown`; an answer
meaning nothing was delivered returns it to `pending`. On restart every
surviving `sending` input becomes `deliveryUnknown` before new work is
accepted for that Conversation, and Core never resends it: the user chooses
Retry (may duplicate external work) or Mark Delivered.

### Stop And Steer

Stop cancels the active Run without replacement. Steer persists Message and
pending RunInput first, then routes by adapter capability: native steering
keeps the Run `running`; interrupt-and-continue finalizes the old Run
`interrupted` and reassigns the input to a follow-up Run. A native answer
that delivered nothing falls through to interrupt-and-continue with the same
request, so the Message is delivered once by whichever path can carry it
([ACP](acp.md#steering-extension)); unsupported steering never silently
behaves as Stop. The prompt that must resolve is the adapter's: a session
stays busy until the agent resolves the cancelled turn, so every Run waits
for its session to stop prompting before claiming it; late updates from the
old turn cannot reach the new Run.

An autonomous Run (`origin = autonomous`, [attribution and
lifetime](acp-attribution.md#attribution-and-lifetime))
has no RunInput and never enters this state machine. The user's next start,
steer or Retry first ends it `completed` under a per-conversation launch lock,
then starts a user Run on the same session; ending it inside the start's
transaction would skip its attribution release and drop queued output.

**Queueing is not a Core concept.** Holding a message until the running turn
finishes is implemented in the app; `DeliveryMode` has exactly three
variants. A durable queue was rejected: `conversation_run_inputs.message_id`
is `NOT NULL`, so every waiting message would render as a bubble under the
streaming answer, and a separate table needs its own recovery and cancel paths.

## Deletion And History Policy

Normal product actions archive or unlink (`archived_at`, `removed_at`)
instead of erasing; Workspace, Project and ProjectSource deletion is
restricted while Conversations or historical run sources refer to them.
Archive and purge are distinct; the UI never turns an archive into a hidden
cascade. Conversation deletion fences pending payload writers, removes main
rows then payload rows, and keeps a process-lifetime tombstone to reject late
submissions; restart finishes an interrupted deletion. Process cleanup and
filesystem deletion cannot join a transaction, so they are idempotent and
leave recoverable orphan state, never half-valid relational state.

## Enforcement

Never log SQL bind values, message bodies, agent output, authentication data,
session tokens or input text; user-facing errors never expose SQL, filesystem
secrets or environment values. `scripts/check-diesel-schema.sh` holds
`schema.rs` to the migrations, and `check-migrations.sh` keeps committed
migrations append-only. No other process may own or share the database until
Ottam has a persistent broker and a reviewed multi-client contract.
