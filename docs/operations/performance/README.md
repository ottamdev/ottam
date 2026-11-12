---
kind: operations
status: active
---

# Performance Measurement Playbook

This is Ottam's repository-wide method for performance experiments: how to
capture, validate, compare, interpret, and retain measurements so a rerun
answers the same question as the original. Feature protocols stay
authoritative for their fixtures, signals, budgets, and correctness gates.
Transcript renderer work also follows the
[Transcript Renderer Measurement Protocol](transcript-renderer.md).

## 1. Start with a falsifiable question

Before changing the implementation, write down:

- the behavior being changed;
- the primary metric and its expected direction;
- the deterministic fixture and the measured phase;
- the comparison variants;
- the correctness gates that must stay true;
- the minimum number of matched pairs;
- the material-change threshold and the rollback condition;
- the run-invalidating conditions.

Do not choose a tighter phase, discard rule, or statistic after seeing feature
results. A new method needs a new baseline captured with that method.

## 2. Separate magnitude, attribution and latency

| Question | Evidence | Interpretation |
|---|---|---|
| How much CPU did the operation consume? | Process CPU seconds ÷ measured-phase wall time | Primary CPU magnitude |
| Which stacks consumed it? | `/usr/bin/sample` or a Time Profiler trace | Attribution, not magnitude |
| How is work distributed per operation? | Signpost interval counts and p50/p95/p99/max | Per-operation latency and outliers |
| Why did scheduling change? | CPU Counters/thread-state trace plus process-policy snapshots | Priority and core-residency diagnosis |
| Did the product stay correct? | Focused tests, plus real-window pixels/AX where relevant | Independent correctness gate |

- Do not take CPU magnitude from a short `%cpu` polling window. Work changes
  shape over a run, so moving the window can outweigh the implementation.
  Bound CPU seconds and wall time to the same deterministic phase.
- Signpost intervals include only the synchronous work nested inside them.
  Framework work may run after the interval ends, so signposts explain a
  whole-process result; they do not replace it.
- Time Profiler categories are inclusive and overlap. Never sum them as if
  they partition CPU.

## 3. Define the measured phase

A phase needs an objective start and end that can be recovered from raw
evidence. Prefer semantic markers: the first and last accepted stream admission,
an operation ID, or explicit phase signposts. Record both wall-clock timestamps
and marker ordinals in the per-run summary.

The phase must cover equivalent work in every variant. If a disturbance makes a
run longer while the corpus stays fixed, normalize CPU seconds by that longer
phase. Do not crop the run to make totals resemble another variant.

For transcript streaming, the scenario runner writes the scored phase's start
and end to its durable action log once the requested focus regime is
established. Complete-corpus scenarios bracket the first running turn through
final completion. Detached streaming brackets a fixed-duration window after
its controller-owned scroll-back transition. Unified-log signpost counts are
for diagnosis only, because live log streaming can drop records.

## 4. Freeze the fixture and environment

Record for every run:

- the source commit and dirty status;
- the fixture identity, seed, knobs, and content hash where available;
- the build configuration;
- the scenario ID and result;
- macOS, Xcode, and the hardware and display identity;
- AC or battery power, and thermal state;
- the application focus regime;
- the trace template and instrumentation version;
- process-policy snapshots, when investigating scheduling.

Compare optimized builds that carry identical diagnostic instrumentation. Never
mix debug and profile/release results. Never compare an instrumented feature
build with an uninstrumented baseline.

## 5. Use matched pairs

By default, capture at least three pairs close together: baseline A then
feature A, baseline B then feature B, baseline C then feature C. Within a pair,
keep the machine, display, power state, fixture, seed, configuration,
instrument, and focus regime the same. If session drift is a concern, declare a
separate experiment that alternates the order. Do not change the order after
inspecting results.

Active and inactive app runs are different operating regimes. Compare active
with active and inactive with inactive, and never average them together.
macOS may change scheduler priority and core placement when an app deactivates,
even when the renderer runs the same code.

## 6. Decide run validity before aggregation

A run is invalid when any predeclared condition occurs, including:

- the scenario fails or cleanup is incomplete;
- the focus regime changes during the measured phase;
- the fixture, build configuration, instrumentation, or source variant is
  wrong;
- the measured phase has no work or cannot be reconstructed;
- thermal or power state crosses the declared boundary;
- another process or a permission failure prevents the capture.

Retain invalid runs and state why each was excluded. Do not aggregate, hide, or
replace them without recording both the failed attempt and the rerun. Outliers
from otherwise valid runs stay in the result unless a predeclared condition
explains them.

## 7. Capture workflow

`scripts/performance/measure-transcript.sh` builds a profile app, drives one
registered UI scenario, and records process CPU and focus state. It takes an
8-second `sample`, runs one `xctrace` template, exports that template's useful
table, and archives the UI evidence:

