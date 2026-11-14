---
kind: spec
status: deferred
---

# macOS UI Validation Follow-ups

## Current Status

Native Swift tests own presentation and state branching. The only complete
real-app run is `./scripts/smoke-app-bundle.sh`, which captures seven pages in
one isolated app lifetime (inventory in `ui-testing/README.md`). Behavioral
scenarios run only by explicit ID. The `macos-ui-validation` workflow is
`workflow_dispatch` only. Restore automatic execution is unblocked; every
other item below is trigger-based.

## Backlog

- **Restore automatic execution.** Unblocked: manual runs now execute every
  step and upload their evidence. Run native app tests, native UI-harness
  tests, and the seven-page smoke in one job on one built bundle, and upload
  screenshots, AX evidence, results, logs, and the contact sheet. Do not make
  the workflow a required check until automatic execution ships. A zero-step
  billing or runner failure is unavailable infrastructure, not a product
  result.
- **Measure actual cost.** Trigger: successful automatic runs exist. Record
  duration, app-launch count, foreground activations, and billed minutes. Do
  not set a target from the retired suite's timing.
- **Metadata hardening.** Trigger: route or AX coverage drifts. Add
  declarative page-inventory metadata so a new top-level route or Settings pane
  must update the smoke. Separately, add identifier-scope metadata and audit
  tests if duplicate-identifier exceptions get hard to keep aligned with the
  accessibility identifier catalog.
- **`--repeat N` and flaky-result aggregation.** Trigger: scenario history
  shows a need for stability sampling. Each attempt keeps independent evidence,
  and mixed outcomes stay visible.
- **Optional visual variants.** Trigger: the single appearance is stable and
  evidence shows a real gap. Consider alternating appearance or one manual
  extended run.

## Decisions

- Behavioral scenarios stay manual diagnostic tools. Reassess one only when it
  is stale, costly to maintain, or duplicated by lower-level coverage. Do not
  schedule a blanket catalog reduction, and never select scenarios
  automatically from changed paths.
- Do not restore a light/dark matrix, batch aliases, broad-change fallback, or
  duplicate smoke runs. The retired automatic suite caused 18 foreground
  launches.
