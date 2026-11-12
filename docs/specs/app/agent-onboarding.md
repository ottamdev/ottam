---
kind: spec
status: active
---

# Agent Onboarding

## Current Status

Read-only detection and the first-run setup sheet have shipped for Codex,
Claude Code, OpenCode, and Oh-My-Pi. The landing's **Set Up an Agent…**
opens `AgentSetupSheet` (`firstRun: true`), which shows the Node runtime
first when the chosen agent needs it and keeps Continue disabled until Node
is ready. Item 4 closes the remaining ordering gap in Settings › Agents.

Core semantics (existing-setup layout, guardrails, claiming) live in
[acp.md](../../architecture/backend/acp.md) § Managed Adapter Lifecycle And
Setup UI. Clean-Mac acceptance belongs to
[agent runtime setup](../backend/agent-runtime-setup.md#remaining-acceptance),
and Claude's 72-hour shared-home coexistence gate to
[agent instances](../backend/acp/agent-instances.md) item 4.

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
    (OpenCode, Oh-My-Pi) are unaffected. The setup sheet's order stays.
    Nothing installs automatically (Decisions).
  - Done when: with no usable Node, Settings › Agents shows Get disabled,
    with the reason, for Codex and Claude Code; installing managed Node
    enables Get; OpenCode's and Oh-My-Pi's Get stay enabled throughout.
  - Verify with: `./scripts/test-app.sh --filter AgentAdapterInstall` with a
    runtime that is not ready; `./scripts/ui-test.sh run
    onboarding.missing-node` for the unchanged sheet; a real-window Settings
    check with screenshots plus AX.

## Backlog

- Oh-My-Pi detection (`setup_detection::inspect_omp`) has no unit test and
  reports no skill, hook, MCP server, or plugin counts. `detect`'s error for
  a harness without existing-setup support still names only Codex, Claude
  Code, and OpenCode (`setup_detection.rs`).
- Neither OpenCode nor Oh-My-Pi has detected a real CLI setup in the real
  window.
- The path from a signed-out existing setup through a terminal `codex login`
  to a recheck has not been walked in the real window.

## Decisions

- 2026-10-09: the setup sheet recommends and preselects an **Ottam profile**;
  "Use my CLI setup" reuses the CLI's sign-in, hooks, skills, MCP servers,
  and plugins. Both use Ottam's pinned adapters. Supersedes the 2026-10-04
  default of reusing a detected setup.
- 2026-10-04: first run is one sheet from the landing, in order: Node, the
  per-harness choice, install, sign-in, as Mail's account setup does.
  Rejected: fixing the order inside Settings › Agents alone (setup stays
  undiscoverable) and installing and attaching automatically at first launch
  (installs software and attaches to the user's real account without asking).
