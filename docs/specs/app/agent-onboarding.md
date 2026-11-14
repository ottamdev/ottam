---
kind: spec
status: active
---

# Agent Onboarding

## Current Status

Read-only detection has shipped for Codex, Claude Code, OpenCode, and
Oh-My-Pi. The Onboarding window below replaces the first-run setup sheet,
which is removed; Settings › Agents adds a CLI setup with "Add My CLI
Setup…". What remains is real-window verification.

Core semantics (existing-setup layout, guardrails, claiming) live in
[acp.md](../../architecture/backend/acp.md) § Managed Adapter Lifecycle And
Setup UI. Clean-Mac acceptance belongs to
[agent runtime setup](../backend/agent-runtime-setup.md#remaining-acceptance),
and Claude's 72-hour shared-home coexistence gate to
[agent instances](../backend/acp/agent-instances.md) item 4.

## Active

### Onboarding window

On first launch a dedicated window plays the brand intro, then walks the user
through choosing agents, installing them, signing in, and creating a named
agent. The Welcome window opens once onboarding is complete.

Try it:

1. Launch with a fresh `OTTAM_HOME`. Only the Onboarding window opens, not
   the Welcome window. The thread draws in, the wordmark fades in below it,
   and after about 3 s it crossfades to "Choose your agents". With Reduce
   Motion on, the mark is static and the wordmark appears without the fade.
2. Codex, Claude Code, OpenCode, and Oh-My-Pi are switch rows, all off. An
   agent switched on with a detected CLI setup gains a Profile row (Ottam
   profile by default, or My CLI setup). Its caption reads "Separate from
   ~/.codex" or "Shares ~/.codex", with the setup's skill, MCP server, hook,
   and plugin counts.
   Continue stays disabled until at least one agent is checked. The window
   has no close button.
3. Tick Codex and Claude Code, then Continue. Node installs first, with a
   progress row, then both adapters install in parallel. A failed row offers
   Retry and Remove. Continue is enabled when every remaining row is ready.
4. Sign in. Codex opens the browser. Claude shows a terminal only if its
   login stalls. A row using a CLI setup that is already signed in starts
   ready. Continue is enabled when every row is signed in.
5. Add named agents. Suggested rows show reviewer, implementer, planner, and
   debugger, each with its tile, a one-line summary, and Add. Add creates it
   on the first chosen agent in the list's order (implementer in the agent's
   default mode, so its Add waits until the agent lists its modes; the rest
   read-only) and moves it to the Named Agents list below, the same list
   and edit sheet as Settings › Agents. "Add Named Agent…" opens a blank
   sheet. Its Model menu lists the agent's own models, for an Ottam profile
   and for My CLI setup alike. Finish is enabled once one exists.
6. Finish. The Onboarding window closes and the Welcome window opens.
   Settings › Agents lists the named agent.
7. Quit during install, then relaunch. Onboarding reopens at Install with
   the same selection, and finished installs stay done. Cancelling an
   install leaves that row with Retry and Remove. Back, beside Continue
   from step 2 on, returns one step; installs already started keep running.
8. Relaunch after finishing. The Welcome window opens directly. A home
   created before onboarding existed never sees it.
9. Settings › Agents › Codex: "Add My CLI Setup…" adds the detected
   `~/.codex` instance and opens its page, where install and sign-in work as
   before. The old setup sheet is gone.
10. Sign out of every agent and open a workspace. The landing offers "Sign
    In to Codex" and starts sign-in directly. With no agent installed it
    offers "Open Agent Settings…".

- [~] 5. Onboarding window, launch gating, and resume state
  (`$OTTAM_HOME/onboarding.json`). Left: the Reduce Motion intro.
  - Done when: Try it steps 1, 7, and 8 pass.
  - Verify with: `./scripts/test-app.sh --filter Onboarding`, and a
    real-window launch with a fresh `OTTAM_HOME`.
- [~] 6. Choose, Install, and Sign In steps. Left: Node install and
  parallel adapters on a fresh home, Retry and Remove after a cancel, and
  OpenCode's provider terminal inside the window.
  - Done when: Try it steps 2–4 pass, including the missing-Node path.
  - Verify with: a real-window run with live providers, once with
    `OTTAM_NODE` pointing at a missing path.
