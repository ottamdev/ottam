---
kind: operations
status: active
---

# Transcript Renderer Measurement Protocol

This is the comparison protocol for
[Markdown Renderer Improvements](../../specs/app/chat/markdown-renderer-improvements.md).
It makes a number recorded on a feature branch and a number recorded on
`main` mean the same thing. The repository-wide capture, validity, pairing,
evidence, and reporting rules are in the
[Performance Measurement Playbook](README.md). This document adds the
transcript fixtures, signals, bounds, and recorded results. Where it is
stricter, it wins.

Revise bounds (§5) only before feature-branch results are seen.

## 1. Reference machine

The §5.1 and §7 numbers were taken on a MacBook Pro `Mac17,8` (Apple M5 Pro, 6
efficiency + 12 performance cores, 64 GB) on macOS 26.6.2 and Xcode 26.6. It
drove two Studio Display XDR panels at 2560×1440 @2× and **120 Hz**, on AC
power, with no thermal warning (`pmset -g therm`). Re-check two properties if
the machine changes:

- **Display refresh.** `StreamingCommitCoordinator.applyFrameRateRange`
  clamps the display link to `min(screen.maximumFramesPerSecond,
  structuralRate)`. On 60 Hz and 120 Hz panels the fixed 30 Hz commit rate
  runs exactly. A 24 Hz panel would clamp it and change what the commit-rate
  gates measure. Record which panel the window was
  on for every run.
- **Thermal headroom.** A short sample on a well-cooled machine on AC looks
  flatter than sustained behavior. Treat a suspiciously tight spread as a
  reason to lengthen the sample.

## 2. Build configuration

All measurement runs use the `profile` configuration:

```sh
OTTAM_UI_TEST_CONFIGURATION=profile ./scripts/ui-test.sh run <scenario-id>
```

`profile` is `scripts/build-app.sh --profile`: `-c release`, keeping
`-Xswiftc -DDEBUG`. The app is optimized, but the `#if DEBUG` UI-test and
capture hooks the scenario runner drives still exist. It is cached separately
from `debug` (`.build/ui-testing/runtime/source-app-profile`). Never compare a
`debug` run against a `profile` run.

Activity-label animation:

- `transcript.updating-tools` captures real ACP tool updates: equal-width label
  replacement, truncation, completion, a second turn, and cancellation.
  Inspect the paired before/motion images and the AX evidence, because a
  passing harness result does not prove visual motion.
- `transcript.measure-idle-tool`, run through
  `scripts/performance/measure-transcript.sh`, gives a 20-second silent
  pending-tool phase. It isolates idle animation cost and does not measure
  changing tool text. Report GPU, WindowServer, and energy limits separately
  from app-process CPU.

## 3. Metrics and where each comes from

### 3.1 `sample` + `ps`: the CPU number

- **Primary metric: CPU seconds over the whole streaming phase, divided by the
  phase's wall duration.** The scenario runner records durable phase markers
  after the requested focus regime is established (see the playbook §3). The
  corpus is byte-frozen, so the phase covers the same workload in every
  matched run.
- `sample` runs for 8 s mid-stream and is read for SwiftUI graph, text layout,
  and `drainTick` totals. It shows *where* time went, not how much.

Never take magnitude from a mid-stream `%cpu` window. On `main` against
`main`, an 8 s window varied by 26–29 % depending on where it landed, while
whole-phase CPU varied by 1.7 %. Normalizing by phase duration also absorbs
interference: a disturbed run streams longer and burns proportionally more CPU.

This metric is statistical. It cannot show how time was spread across commits:
many small commits and a few 5 ms hitches can produce the same aggregate.

### 3.2 Signposts: counts, distributions, outliers

`OSSignposter`, subsystem `dev.ottam.Ottam`, category `transcript-render`.
Always compiled and disabled at runtime, so the cost is near zero when nothing
is listening and a release build stays traceable. The schema is in
`OttamInstrumentation/TranscriptSignpost.swift`.

| Signal | Kind | Carries |
|---|---|---|
| `assembly` | interval | item count |
| `stream.commit` | interval | delta count, byte count |
| `markdown.parse` | interval | item id, revision |
| `layout` | interval | — |
| `disclosure` | interval | row id |
| `scroll.correct` | event | reason (pin / anchor / prepend / restore) |
| `scroll.follows` | event | whether the reader is pinned to the end |

Renderer-neutral scheduling signals:

| Signal | Kind | Carries |
|---|---|---|
| `display.tick` | event | configured Hz, physical screen Hz, callback spacing, missed slots, pending bytes/deltas |
| `normalized.publication` | event | generation and native publication success; does not measure frame latency |
| `commit.present` | spanning interval | native transaction ID; submission-to-Core-Animation-completion proxy, with failure/teardown outcomes separated |

AppKit diagnostic signals explain a native renderer regression. They are not
comparison gates:

