---
kind: spec
status: active
---

# Markdown Renderer Improvements

## Current Status

The Chat transcript is the native AppKit collection renderer in the
[renderer architecture](../../../architecture/app/chat/transcript/markdown-renderer.md),
with code and table parity done and transcript-only TextKit machinery
removed. What remains is keyboard and accessibility acceptance and
evidence-gated performance work. Label shimmer runs on a Core Animation
gradient mask; the overall streaming CPU goal stays open in the
[measurement record](../../../operations/performance/transcript-renderer.md).
Composer and document editing are in the
[Markdown Editor spec](../editor/markdown-editor.md).

## Next

### Accessibility and keyboard acceptance

Text items already publish semantic link/image children, link press
actions, selected text and ranges, and responder-chain Copy, with native
tests. Registered manual transcript scenarios define checks for paging past
512 KiB, selection/copy, detach, disclosure, and route restoration; they
have not been run for this acceptance.

- Scope: find specific failures or missing evidence in keyboard traversal
  between text, code/table actions, and hosted cards; VoiceOver selection
  announcements and focus after item reuse; and AX tree and materialized-item
  bounds during long-history paging and scrolling. Inspect the real window
  and AX tree before calling any of these an implementation gap.
- Done when: each area has a recorded real-window result or a filed gap.
- Verify with: `chat.transcript-appkit-native` and the paging scenarios,
  screenshots plus AX.

### Evidence-gated performance work

Each item is its own rollback boundary and starts from the unchanged
measurement protocol; three matched paired runs must show no correctness,
latency, materialization, accessibility, or CPU regression.

- **Structural invalidation clamp:** resume only after proving a staged
  dirty suffix plus `invalidateDataSourceCounts` cannot suppress a genuine
  reload, global metric invalidation, recovery, replacement, or teardown.
- **Tail text and fade dirty rectangles:** adopt line/range containment
  only when draw counts and duration improve without selection, link,
  image, Reduce Motion, or fade regressions.
- **Adaptive live-delivery byte budgeting:** design only after three
  repeated runs show preparation or layout p95/p99 above the frame interval,
  correlated with admitted bytes and pending age. Keep whole ordered deltas
  and let one oversized delta progress.
- **Two-generation AppKit layout hooks:** only for reproducible insertion or
  deletion residue after the native completion barrier; old and final
  attributes never feed mixed-generation materialization.

## Backlog

- [ ] **Long streamed fences rebuild the whole block per delta.** A delta
  into an open fence extends the projector node in place but rebuilds the
  block's artifact: the tokenizer runs over every line and Core Text re-sets
  the whole block. A 2000-line block takes about 30 ms per rebuild in a debug
  build, past the 30 Hz commit budget on its own; short fences show no
  regression.
  - Scope: cache `(line, language) → tokens` in the artifact cache so only
    new or changed lines tokenize; keep the previous artifact's natural width
    and per-line Core Text lines and re-set only the trailing paragraph when
    the delta has no newline before it. Measure with a fence-heavy corpus.
  - Done when: a 2000-line open fence streams at 50 deltas/s without the
    rebuild dominating the commit interval, and every code-block test passes.

## Non-Goals

- Replacing the AppKit collection renderer or moving ownership back into
  SwiftUI or TextKit.
- Speculative budgeting, redraw, or layout mechanisms without their evidence.
- Treating aggregate CPU or headless tests as native-window interaction
  evidence.
