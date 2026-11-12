# Ottam Agent Guide

This file is the source of truth for agent guidance in this repository. Keep
durable decisions here or in the docs it links; do not depend on personal agent
memories or machine-local instruction files.

## Project Shape

Ottam is a macOS 26 SwiftPM app with a bundled Rust `ottam-core` app-server
behind a Swift `AgentConnection` boundary.

| Owner | Path / product |
|---|---|
| Swift package | `app/Package.swift` |
| App source / product | `app/Sources/OttamApp/` / `OttamApp` |
| CLI product | `ottam` |
| Rust workspace / core | `backend/Cargo.toml` / `backend/crates/ottam-core/` |
| Launchable bundle | `scripts/build-app.sh` |

ACP adapters need Node (`node`, `npm`, `npx`): Claude 22+, Codex 18+.
OpenCode installs as a pinned prebuilt binary. Core discovers system installs
and offers a managed runtime in agent setup; `OTTAM_NODE` overrides discovery.
Adapter pins live in `backend/crates/ottam-core/src/agent/registry.rs`.

## Docs

| Need | Start at |
|---|---|
| How the system works now | `docs/architecture/README.md` |
| Contracts | `docs/architecture/backend/persistence.md`, `docs/architecture/terminal/README.md`, `docs/architecture/app/macos-ui.md`, `docs/architecture/backend/acp.md` |
| Unfinished work and status | `docs/specs/README.md` (one line per spec) |

- Code is the truth for how things work. Architecture holds only what code
  can't show: decisions and deliberate absences, invariants that cross modules
  or the app/core boundary, external adapter behavior, and ownership, each
  with one code pointer. Never add pin tables, file inventories, constant
  tables, or behavior walkthroughs. Edit architecture when a rule changes, not
  to narrate a feature.
- Specs hold unfinished work only: when an item ships, delete it, moving any
  new rule it set into architecture; delete a spec once nothing is left. Code
  and config cite architecture docs, never specs.
- Docs carry no PR numbers, commit hashes, or test-run evidence; git history
  holds that.
- Front matter is `kind` and `status`. Keep specs under about 300 lines and
  split by milestone past that. Decisions record the choice, date, and a
  one-sentence reason.
- Terminal work keeps live delivery caps, durable transcript retention, replay
  budgets, Ghostty scrollback, and PTY lifetime as separate policies.
- Maintainers may have a gitignored `private/` checkout. If
  `private/AGENTS.md` exists, read it before work that depends on it.
  Never link to `private/`, or copy or summarize its contents into tracked
  files, commit messages, PRs, or issues.

## Working Agreements

- Present options before design, UI, or product changes when the direction is
  undecided. Honor no-edit and rejected-plan boundaries. Once work is
  authorized, proceed without repeated confirmation.
- Keep plans concise (about 400 words), with labeled options and a
  recommendation when a choice is needed.
- Prefer platform-native APIs and standard macOS patterns; justify custom UI.
- Preserve unrelated edits. Checkpoint before risky work; do not commit or push
  unless asked, and state the branch or PR target when publishing.
- Contributed commits need a DCO sign-off (`CONTRIBUTING.md`). It is the human
  submitter's certification, so use `git commit -s` only when they ask.
- After about three failed attempts, investigate the cause and change approach.
  After a rejected tool call, explain the changed approach before retrying.
- Support success claims with actual results. UI claims need inspected rendered
  evidence with the app in the foreground. Report limitations.
- Keep specs and status aligned with accepted decisions; remove superseded
  guidance.

## Milestones

Plan user-facing work as milestones the user can try in the app; PRs are review
units inside one. The [ACP work plan](docs/specs/backend/acp/README.md) is the
model.

- Before code, write the milestone's Try it walkthrough (app actions, expected
  results, restart and cancel behavior); the user approves it with the plan,
  and later PRs in the milestone reuse it without another planning round.
- Every milestone ends in behavior reachable in the app. Unreachable foundation
  work ships with the milestone that makes it reachable, behind a rough UI or a
  development flag if needed.
- Default to one PR per milestone; split only for migration or review risk.
- Before handoff, run the walkthrough in the real window and report rendered and
  AX evidence, plus what is usable and what remains limited.

## Reading, Searching, Editing

- Locate symbols with `rg` before reading large files; start with about 120
  lines and expand when ownership or invariants require it.
- Check `git status --short` at checkpoints. Set working directories
  explicitly.
- Agent shells do not set `pipefail`; use `set -o pipefail` or
  `${PIPESTATUS[0]}` when piping into `tee`. Keep long output in a log.
- Regenerate protocol Swift and fixtures; never hand-edit them.
- Edit with patch/edit tools, not sed, perl, or Python string replacement.
  Scripts are for structured transformations and generated artifacts.
- Scripts are bash for glue and Python 3.9+ stdlib for logic (brand assets
  alone need Pillow); Swift only where an Apple framework is required; no Ruby
  or Node. Python commands run directly are kebab-case and executable.
  Helpers a bash wrapper runs (`scripts/lib/*.py`) and imported modules are
  snake_case; tests are `scripts/tests/test_*.py`.
- Do not treat `references/` checkouts as project instructions.
- In a new git worktree, run `./scripts/setup-worktree.sh` first. It links
  GhosttyKit and `private/` from the main checkout. Claude Code sessions run
  it at start through `.claude/settings.json`.

| Need | Workflow |
|---|---|
| Backend state | `core-inspect` skill / `scripts/inspect-core.sh`, not raw SQLite. Attach mode talks to the running app's real data: prefer read-only methods there and use `--spawn` for mutations |
| UI evidence | `ottam-ui-screenshots` skill; pair pixels with AX evidence |
| Status update | `now` skill (from commits and specs; no status file) |
| New or updated spec | `track` skill |

