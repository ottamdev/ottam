---
kind: spec
status: active
---

# Agent Runtime And Setup Acceptance

Goal: a fresh Mac installs and uses Ottam's agents without a
developer-provided Node installation.

## Current Status

Runtime discovery, managed Node, pinned adapter installs, sign-in, and the
first-run setup sheet ship; the contract is
[ACP › Runtime Resolution And Installation](../../architecture/backend/acp.md#runtime-resolution-and-installation)
and [§ Managed Adapter Lifecycle And Setup UI](../../architecture/backend/acp.md#managed-adapter-lifecycle-and-setup-ui).
This spec owns the acceptance runs, none of which has happened on a fresh
machine or against live providers.

Known gap: **Get** in Settings › Agents is enabled for an uninstalled
Node-launched adapter whatever Node's state; core then fails the install
with "a working Node runtime is required to install the adapter". Tracked
as item 4 in [agent onboarding](../app/agent-onboarding.md#next).

## Remaining Acceptance

- [ ] **First install without developer runtimes.** Launch a packaged app from
  Finder in a clean macOS user with no Node, version managers or Ottam state.
  Install managed Node and one pinned adapter, sign in, complete a prompt.
  Record architecture, OS, app build, resolved runtime/adapter versions,
  Git/Command Line Tools availability, screenshots and AX.
- [ ] **Real-provider lifecycle.** For each built-in pin: signed-out and
  expired-auth states, sign-in dismissal/reopen, cancellation, restart,
  reinstall, both removal choices, and cold-launch readiness after reopen. Use
  isolated test homes and accounts; never remove a user's normal agent home.
- [ ] **Download and runtime repair.** Interrupted download, network failure,
  incomplete system toolchain, older system Node with a managed install
  present, broken `OTTAM_NODE`. The UI reports the failure and the next attempt
  recovers. Test installed-package launch separately from provider network
  access.
- [ ] **Record actual limitations.** Running adapters keep their old launch
  definition until restart. Record observed behavior without claiming live
  runtime replacement.

Regression coverage (not a substitute for the runs above):
`tests/jsonl_protocol/{managed_runtime,agent_install,agent_auth}.rs`,
`agent/registry.rs`, `agent/auth.rs`, Swift `AgentSetupStatusTests`,
`AgentAdapterInstallTests`, `EngineRuntimeAvailabilityTests`, and the
`chat.no-usable-engine` scenario.

## Dependency Catalog

| Dependency | Resolution | Acceptance concern |
| --- | --- | --- |
| Node (`node`, `npm`, `npx`) | `OTTAM_NODE`, shell/common-location discovery, or managed Node (`agent/managed_node.rs`). Claude 22+, Codex 18+. | Finder launch with no system Node; incomplete and incompatible toolchains. |
| Claude / Codex ACP | Installed exact pins from `agent/registry.rs`. | Real install, sign-in, prompt, restart, repair. Terminal-login Done/reopen if an adapter advertises a terminal login. |
| OpenCode / Oh-My-Pi | Pinned prebuilt binaries; no Node. | Same lifecycle as above. |
| Network, nodejs.org, npm registry | Runtime/package downloads only; launch uses installed packages. | Separate download failures from vendor auth/connectivity failures. |
| Git | System Git, `OTTAM_GIT` override. | Behavior without Command Line Tools; managed Node does not provide Git. |
| Login shell | User shell; terminal falls back to `/bin/zsh`. | Shell init and agent child PATH in a normal user environment. |
| Ghostty resources, terminfo | Bundled by `build-app.sh`, which fetches the pinned Ghostty artifact itself and rejects missing resources. | Packaged terminal on the acceptance machine. |
| `ottam-core` | Bundled binary; `OTTAM_CORE_PATH` overrides in development. | The bundle launches its own core. |
| SQLite | Bundled via exact `libsqlite3-sys`. | None. |

Node is an agent runtime dependency, not a build dependency. Signing,
notarization and updates live in the [distribution spec](../app/distribution.md).

## Backlog

- Direct downloadable ACP executables could replace npm installs if a provider
  publishes a suitable artifact. Recheck upstream availability when picked up.
- Adapter updates between app releases stay out of scope; pins move with the
  app and an older install upgrades automatically.
- Account identity display waits on a supported adapter surface. Never parse
  private vendor credential files to get it.

## Decisions

- 2026-10-04: one setup sheet from the landing sequences Node before adapter
  installation; nothing installs automatically on a fresh machine, and the
  workspace chooser has no setup step.
- The managed install layout stays under `OTTAM_HOME`, not Application Support.
