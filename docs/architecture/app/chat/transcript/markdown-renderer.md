---
kind: architecture
status: active
---

# Native Markdown Transcript Renderer

Chat transcripts use an AppKit-owned, block-oriented renderer: an
`NSCollectionView` virtualizes transcript blocks, Core Text draws selectable
Markdown text, and visible semantic cards host narrowly scoped SwiftUI views.
SwiftUI composes the surrounding Chat surface but does not own transcript
scrolling, layout, selection or streamed mutation.

The renderer consumes canonical `TranscriptItem` values and owns no durable
Conversation state. Renderer-neutral parsing and styling live in
`OttamMarkdownEngine`; AppKit owns geometry and interaction.

## Ownership

```text
Core normalized Conversation revisions
  → ChatTranscriptStore            merge by local ID / revision / UTF-8 offset
  → ChatTranscriptPresentationCoordinator   latest-only snapshots
  → MacAgentTranscriptViewController
      ├─ TranscriptBlockProjector / TranscriptProjectionStore
      ├─ TranscriptRenderTransactionCoordinator   (single writer)
      ├─ TranscriptLayoutCoordinator / TranscriptCollectionLayout
      └─ TranscriptSelectionController
```

- **Store.** Core owns transcript identity, text, ordering, segmentation and
  revisions. `ChatTranscriptStore` merges saved pages and live
  `conversation.transcriptDelta` notifications; it never replays adapter
  envelopes or rebuilds per-Run timelines. Its `StreamingCommitCoordinator`
  display link publishes text-only ticks straight to the mounted native view
  without invalidating the workspace's SwiftUI graph; structural and lifecycle
  changes publish normal snapshots.
- **Presentation.** `ChatTranscriptPresentationCoordinator` keeps only the
  latest pending snapshot while a native publication is in flight and commits
  copy sources only after native completion. Conversation changes supersede
  pending work; teardown retires it.
- **Render transaction.** `TranscriptRenderTransactionCoordinator` is the
  single writer across projection, collection mutation, layout, viewport,
  completed presentation and canonical acknowledgement. Projection, layout and
  item views are calculation helpers and add no queues or coordinators.