| Signal | Kind | Carries |
|---|---|---|
| `render.prepare` | interval | transaction ID, projected block count, artifact count |
| `collection.update` | spanning interval | transaction ID, insert/delete/move counts, native completion result |
| `collection.mutation` | interval | transaction ID; only the mutation closure AppKit invokes inside `collection.apply` |
| `layout.commit` | interval | viewport target, immediate/deferred materialization, synchronous materialization count |
| `collection.frame` | interval | transaction, requested and actual size deltas, whether the document frame changed |
| `collection.scroll` | interval | transaction, viewport target, proposed/constrained/actual offset deltas, whether the clip moved, whether scrollers were reflected |
| `collection.materialize` | interval | transaction, pre-layout dirty state, visible item counts and appeared/disappeared item counts |
| `collection.refresh` | interval | transaction, all/ID/none mode, requested count, applied visible-item count, selection refresh |
| `collection.update-lifecycle` | event | prepare/finalize phase, native bracket ID, action counts, earliest affected index |
| `collection.invalidation` | event | scope and origin, AppKit full/count flags, invalidated count, dirty start, enclosing native bracket |
| `collection.attributes` | interval | requested/expanded rects, geometrically intersecting items, returned items |
| `collection.environment` | event | transaction, app/key-window/visibility state, thermal state, current-thread QoS |

How to read them:

- **Phase boundaries.** Analyzers prefer explicit measured-phase timestamps,
  then successful `normalized.publication` events. `stream.admission` comes
  only from historical builds and is a legacy fallback. Event-only fallback
  spans are diagnostic and never replace matched workload boundaries.
- **`commit.present`.** It wraps canonical native mutation and final layout
  transactions. It needs native success and Core Animation completion, which
  is a proxy and does not prove physical presentation. Failed and torn-down
  updates are reported separately and excluded from latency distributions.
  Record which proxy ended the interval (a `CATransaction` completion or a
  next-display stamp).
- **Asynchronous intervals.** `collection.update` begins before
  `NSCollectionView.performBatchUpdates` and ends in its completion handler.
  `stream.commit` measures synchronous display-tick work only; never stretch it
  over either lifecycle.
- **Summary fields.** `phaseNormalizedPublications` and `phaseDisplayTicks`
  count delivery work. `streamingDeliveryValid` checks delivery evidence.
  `transcriptBodyRequestsInRun` counts body-read RPCs.
- **Layout outcomes.** An unchanged visible set does not make
  `layoutSubtreeIfNeeded` a no-op, so read duration together with
  `needed_layout_before`. Visible-item set construction is guarded by
  `OSSignposter.isEnabled`. Compare these timings only against an
  instrumentation-identical baseline.
- **Scheduling.** Active/inactive investigations archive `lsappinfo`,
  `taskpolicy`, and `ps` snapshots. Those snapshots and CPU Counters traces are
  the evidence for App Nap, priority, and core residency;
  `collection.environment` reports only public in-process state. Do not infer
  App Nap from `NSApplication.isActive`, and do not add a process activity to
  make a measurement look active.
- **Placement.** Comparative signposts sit at seams that survive a renderer
  cutover: normalized revision publication and native submission, the
  display-link tick in `StreamingCommitCoordinator`, `ChatTranscriptAssembler`,
  the parser, and the submission-to-presentation proxy. Diagnostics below those
  seams explain a comparative result; they do not replace it.

A direct Instruments or `xctrace` attachment helps diagnosis. It is not a
comparable benchmark unless it keeps the playbook's phase, environment,
validity, and raw-evidence contract. Use the tracked adapter:

```sh
./scripts/performance/measure-transcript.sh \
  --run renderer-feature-active-1 \
  --variant feature \
  --pair 1 \
  --scenario transcript.measure-single-reply \
  --focus active \
  --template Logging
```

### 3.3 Why both

Signposts capture framework work only when it is synchronous inside the
interval. AppKit can schedule layout, materialization, and presentation after
the call returns, so signposts under-report total cost. `sample` stays the
primary CPU number. Signposts supply the counts, per-commit distributions, and
outliers that `sample` cannot give. A claim that rests on only one of the two
is incomplete.

## 4. Fixtures

Fixtures are deterministic and seeded, so a rerun is byte-identical across
checkouts. Record seeds with every run. `OttamTranscriptFixtures` builds every
fixture. It is a Foundation-only leaf target shared by the unit benchmark and
the UI-test scenarios. `TranscriptFixtureCatalog` holds the seeds, and
SHA-256 goldens in `TranscriptFixtureTests` pin the realized bytes.

| Fixture | Shape | Corpus seed | Scenario |
|---|---|---|---|
| Streaming, multi-turn | 40 turns, 250,152 B | `0x5747_4D11` | `transcript.measure-multi-turn` |
| Streaming, single reply | one reply, 64,168 B | `0x5747_4D11` | `transcript.measure-single-reply` |
| Streaming, detached | single reply, reader scrolled back | `0x5747_4D11` | `transcript.measure-detached` |
| Streaming, sustained | 4 MB, opt-in via `OTTAM_BENCH=1` | `0x5747_4D11` | unit benchmark only |
| Settled document | small / 250 KB / 4 MB opt-in | `0xD0CC_0DE` | markdown lab |

