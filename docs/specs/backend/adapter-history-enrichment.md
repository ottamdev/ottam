---
kind: spec
status: deferred
---

# Local Transcript Capture And Persistence

## Current Status

The local transcript cutover is done; the contract is
[Persistence](../../architecture/backend/persistence.md#transcript-ownership-and-order),
and payload follow-ups are in [SQLite Payload Storage](sqlite-payload-storage.md).
What remains waits on adapters or on measurement.

## Deferred

- [ ] **Show reasoning activity when the adapter supplies a lifecycle signal.**
  - Why: neither tested pin sends reasoning start/end over ACP. Readable
    summaries arrive late or never, and core can't infer a missing start from
    later thought titles.
  - Prerequisite: an upstream adapter capability or extension for reasoning
    start/end with scoped identity, and a pin upgrade. Don't patch installed
    caches or read vendor session files as a fallback.
  - Scope: normalize the signal as live activity, independent of reasoning text
    and transcript positions. Keep generic working status when unsupported.
    Empty thinking text and token-progress events never create blank rows or
    fabricated summaries. Clear activity on matching completion, cancellation,
    disconnect and Run end; a late or unrelated event never clears a newer one.
    Never reorder thoughts ahead of tools or delay text to keep a title visible.
  - Done when: the adapter prerequisite and Ottam's handling are verified
    separately; supported adapters show reasoning activity before a summary.
  - Verify with: wire captures for each upgraded pin covering delayed or missing
    summaries, thought and text in one burst, interleaved tool completion, stale
    IDs, cancellation and reconnect; real-window screenshots plus AX. Never
    request or reconstruct hidden reasoning.

## Backlog

- **App CPU target of about 30% while streaming.** The last matched pairs
  measured a 35.0% median for `transcript.measure-single-reply`; method in
  [transcript renderer](../../operations/performance/transcript-renderer.md).
  Backend write reductions don't imply the app target passed.
- Measure checkpoint frequency, queue memory, SQLite growth and payload disk
  use (counts, sizes and timings only) before optimizing.
- Durable-history retention controls, compression tuning, deduplication and
  export/import, only when needed. Per-chunk compression and tool-cache size and
  clear controls already exist.

## Decisions

- 2026-09-07: Ottam owns its transcript locally. Sampled `session/load`
  history was inconsistent across Codex, Claude and Pi, so historical detail is
  not recovered from adapter replay; the replay-enrichment plan is retired.
- Not goals: persisting every transport event or provider object, a durable
  streaming log or zero-loss durability, blocking live output on a save, exact
  vendor message boundaries the adapter doesn't report, vendor-history import,
  and migrating old development transcripts.