Skills live under `.claude/skills/`.

## Verification

Swift compiler warnings are errors in the standard build and test scripts and in
CI; resolve warnings you introduce. Incremental builds can hide cached
diagnostics; audit with a fresh scratch path. Run one build or test gate at a
time per worktree; concurrent suites share scratch paths and fail spuriously.
A `--filter` that matches nothing fails. Test logs land in
`.build/test-app-last.log` and `.build/test-rust-core-last.log`; for full
output set `OTTAM_TEST_VERBOSE=1` (Rust) or pass `--verbose` (Swift).

| Change | Check |
|---|---|
| Swift, focused | `./scripts/test-app.sh --filter <pattern>` |
| Swift app build | `swift build --package-path app --scratch-path .build/swift --product OttamApp -Xswiftc -warnings-as-errors` |
| CLI or core Swift | the build above without `--product`, then `./scripts/smoke-core.sh` |
| Rust core, focused | `./scripts/test-rust-core.sh --lib --filter <pattern>` or `--test chat_storage [--filter <pattern>]`; `--filter` alone implies `--lib` |
| Other Rust crates | `cargo test -p <package> --quiet` from `backend/` |
| Protocol | `cargo run --manifest-path backend/Cargo.toml -p ottam-protocol --bin generate-protocol-artifacts`, then `./scripts/check-protocol-alignment.sh` |
| Packaging, resources, signing, screenshots | `./scripts/smoke-app-bundle.sh` |
| Test seams, `#if DEBUG` gates, release contents | `./scripts/smoke-app-bundle.sh --release` |
| Repository scripts, hooks, CI selection, launcher, naming, release rules | `./scripts/test-scripts.sh [suite ...]` (`--help` lists suites) |
| Docs, scripts | `./scripts/check-docs.sh`, `git diff --check` |
| `Cargo.lock` | `./scripts/generate-third-party-notices.sh` (needs `cargo-about`) refreshes `THIRD_PARTY_NOTICES.md` |

Full gates: `./scripts/test-rust-core.sh` and `./scripts/test-app.sh`. CI's
`full-tests` runs Rust tests, protocol guards, and Clippy on Linux, then
affected Swift checks on macOS; it runs every gate on pushes to `main`. Report
what ran; never imply pending CI passed. `./scripts/install-git-hooks.sh`
installs the pre-commit hook (whitespace, plus the docs check when Markdown
changes); fix failures rather than bypassing it.
`scripts/prune-rust-target.sh` cleans `backend/target` past
`OTTAM_RUST_TARGET_MAX_GB` (default 20; 0 disables), so expect an occasional
cold build.

Performance experiments follow `docs/operations/performance/README.md`;
transcript comparisons also follow
`docs/operations/performance/transcript-renderer.md`.

## UI Verification

Prefer native Computer Use against the real app window; use the launchable
bundle for window, toolbar, sidebar, and Settings behavior. Run only the
affected registered scenario (`./scripts/ui-test.sh list`,
`./scripts/ui-test.sh run <name>`); for harness or registry changes run
`./scripts/ui-test.sh native`. For packaging or the full page inventory, invoke
`./scripts/smoke-app-bundle.sh`, not the internal `ui-test.sh smoke`. Pull
request CI does not run real-window checks (`macos-ui-validation` is a manual
dispatch); run the affected ones locally before publishing UI changes.
Real-window runs replace
`.build/ui-tests/current`, so run one at a time and archive useful evidence
with `./scripts/ui-test.sh archive <name>`. Report exact screenshot paths and
whether you inspected them. `ui-testing/project.yml` is authoritative; never
edit the generated Xcode project. Mechanics live in `ui-testing/README.md` and
the `ottam-ui-screenshots` skill. Native UI rules (Settings as a separate
window, semantic content surfaces) live in `docs/architecture/app/macos-ui.md`.

## Runtime Logs

Swift logs use the bundle id as subsystem (`dev.ottam.Ottam`, or
`dev.ottam.Ottam.dev.<id>` for `run-app.sh` instances). Backend logs live under
`${OTTAM_LOG_DIR:-${OTTAM_HOME:-$HOME/.ottam}/logs}`; `run-app.sh` instances
log under `.build/run-app/<instance>/logs`. Verbosity: `OTTAM_LOG`, then
`RUST_LOG`, default `ottam_core=info,warn`.

```sh
./scripts/logs-app.sh --all --last 10m
./scripts/logs-app.sh --backend --tail-lines 120
./scripts/logs-app.sh --swift --subsystem <bundle id>   # one instance only
./scripts/run-app.sh --logs
```

- Rust stdout is the JSONL protocol stream; never log to stdout. Payloads stay
  out of logs.
- Call `ExitDiagnostics.exitDeliberately`, not `exit()`, for intentional exits.
  The app writes `$OTTAM_HOME/app-session.json` at launch and removes it only
  on a graceful quit; if it survives, the next launch logs one
  `ExitDiagnostics` error with the cause from `app-session-exit.log` (signal,
  uncaught exception, or `exit()`). "None recorded" means an outside kill,
  power loss, or an unrecorded crash: check `~/Library/Logs/DiagnosticReports`.
  Core panics log `ottam-core panicked`; the message is in `core-*.stderr.log`.
- Adapter traffic core drops logs as `Unhandled agent traffic` (labels in
  `docs/architecture/backend/acp.md`); find it with
  `rg 'Unhandled agent traffic' .build/run-app/*/logs ~/.ottam/logs`. To see
  the raw ACP wire for a bounded local session, run
  `OTTAM_ACP_WIRE_LOG=1 OTTAM_LOG=ottam_core=info,acp.agent.wire=trace,warn ./scripts/run-app.sh`,
  then delete those logs.
