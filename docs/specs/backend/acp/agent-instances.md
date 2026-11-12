---
kind: spec
status: active
---

# Agent Instances

## Current Status

Instances ship ([ACP work plan](README.md) M4 and M5); the contract is in
[acp.md](../../../architecture/backend/acp.md) § Instances And Homes. One
gate is open: Claude's 72-hour coexistence check on a shared home.

## Active

- [ ] 4. Claude existing-setup coexistence gate.
  - What: at least 72 hours of ordinary Ottam use on `~/.claude` beside
    terminal Claude. It must include restarts and a real credential-refresh
    cycle, with no unexpected re-login.
  - Why: a shared home is what once forced a daily Claude re-login. The
    refresh-lock guardrails are only proven by synthetic locks.
  - Done when: the run completes and its record shows the refresh. Time
    alone does not satisfy the gate, and a merge does not establish it.
  - Record: timestamps, restart and refresh outcomes, and the
    unexpected-login count. Never record credential values. Never
    manufacture or remove a lock in the real CLI home.
  - Verify with: `./scripts/inspect-core.sh call agent.list '{}'`, plus the
    core logs (`./scripts/logs-app.sh --backend`).

## Next

- Per-instance defaults (model, mode, thought level) applied at
  `session/new`. Precedence: explicit request, then instance default, then
  vendor default. Picked up in
  [live-session-configuration](../live-session-configuration.md), not here.
- Ask OpenCode upstream to document `OPENCODE_DB` and to add a data-home
  override. Ask the user before posting.

## Backlog

- Packaged agents: `agent.create` takes an optional `package { source, ref }`
  that seeds a new managed home from a package. Export reads any home and
  never writes there. The format is decided (2026-09-13: a harness-native
  overlay, amended 2026-10-10 to reference marketplace plugins by ref);
  publishing and trust rules are not.

## Decisions

- 2026-09-13: two newtypes, `HarnessId` and `AgentInstanceId`, so the
  compiler refuses a harness operation called with an instance id.
- 2026-09-13: the routing key stays single. `engine_id` is the instance id,
  and seeded instances keep their legacy ids (`codex`, `claude`), so no data
  migration. Rejected: a separate profile axis with composite keys, and
  env-var-only profiles with no core model.
- 2026-10-10: instance ids are `<harness>.<UUID>`, immutable and never
  reused; labels are mutable.
- 2026-09-13: home creation fails closed. There is no fallback to the
  vendor's shared home, which would leak one account into another.
- 2026-09-13: delete is a soft delete with a tombstone; purge is separate
  and explicit. A conversation on a deleted instance reports the removal
  and never reroutes.
- 2026-10-04: oh-my-pi isolates by `PI_CODING_AGENT_DIR` with
  `OMP_PROFILE=""`, not by omp's named profiles, which live inside
  `~/.omp` and would make purge delete there. Accepted cost: the `~/.omp`
  root (`.env`, plugins, worktrees, logs) stays shared.
- 2026-10-13: a managed OpenCode profile also points `XDG_DATA_HOME` and
  `XDG_STATE_HOME` inside its home. Ottam's plugin restores the user's
  values for shell commands.
- 2026-10-04: an existing-setup layout sits beside managed homes, because a
  separate home hides the user's hooks, skills, MCP servers, and plugins.
  Guardrails: no probes, no lock cleanup, no force-close mid-refresh, no
  sign-out, no purge.
- 2026-10-09: onboarding recommends and defaults to a managed profile.
- 2026-10-04: seeds are unconditional, and an unclaimed seed's layout can
  be set; onboarding sets it. An instance is claimed once a conversation run
  or agent session references it.