A delivered revision is not proof of durability; see the
[checkpoint contract](../../../backend/persistence.md#transcript-ownership-and-order).

`MacAgentTranscriptView` implements `sizeThatFits` so SwiftUI never measures
the AppKit transcript through a temporary Auto Layout engine per constraint
pass.

## Invariants

Identity and projection:

- Blocks carry stable `TranscriptBlockID`s; the data source never uses
  index-derived identity. Stable prefix blocks keep identity while the open
  streamed tail changes; hosted cards stay single blocks.
- A plain-text append takes the bounded fast path only when all preceding
  source is represented and the tail cannot change Markdown structure.
  Incomplete link, fence, list, quote and delimiter prefixes are deferred or
  reparsed with context. Withheld text (a pipe paragraph that could still
  become a table) always goes through the parser once conclusive, even when
  the resolving delta is a bare newline. An unclosed fence keeps one identity
  through its own newline.
- The renderer sorts by Core's positions. An admitted steer splits the output
  (`assistant A → steer → assistant B`) and that order survives restart and
  rejected delivery. Revisions never move an existing item. Only adjacent
  reasoning, tool and plan items form an activity group, keyed by its first
  local item ID.
- Live Run status and finished output use a `run/<runID>/trailer` item; no
  assistant Message replaces it, and it reports the Run outcome independently
  of input delivery.

Layout and publication:

- AppKit completion is the barrier before viewport publication,
  materialization, selection refresh or presentation promotion. Admission
  acknowledgement never precedes native render completion.
- Identity-stable changes may complete synchronously while the coordinator is
  idle; identity-changing updates wait for native completion. Canonical inputs
  coalesce while AppKit is busy; active native updates are never cancelled.
- Geometry changes are atomic and nonanimated. Card-internal effects animate
  only inside final bounds, respect Reduce Motion, and never create mixed
  layout generations.
- The controller decides scroll anchoring (`.bottom`, `.anchor`, `.offset`,
  `.unchanged`, `.deferred`); the layout applies it and never judges whether
  the reader is pinned.
- Content width comes from the clip view and does not shrink for an overlay
  scrollbar, so content guides stay aligned with the composer.
- Vertical rhythm belongs to `TranscriptCollectionLayout`
  (`MarkdownBlockSpacing.gap(from:to:)`), not paragraph styles: Core Text
  drops a frame's trailing paragraph spacing, so paragraph spacing in styled
  text is a no-op.
- Failed publication keeps the latest snapshot and retries once after the
  transaction unwinds; a second failure exposes an explicit Retry. Terminal
  snapshots flush immediately. Conversation detail applies even when the
  transcript request fails; raw event replay is never a fallback.
- A hosted card measures against its payload's `heightSignature`, not its full
  `signature`, so a revision bump without a height change costs no relayout.

Items:

- Code and table blocks are selectable Core Text items in one native tile
  whose chrome, fold and scroller state are measurement inputs, so layout and
  drawing never disagree. A wheel gesture over a tile is owned once per
  gesture: sideways with overflow stays on the tile, anything else scrolls the
  transcript.
- Tables are one Core Text document on `NSTextTab` stops
  (`TranscriptTableLayout`); row height is pinned through paragraph spacing
  (`TranscriptTableChrome.rowHeight`), not a minimum line height, which makes
  inline-code pills fill the row.
- List markers are shared between the styler and `TranscriptBlock.displaySource`
  (`MarkdownStyler.markerText`) so copy and drawing agree.
- Code block expansion (`expandedCodeBlocks`) travels with the
  `TranscriptViewSession`; an open fence's height freezes at the fold cut.
- Core Text items cache immutable artifacts; reuse clears transient selection
  and hover state and never keeps canonical content independently.

Selection, copy and accessibility:

- `TranscriptSelectionController` owns transcript-wide selection; item views
  only hit-test and draw. Semantic cards are hard selection boundaries, never
  traversed as hidden text views.
- Copy reconstructs Markdown source for whole blocks (fenced code with a fence
  longer than any inner backtick run; a pipe table via
  `TranscriptTableSource.pipeTable` that parses back to the same cells) and
  rendered text for partial ranges. Responder-chain Copy uses the same
  selection. Whole-message copy exists only for finished turns whose final
  assistant body loaded without error, and copies that message alone.
- Links are marked by color alone; activation resolves relative destinations
  inside the Conversation resource root and opens through `NSWorkspace`.
- Selection, copy and accessibility expose the last completed presentation
  during an in-flight structural update.
- A hosted card reaches the AX tree only through an element it publishes. A
  hosting view with nothing focusable publishes no subtree, so the row must be
  one AX element; card identity is re-applied on every configure because the
  card store adopts a changed spec into the existing host.
- Early-ended turns (stopped, interrupted, unexplained) share one AX
  identifier: what an automated reader needs is that the output is incomplete.
- Expanded tool cards (`ChatPayloadDetailStore`) copy the complete captured
  text up to 16 MiB and report the limit beyond; requested changes never imply
  successful application.

## Rejected

- SwiftUI `ScrollView`/`LazyVStack` as the transcript owner.
- A transcript-wide TextKit surface: cards implement the app-owned
  `TranscriptCard` contract, not `NSTextAttachment`. `MarkdownTextView` stays
  the TextKit 2 owner for composition only.
- `reloadData`, forced redraw, disabled native scrolling or mixed-generation
  materialization as streaming fixes.
- Parser, projection, layout or view objects as competing writers; any
  Run-based text relay or persistence stand-in owning transcript text.
- SwiftUI-driven shimmer (`TimelineView`): it re-triggered window constraint
  and workspace layout through the nested hosting view. `ChatShimmerText`
  moves a Core Animation gradient mask over a static hosting view instead.

Renderer telemetry times each transaction phase separately; aggregate CPU
does not prove ordering or frame pacing. The measurement procedure is in
[Transcript Renderer Measurement Protocol](../../../../operations/performance/transcript-renderer.md);
open work is in the
[Markdown Renderer Improvements spec](../../../../specs/app/chat/markdown-renderer-improvements.md).
