---
name: ottam-ui-screenshots
description: Verify Ottam macOS UI with real-window screenshots paired with AX evidence through scripts/ui-test.sh. Use when changing or reviewing layout, routes, the tools pane, toolbar, sidebar, or Settings, or when asked for rendered UI evidence.
---

# Ottam UI Screenshots

Use this skill when changing or reviewing Ottam macOS UI layout, route panes, split views, toolbar placement, sidebar/list rendering, inspector glass, or safe-area behavior.

## Rule

Use the production Swift UI scenario runner for repeatable route-level and
workflow verification. It pairs real isolated-window pixels with bounded AX
trees, accessibility audits, logs, typed results, and exact cleanup:

```sh
./scripts/ui-test.sh run <scenario-id>
```

The app's screenshot environment variables are private preparation plumbing
owned by the scenario runner. Do not invoke them directly or add scenario
behavior to Bash.

## Commands

Capture the one complete production page inventory with paired semantic
evidence only when packaging, resources, signing, or the page inventory is in
scope, or when the user explicitly asks:

```sh
./scripts/smoke-app-bundle.sh
```

The latest evidence replaces `.build/ui-tests/current`. Before another run,
preserve evidence that matters with a descriptive workflow or feature name:

```sh
./scripts/ui-test.sh archive terminal-layout
```

Archives live at `.build/ui-tests/archive/<name>` and intentionally exclude
isolated home, temporary/control data, staged apps, and Derived Data.

Run a focused registered workflow:

```sh
./scripts/ui-test.sh list
./scripts/ui-test.sh run chat.tools-pane
```

`run <scenario-id>` is the only one of these to reach for while iterating. The
runner has no aggregate real-window suites. Do not run multiple scenarios for
generic confidence; use more than one only for independently affected behavior
or an explicit user request. `./scripts/ui-test.sh native` covers harness and
registry contracts without driving the app.

The bundle smoke builds/signs once, verifies bundle resources, and delegates
its page inventory to an internal `ui-test.sh smoke --source-app` entry point.

## Evaluate

Inspect production screenshots under `.build/ui-tests/current/screenshots` and
pair the visual review with the matching AX tree and audit under
`.build/ui-tests/current/accessibility`. Inspect route screenshots for:

- window chrome and unified toolbar placement
- sidebar selection and `List` rendering
- split-pane sizing and safe areas
- inspector/sidebar material behavior
- route content fitting without overlap

## Troubleshooting

- If `scripts/smoke-app-bundle.sh` fails, inspect `result.json`, `manifest.json`,
  logs, semantic trees, and diagnostics under `.build/ui-tests/current` before
  rerunning.
- If screen lock or Screen Recording availability prevents ScreenCaptureKit
  capture, report the run as environment-unavailable; do not substitute a
  detached or in-app render for authoritative full-window evidence.
- If a plain smoke run is killed by the command harness without output, rerun with `bash -x scripts/smoke-app-bundle.sh` and verify that no `Ottam` process is left running.
- Keep app-side testing support narrow: SwiftUI prepares deterministic product
  state and publishes the attached `NSWindow`; the external driver owns
  pixels, semantics, audits, actions, classification, and cleanup.