Wire chunking has its own seed (`0x0DD1_7E5`), so chunk boundaries can vary
without changing the bytes. The older seeds (`0xC0FF_EE`, `0xBEEF`,
`0xFACA_DE`) are frozen. Never edit an existing generator; add a new one.

Streaming runs sweep chunk sizes of 32–256 B; 5, 16, and 50 deltas/s; and
pinned or detached scroll states. Knobs:
`OTTAM_UI_TEST_STREAM_{CHUNK_BYTES,DELAY_MS,SAMPLE_SECONDS,DETACH_VIEWPORTS}`.
The commit rate is fixed at 30 Hz (`StreamingCommitLifecycleView`); a Debug
build's `OTTAM_DEBUG_STREAMING_TEXT_FPS` overrides it for experiments only,
never for a gate.

### 4.1 Scroll state is set by a probe, not a gesture

The detached fixture presses `chat.scroll-back-probe`, a DEBUG-only control
that exists only when the scenario runner launched the app. Each press calls
the controller transition that sets detached mode, then scrolls back one
viewport. The measurement scenario has no action that can reattach.

Do not synthesize the gesture from outside the process, because every route
fails:

- HID-tap wheel events route by the physical cursor and fail silently.
- Process-targeted events are dropped unless the app is frontmost, also
  silently.
- `AXScrollUpByPage` returns `kAXErrorCannotComplete`.

The probe sets detached mode directly, because the pinned commit would re-pin
before a bounds-change notification arrived. A hand-scrolled run cannot be
paired at all. Detach *detection* (that a real gesture detaches, and the anchor
holds across a prepend) is a separate correctness gate, checked against
rendered pixels in the focused real-window scenario.

## 5. Comparison bounds

A comparison is a material regression when all three hold:

- three or more paired runs show the same direction;
- the feature branch exceeds ordinary `main`-versus-`main` variation;
- the feature branch is **>15 % worse** in median or p95 streaming CPU,
  main-thread commit work, or commit-to-present time.

Correctness failures, missed absolute budgets, and repeatable hitches block
regardless of percentages. If the noise floor swallows a bound, record the
revised bound here **before** measuring the feature. Revising after seeing
feature results is prohibited. So is inventing a tighter sampling method to
rescue a number.

### 5.1 Noise floor

Six `main`-against-`main` runs of `transcript.measure-single-reply` in
`profile` on the §1 machine:

| Metric | min | median | max | Spread |
|---|---|---|---|---|
| **CPU % of streaming phase** | 23.6 | 23.8 | 24.0 | **1.7%** |
| CPU seconds | 5.36 | 5.42 | 6.23 | 16.1% |
| `stream.commit` count | 720 | 721.5 | 799 | 10.9% |
| Run duration | 30.7 s | 30.9 s | 33.7 s | 9.7% |

One run was disturbed: it ran 2.8 s longer with 799 commits. It stays in the
set because the gating metric absorbed it and the raw metrics did not. The
15 % bound stands.

Gate on CPU % of the streaming phase. Never gate on raw CPU seconds or raw
commit counts, which scale with run duration. Signpost counts from `log stream`
drop records under load: two batches of one scenario disagreed by 2.9 % and
23.7 % on commit count. If a gate ever rests on counts, capture them with
`xctrace`.

## 6. Running a comparison

Follow the [playbook](README.md). The entry point is
`scripts/performance/measure-transcript.sh`. Its immutable run directory
records source, environment, focus, phase, and raw trace evidence.

1. Confirm the machine matches §1: AC power, thermal state, and which panel the
   window is on.
2. Build both sides in `profile`.
3. Run paired, `main` then feature, close together with the same fixture and
   seed.
4. Repeat at least three times. Report p50, p95, and max. Report an isolated
   max outlier, but do not assert on it.
5. Archive raw results, not summaries: `./scripts/ui-test.sh archive <name>`.

Screenshot success is not performance evidence. The retained artifacts are the
`sample`/`ps` numbers, the signpost trace, and the fixture seeds.

Traps found in earlier runs:

- **Variant switches.** Restore sources with fresh mtimes, and invalidate the
  source-app fingerprint if its bundle was built with stale dependencies.
  Restoring archive mtimes once left a baseline Core cached inside a feature
  app. The benchmark now rejects normalized single-reply runs with no sustained
  display clock.
- **Phase end.** Scenarios wait for the fixed Stop control, then for the submit
  control's ready value. Polling for the running trailer's absence ended phases
  early, because the AX walk can miss it mid-stream.

## 7. Where the numbers stand

On the §1 machine, `transcript.measure-single-reply` in `profile`, active
focus, 64-byte chunks every 20 ms: the current native renderer streams at
mid-30s % of one core (median 36.0 %, range 33.4–36.9 %) with zero body-read
RPCs. The 30 % target is unmet. The §5.1 noise floor (23.8 %) was taken on the
retired TextKit transcript and is a method baseline, not a renderer to beat.
`transcript.measure-idle-tool` shows the Core Animation shimmer mask at
0.56 % over a 20 s silent pending-tool phase; that covers idle animation only,
not streaming, GPU, WindowServer, or energy. Each new comparison captures its
own matched baseline.