- [~] 7. Named agent step. Left: a real-window run of the suggested rows
  and edit sheet with an Ottam profile (a CLI setup's models are listed).
  - Done when: Try it steps 5 and 6 pass.
  - Verify with: a real-window run and `namedAgent.list` through
    `scripts/inspect-core.sh`.
- [~] 8. Remove `AgentSetupSheet`, add "Add My CLI Setup…", and fix the
  landing's action. Left: the scenario runs below.
  - Done when: Try it steps 9 and 10 pass.
  - Verify with: `./scripts/ui-test.sh run settings.existing-setups` and
    `chat.no-usable-engine`.

## Next

- [ ] 4. Gate adapter installation on Node readiness in Settings › Agents.
  - Why: on an instance page, **Get** for an uninstalled Node-launched
    adapter is enabled whatever Node's state.
    `AgentAdapterInstallPresentation.isActionEnabled` reads only the install
    phase, and core then fails the install with "a working Node runtime is
    required to install the adapter" (`agent_install_service.rs`).
  - Scope: disable Get for a Node-launched adapter while its runtime is not
    ready, with a line pointing at the Node Runtime section (shown whenever
    `AgentsSettingsModel.hasRuntimeProblem`). Prebuilt-binary agents
    (OpenCode, Oh-My-Pi) are unaffected. Nothing installs automatically
    outside onboarding (Decisions).
  - Done when: with no usable Node, Settings › Agents shows Get disabled,
    with the reason, for Codex and Claude Code; installing managed Node
    enables Get; OpenCode's and Oh-My-Pi's Get stay enabled throughout.
  - Verify with: `./scripts/test-app.sh --filter AgentAdapterInstall` with a
    runtime that is not ready, plus a real-window Settings check with
    screenshots and AX.

## Backlog

- The UI-test harness cannot capture the Onboarding window: runs skip
  onboarding unless `OTTAM_ONBOARDING=1`, and `UITestPreparation` has no
  readiness hook for it. Add one with `onboarding.*` scenarios once the
  window's layout settles.
- Oh-My-Pi detection (`setup_detection::inspect_omp`) has no unit test and
  reports no skill, hook, MCP server, or plugin counts. `detect`'s error for
  a harness without existing-setup support still names only Codex, Claude
  Code, and OpenCode (`setup_detection.rs`).
- Neither OpenCode nor Oh-My-Pi has detected a real CLI setup in the real
  window.
- The path from a signed-out existing setup through a terminal `codex login`
  to a recheck has not been walked in the real window.
- Replace the model-catalog probe with ACP's unstable `session/options` per
  agent once it advertises `sessionCapabilities.options`: claude-agent-acp
  has it from 0.106.0 (pinned at 0.100.0); codex-acp, OpenCode, and
  Oh-My-Pi do not yet. It drops the per-adapter cleanup, though Claude's
  implementation still starts the CLI, so hooks still run.

## Decisions

- 2026-11-12: first launch opens a dedicated Onboarding window instead of
  the Welcome window, until onboarding completes. It replaces the setup
  sheet because one guided pass across several agents beats repeating a
  per-agent sheet from the landing.
- 2026-11-12: ticking agents and pressing Continue in onboarding is consent
  to install managed Node, if needed, and those adapters. This supersedes
  the 2026-10-04 rule for onboarding only, since that rule rejected
  installing without asking.
- 2026-11-12: onboarding has no skip. Quitting is the only way out, and the
  next launch resumes. A failed agent can be removed from the selection.
- 2026-11-12: the last step creates a named agent, not an agent instance,
  because the roles users want (reviewer, implementer) carry instructions
  and need no extra sign-in.
- 2026-11-13: the named-agent step suggests the four roles as one-click
  rows and reuses Settings' Named Agents list and edit sheet, because a
  prefilled form on the step read as cluttered.
- 2026-11-13: CLI-setup homes are probed for the model catalog, reversing
  "never probed", because onboarding's named agents need models; the probe
  stores nothing and waits out sign-in refreshes
  ([acp.md](../../architecture/backend/acp.md) § Model Catalog Freshness).
- 2026-11-12: completion lives in `$OTTAM_HOME/onboarding.json`, written by
  the app, so launch decides without a core round-trip. A home without the
  file but with `ottam.sqlite` predates onboarding and counts as complete.
- 2026-10-09: the setup sheet recommends and preselects an **Ottam profile**;
  "Use my CLI setup" reuses the CLI's sign-in, hooks, skills, MCP servers,
  and plugins. Both use Ottam's pinned adapters. Onboarding keeps this
  default per agent.