```sh
evidence_root=/path/to/feature/ottam/.build/performance/runs

./scripts/performance/measure-transcript.sh \
  --run pin-baseline-active-1 \
  --variant baseline \
  --pair 1 \
  --scenario transcript.measure-single-reply \
  --focus active \
  --template Logging \
  --root /path/to/baseline/ottam \
  --output-root "$evidence_root"
```

Run the matching feature checkout with the same flags, changing only `--run`,
`--variant feature`, and `--root`, and keeping the same `--output-root`.
Without `--output-root`, evidence goes to
`<measured-checkout>/.build/performance/runs/<run>`. Run directories are
immutable: the command refuses to overwrite one.

The `--template` values:

- `Logging`: signpost events and intervals.
- `Time Profiler`: sampled stack attribution.
- `CPU Counters`: exported thread-state scheduling evidence.

Each template is a separate experiment. The whole-phase CPU summary from every
valid run is usable, but stack or thread-state detail should be compared only
between runs captured with the same template.

Analyzers:

```sh
./scripts/performance/summarize-runs.py \
  .build/performance/runs/pin-{baseline,feature}-active-{1,2,3} \
  > .build/performance/pin-summary.json

./scripts/performance/analyze-transcript-signposts.py \
  .build/performance/runs/pin-baseline-active-{1,2,3} \
  .build/performance/runs/pin-feature-active-{1,2,3} \
  > .build/performance/pin-signposts.json

./scripts/performance/analyze-transcript-time-profile.py \
  .build/performance/runs/pin-time-profile-active-{1,2,3} \
  > .build/performance/pin-time-profile.json

./scripts/performance/analyze-thread-state.py \
  .build/performance/runs/pin-cpu-counters-active-1 \
  .build/performance/runs/pin-cpu-counters-inactive-1 \
  > .build/performance/pin-thread-state.json
```

The analyzers report every run and list the excluded ones. They aggregate only
runs whose summaries show a passing scenario, a valid measured phase, archived
evidence, and stable focus. `summarize-runs.py` computes a baseline/feature
delta only for exactly one run of each variant, and only when build
configuration, fixture knobs, fixture source digest, machine identity digest,
phase definition, and workload identity all match. Otherwise it reports the
mismatched fields and leaves the pair incomplete.

Other harnesses:

- **Idle transport/worker comparisons.** Build both variants with identical
  configuration. Run `./scripts/performance/measure-idle.py --app
  /absolute/path/Ottam.app --output /absolute/path/new-run-directory` for each
  member of three alternating before/after pairs. The harness launches a fresh
  background chooser with its own home and settles for 15 s. It then records
  60 s of process counters with an 8 s stack sample, and terminates only the
  app and core it launched. CPU counters are Mach ticks converted with the host
  timebase; interrupt wakeups and package-idle wakeups are reported separately.
  An idle chooser does not exercise an active conversation's payload observer,
  so validate notification delivery and maintenance scheduling with focused
  tests. Exclude activation of the measured app and post-build thermal
  disturbances.
- **Conversation-switch memory.**
  `scripts/performance/measure-switch-memory.sh <cache-capacity> <output-dir>`
  reads the app's footprint after `chat.measure-switch-cache-memory`. Compare
  capacities on the same build.

## 8. Evidence contract

Keep the raw evidence needed to audit every derived number:

- `summary.json` and the environment and source manifests;
- the `.trace` bundle and exported XML;
- `sample.txt` and process snapshots;
- the unified signpost log used to find semantic phase markers;
- scenario output and archived UI-test evidence;
- analyzer output;
- the exact baseline and feature commits;
- a short conclusion that names the excluded runs and why.

`.build` is disposable. Before another UI scenario replaces
`.build/ui-tests/current`, copy evidence that must survive local cleanup to the
feature's agreed archive location. Do not commit large traces or captured
private data.

## 9. Reporting template

Every conclusion states:

1. the hypothesis and the implementation delta;
2. the baseline and feature commits, and their dirty state;
3. the fixture, knobs, build, focus, and trace template;
4. the valid pairs and the excluded runs;
5. per-run and median primary CPU magnitude;
6. the relevant p50/p95/p99/max signpost distributions;
7. stack attribution or scheduler evidence, when needed;
8. correctness validation and the inspected UI evidence;
9. the decision: keep, iterate, or roll back.

A falling internal count does not establish an optimization, because work may
move into preparation, layout, drawing, or framework callbacks. Keep a change
only when the end-to-end metric improves without breaking correctness and
without moving comparable work outside the measured boundary.

## 10. Common traps

- Comparing one run per side.
- Pairing runs captured hours apart.
- Reporting raw CPU seconds when run duration changed.
- Treating `sample` or one Time Profiler slice as total CPU magnitude.
- Summing overlapping inclusive stack categories.
- Treating signpost duration as asynchronous framework cost.
- Trusting the requested focus mode without verifying the frontmost state.
- Silently discarding a focus-drift or setup-failure run.
- Declaring a win from fewer layout passes while total CPU is flat.
- Keeping the only copy of capture or analyzer scripts under ignored `.build`
  paths.
