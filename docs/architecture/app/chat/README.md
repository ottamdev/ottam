---
kind: architecture
status: active
---

# Chat Architecture

Core owns durable Conversations, Messages, Runs, inputs and the normalized
transcript spine; lifecycle events stay separate from transcript content. The
Swift Workspace model consumes the ordered protocol stream and projects it into
native presentation. It keeps no second durable transcript store, and message
phase and parent references never reorder the transcript or add cards.

Ownership on the Swift side:

| Area | Owner |
| --- | --- |
| Draft text, attachment selection, mode picks | composer (`ChatWorkspaceModel`) |
| Merging transcript revisions, display-clock publication | `ChatTranscriptStore` |
| Everything after publication | [renderer](transcript/markdown-renderer.md) |
| Per-conversation sessions and eviction | `ConversationTranscriptCache` |
| Queued messages | `ChatMessageQueue` (app state, `UserDefaults`) |
| Reader state (open groups, reading place) | `ChatConversationUIState` |

Submission clears composer state only after the submission boundary accepts
the input; what the composer shows as attached is exactly what is submitted.

## Session Observations And Failures

- Retry activity uses the turn-status row; failures and actionable usage
  limits use the notice surface; permission presentation wins over both.
- Saved observations are labeled "Last reported": an elapsed reset time does
  not prove quota replenished, and quota text alone supplies no reset time.
- Termination and Core disconnection clear transient activity; a stale
  snapshot cannot restore it. A newer watermark may restore activity for a
  continuing Run after a reconnect; terminal Runs stay closed.
- A failed turn keeps a persistent notice derived from the saved run error.
  Opening it never retries. Retry (`ChatWorkspaceModel.retryableFailedRunID`)
  is offered only for the latest failed turn, while idle, with working root and
  engine available and no input still resolving delivery. It posts an explicit
  continuation with the failed run's engine, model, mode and effort; the old
  output and failure stay in history.
- A tool result Core could not store keeps the assistant text readable and
  shows "Some tool details could not be saved completely". Only a genuinely
  missing transcript range gets the transcript-loss warning.

## Conversation Cache

`ConversationTranscriptCache` keeps one `ConversationTranscriptSession` per
recently opened conversation (store, last detail, resync flag,
`TranscriptViewSession`); eviction drops them together.

- The cap counts conversations, not bytes: a session's memory is mostly what
  was built from its text, so a byte budget on body text never fired. The
  shown and running conversations are never evicted; memory pressure keeps
  only the shown one; Core replacement and workspace change drop all.
- Cached stores keep merging deltas and read no bodies. A gap, recovery
  notice or undecodable delta marks the session for resync on return; the mark
  clears only when a resync finishes.
- A cached return shows the cached detail without applying it (engine, mode,
  usage may be stale) and refreshes without the first transcript page.
- `ChatConversationUIState` keys open activity groups by member ID because a
  group's ID changes when older pages prepend. It survives eviction and Core
  replacement; a workspace change clears it. The Subagent tab is not cached.

## Sending Into A Working Run

| gesture | live Run | no live Run |
| --- | --- | --- |
| Return | queue behind the turn | send |
| Command-Return | steer into the turn | send |
| Escape twice, or Return on an empty draft | stop the Run | — |

Steering is the deliberate gesture because it costs something (interrupts or
lands mid-thought); queueing needs nothing from the adapter, so the unmodified
key queues and a live Run never disables the composer. An engine without
native steer steers through interrupt-and-continue: capability changes the
mechanism, not the gesture. Exception: an autonomous follow-up run takes over
a message sent while one streams, and the button reads Send, not Queue.

The queue is app state, not a Core concept; see
[persistence › Stop And Steer](../../backend/persistence.md#stop-and-steer).
Nothing is written mid-Run, so the live Run stays at the tail for the turn.
Queued messages drain one per Run, only after `completed`. `canceled`,
`failed` and `interrupted` hold the queue: stopping is how the reader takes
the wheel back and must not fire the next message, and a steer's follow-up
Run is already starting. Removing every queued item is the discard path;
there is no separate clear verb.

## Composer Controls

- Modes are confirmed and pending per conversation; sends use pending, then
  confirmed, then the new-chat default. A pick in an existing conversation is
  pending until the next message and never changes the per-engine default.
  Without `liveModeChange`, a pick during a turn applies to the next turn.
- "Always allow tool actions" appears only for agents that support it; in a
  draft it edits the default, in a chat that chat's value.
- `ChatContextGauge` is a drawn ring because a circular `ProgressView`
  ignores tint on macOS. Missing token values read as unavailable, never zero.

## Command Picker

`/` at the start of an otherwise empty composer opens `ChatCommandPanel`:
modes, agent commands and skills. Modes list before the agent reports
commands. A command that takes input is inserted; others send at once unless
attachments are pending. Codex skills use `$name`. The open card replaces a
plan or question panel, which returns on Esc or send (`isCommandPickerOpen`).

## Plan Review And File Tabs

- A pending plan approval gets no transcript card; other permissions keep
  theirs. `ChatPlanPanel` lists the adapter's own options; Revise picks the
  stay-in-plan option and focuses the composer. A plan arriving for the
  conversation on screen opens the Plan tab even over Review or Terminal.
- File tabs (`ConversationTool.file`, keyed by `MarkdownFileID`) share the
  Plan tab's viewer, keep their place across switches, survive a file's
  deletion and return, and last for the window's lifetime.
- `HostedContinuationDividerCard` marks the first output of a run whose engine
  differs from the previous run's (replayed vs resumed); a same-engine replay
  gets one too; none when the predecessor run is not loaded.

## Attachments

Image attachments are typed Chat content. Composer previews, submitted
content and the transcript agree: removing a preview removes it from
submission, and an agent that does not accept images fails visibly before
submission rather than dropping the attachment.

- Submission emits ACP image content blocks beside text. No private agent
  syntax, no double base64. Agents receive the image itself; Ottam never
  relies on a model reading a local path, and internal storage paths never
  appear as prose.
- Attachment bytes belong to Ottam's durable Conversation domain, not an
  adapter home. Relative Markdown image paths resolve against the
  Conversation's resource root; absolute and `~/` paths stand alone. Links and
  inline file references stay inside the root.
- Image reads are not held to the root (rendering only draws pixels, and an
  agent's `/tmp` output should show). Containment is enforced at the read
  boundary instead: `MarkdownImageAttachment.readLocal` opens only regular
  files, nonblocking, on a local volume, under 32 MB, never resolving a name
  inside an automount point, and refuses a file URL naming a host.
- Loading and decode failures produce compact stable placeholders; an async
  image result may change its block height but never owns scroll mode.

Not implemented: repository-file mentions, inline image tokens, file
promises from other apps, non-image attachments.
