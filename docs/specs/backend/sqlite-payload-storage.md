---
kind: spec
status: active
---

# SQLite Payload Storage

## Current Status

Payload storage ships; the contract is
[Persistence](../../architecture/backend/persistence.md#payload-storage), and
the migrations under `storage/payload/migrations/` are the schema source of
truth. Open: an undecided write-cost tradeoff, and upgrade and backup
coverage for the payload database.

## Active

- [ ] **Decide the incompressible-write tradeoff.** Open decision.
  - Measured against the old per-part file writer (three matched triplets per
    workload): incompressible writes cost about 67% more writer CPU (5.3 ms per
    2 MiB capture), refills about 52% more (2.0 ms per 1 MiB). A raw-codec
    control puts most of the cost in the SQLite worker path, not compression.
    The same run cut tiny-result allocation from 409.6 MB to 50.25 MB and
    16 MiB streamed-delta CPU from 18.7 s to 0.29 s.
  - Options:
    - A. Accept the tradeoff as measured.
    - B. Profile the worker's incompressible path first, and accept only if
      no cheap fix appears.
  - Recommendation: A. The cost is a few milliseconds per large binary
    capture against large wins on common workloads. The user decides.
  - Done when: the choice is recorded under Decisions with its date.
  - Verify with: `./scripts/test-rust-core.sh --lib --filter payload`, the chat
    storage tests, and `./scripts/ui-test.sh run chat.transcript-payloads` with
    screenshots plus AX.

## Next

- [ ] **Cover both databases during upgrades.**
  - Why: only `Database::open` rejects migration versions newer than the
    binary. `storage::payload::Repository::open` runs pending migrations without
    that guard, and payload-only migrations don't trigger the coordinated
    backup. `previous_schema_upgrade_backs_up_and_backfills_run_origin` takes
    the main backup branch but never restores the pair.
  - Scope: apply the version refusal and pre-migration backup policy to both
    databases, with a typed failure and no automatic downgrade.
  - Verify with: newer payload migration rejection, and a previous-to-current
    fixture that takes the backup path, restores the verified pair and checks
    committed history after reopen. Existing fresh/down-up and coordinated WAL
    restore tests stay as regressions.
- [ ] **Consolidate migration backups and bound retention.**
  - Why: pending main migrations create the coordinated pair plus an extra
    main-only `ottam-before-<version>.sqlite`. Nothing prunes backups.
  - Scope: keep a verified recoverable set without duplicate main snapshots.
    Choose and document a retention policy before adding pruning.
  - Verify with: backup failure before migration, retained recovery sets, and
    pruning that can't discard the only verified pre-upgrade state.

## Backlog

- Cross-payload content-addressed deduplication and dictionary compression.
- Reusing matching chunks from a differing final snapshot. A replacement may be
  written once; never repeatedly rewrite a growing BLOB.
- Automatic compression-level tuning and durable-history retention controls.
  Tool-result cache size and explicit clearing already exist in Settings.
- Live-provider acceptance of isolated replay recovery. Current fixtures don't
  establish live-provider coverage; availability and identity mapping are
  provider-dependent.

## Decisions

- A dedicated payload database with its own connections and
  worker-owned transactions. It never takes the main database gateway. Keep
  Diesel; no `rusqlite`, other engine, custom binary log, or JSONL storage.
- 2026-09-10: paged reads cost about 0.3 ms more per page than files; accepted
  for the paged output viewer given the storage savings. Investigate any
  repeatable regression over 10%.
- 2026-09-11: SQLite-only clean break. Core doesn't convert, read, back up or
  fall back to legacy payload files; an experimental home needs an explicit
  reset.
- WAL with `synchronous=NORMAL` on both databases; capture completion waits for
  both commits, with no cross-database transaction or power-loss barrier.
  Independent rollback is accepted capture loss, reported explicitly.
- SQLite memory statistics (`SQLITE_CONFIG_MEMSTATUS`) take one global mutex
  per allocation. The app and integration tests keep SQLite's defaults; only
  the `ottam-core` unit-test binary turns statistics off
  (`storage/test_home.rs`), because its parallel tests contended on that mutex.
  Measure concurrency and latency against the app's settings. Turning
  statistics off in production would need an audit of process-wide SQLite
  configuration first, since it also disables heap-limit facilities.
