# Ottam UI Testing

This directory contains Ottam's macOS UI scenario runner. It combines
code-registered Swift scenarios, isolated application identities,
deterministic fixtures, AX actions and semantic trees, ScreenCaptureKit window
evidence, accessibility audits, typed failures, and exact cleanup.

Most UI decisions should be covered by native Swift tests. Real-window testing
has two public surfaces:

```sh
./scripts/smoke-app-bundle.sh
./scripts/ui-test.sh run <scenario-id>
```

The bundle smoke is the single complete visual inventory. It builds the real
bundle once and captures the chooser, Chat, Settings App, Settings Agents, one
representative agent detail, Settings MCP Servers, Settings Terminal, and
Settings Storage (`SmokeScenario.swift`).
The workspace uses an isolated minimal Git repository with one stable
diff, and Settings captures require page-specific visible AX text in addition
to the Settings-window identity.
Use it for packaging/resource/signing changes, changes to that page inventory,
or when the user explicitly requests the complete visual run.

Use `run <scenario-id>` for one specifically affected workflow. Agents should
run the smallest relevant validation and must not batch scenarios for generic
confidence. Multiple real-window scenarios are appropriate only when multiple
behaviors are independently affected or the user explicitly asks.

The operational commands are:

```sh
./scripts/ui-test.sh list
./scripts/ui-test.sh native
./scripts/ui-test.sh preflight
./scripts/ui-test.sh request-permissions
./scripts/ui-test.sh run chat.queue-drain
./scripts/ui-test.sh archive queue-layout
./scripts/ui-test.sh queue --json
```

`native` runs the UI harness, coordinator, scenario-policy, registry, and
accessibility XCTest contracts without launching the product. Packaging checks
use an internal `ui-test.sh smoke --source-app <bundle>` entry point so the
exact bundle they built is exercised without a second product build: the
override replaces the cached isolated source app for that run, and the
`application_staged` record names the source bundle that was staged. Agents
should invoke `smoke-app-bundle.sh`, not the internal smoke command.

## Evidence

Each real-window command replaces the worktree-local evidence bundle:

```text
.build/ui-tests/current/
```

It includes `manifest.json`, `result.json`, `scenario.json`, action records,
logs, diagnostics, real-window PNGs, semantic trees, and accessibility audits.
Runs in one worktree are serialized because they share `current/`; different
worktrees remain independent.

A run that does not complete successfully writes `STALE.txt` into the bundle.
Its presence means the screenshots and audits there are partial or left over
from an earlier run, so they never read as a fresh passing result.

Before starting another run, preserve useful evidence with a semantic name:

```sh
./scripts/ui-test.sh archive claude-login
```

Archives live at `.build/ui-tests/archive/<name>/`. Names are descriptive
kebab-case and never dates or opaque IDs. Archiving refuses to overwrite an
existing name and omits rebuildable or sensitive internals (`home`, `tmp`,
`control`, `staged-app`, and `DerivedData`).

Each `ScenarioCaptureRequest` chooses a named `SemanticSnapshotPolicy` and an
`AXAuditPolicy`. The manifest records both policies, violations, the audit
path, and blocking/excepted issue counts. Audit exceptions must name one rule,
target one non-empty identifier, and include a non-empty reason.
Route policies may also require visible accessible-text fragments when SwiftUI
does not preserve a stable identifier on a semantic heading.

`result.json` records the compatible top-level `classification` plus
`primaryFailure` and `secondaryFailures`. Failures after launch attempt an
independent postmortem screenshot and bounded whole-application AX tree before
cleanup. Build, signing, preflight, and source-app preparation failures still
write typed evidence.

## Adding a scenario

Scenarios are ordinary Swift definitions under
`Sources/OttamUIScenarios/Scenarios`. A scenario supplies its descriptor
and launch configuration, optional preparation, and user journey. Through
`UIScenarioContext`, it can create typed element references; wait for existence,
enabled, focused, or value conditions; press, focus, or set values; group named
steps; make assertions; record activities; and request captures.

Register a scenario once in `ScenarioRegistry`. The shared runner supplies
staging, isolated state, deterministic fixtures and endpoints, readiness,
ScreenCaptureKit and AX evidence, logs, classification, and exact cleanup.
Adding a scenario must not add a Bash launcher or a second evidence path.

List and run scenarios individually:

```sh
./scripts/ui-test.sh list
./scripts/ui-test.sh run workspace.core-replacement
./scripts/ui-test.sh run chat.auth-recovery
```

The tool/driver build and prepared source app are cached below
`.build/ui-testing/runtime`. Cache fingerprints cover source, project,
resources, toolchain, and signing inputs. Every execution still creates a
uniquely identified per-run app clone. Successful cleanup removes disposable
internals; failures retain useful diagnostics. Set
`OTTAM_UI_TEST_RETAIN_INTERNALS=1` only for an explicit debugging run.

The running app is named **Ottam Test** from the primary checkout, or
**Ottam Test · \<worktree\>** from a linked worktree. The shared
`scripts/app-display-name.sh` helper uses the same directory-based labels as
the dev launcher, capped at 12 characters (11 plus `…` for longer names).
Scenario names and run IDs remain in evidence, not the visible name. The
manifest records `displayName` alongside the full `worktreePath`. Internal
bundle IDs, staged paths, executable names, and exact cleanup retain their
unique worktree/run identities. Backend and driver names are unchanged.

## Permissions and signing

The stable signed driver owns local Accessibility and Screen Recording consent:

```sh
./scripts/ui-test.sh request-permissions
./scripts/ui-test.sh preflight
```

If preflight reports `environment_unavailable`, grant the stable app it names
in System Settings. The harness never edits a TCC database.

The runner selects a locally installed `Apple Development` identity and derives
its team from the certificate's `OU`. Machines with multiple identities can
select one explicitly:

```sh
OTTAM_UI_TEST_SIGNING_IDENTITY="Apple Development: Name (ID)" \
  ./scripts/ui-test.sh preflight
```

`OTTAM_UI_TEST_SIGNING_IDENTITY` also accepts the SHA-1 hash.
`OTTAM_UI_TEST_DEVELOPMENT_TEAM` may be supplied as a matching cross-check.
For direct Xcode builds, copy
`Configuration/CaptureSigning.local.xcconfig.example` to the ignored local
configuration and fill in local values.

Missing Accessibility or Screen Recording access, a locked screen, or a
non-console GUI session is `environment_unavailable`; it is not a product
failure and must not be replaced with detached or in-app pixels.

## Project generation

`project.yml` is authoritative. The committed
`OttamUITesting.xcodeproj` is generated for discoverability and direct
Xcode use. Edit `project.yml`, regenerate with XcodeGen, and never inspect or
hand-edit generated project internals. CI regenerates and rejects drift.

Current testing policy and agent expectations live in `AGENTS.md`; deferred
system follow-ups live in `docs/specs/operations/macos-ui-validation.md`.
