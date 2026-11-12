---
kind: spec
status: active
---

# Agent Roster And Client Compatibility

## Current Status

The roster is Claude Code, Codex, OpenCode, and oh-my-pi, all shipping at
the pins in `agent/registry.rs` (see
[acp.md](../../../architecture/backend/acp.md) § Runtime Resolution And
Installation). Two existing-setup walkthrough steps are still open, for
OpenCode and oh-my-pi
([work plan › Open checks](README.md#open-checks-on-shipped-milestones)).
Whether to add Grok Build is an open decision.

Label adapter claims "source-verified at <version>" (read from source, no
live session) or "observed live at <version>". Re-verify any claim that no
recorded fixture in `agent/fixtures/` covers before relying on it.

## Next

- [ ] 7. Grok Build: go or no-go.
  - The user decides; nothing schedules it. Its condition was items 1–6
    verified on the four roster agents. Only the two existing-setup
    walkthrough steps above remain.
  - If go, it is one definition plus a recorded fixture. Source-verified at
    1.0.30: native ACP; `loadSession`, `session.list/resume/
    close`, `set_mode`, `set_model`, `set_config_option`, `plan`; no
    `prompt.image`. Usage comes as `x.ai/session/usage`, not
    `usage_update`. Auth is agent-managed with no `_auth/status_update`.
    The npm `postinstall` unpacks a platform binary into `$GROK_HOME/bin`,
    so treat it as binary-lane with `GROK_HOME` at the managed home.
  - Before redistributing, settle the license: the repo and npm say
    Apache-2.0, the ACP registry says proprietary.
- [ ] Pick which `AgentOnly` rows to close first. The trigger (all four
  `initialize` answers recorded as fixtures) has fired. `set_config_option`
  overlaps [live-session-configuration](../live-session-configuration.md).
  `usage_update` belongs to [usage-and-cost](usage-and-cost.md). `fs/*` and
  `terminal/*` are deliberate non-goals.

## Backlog

- **API keys in Account.** Proposed 2026-10-13. Today a key reaches an
  agent only through its terminal login. OpenCode without a connection
  shows free models only. ACP can't carry a key: OpenCode's one auth
  method's `authenticate` is a no-op.
  - UX: **Add API key…** under Account opens a sheet with a provider picker
    and a secure field; the user never types a variable name. Model
    providers lists each connection (provider, and stored key or
    environment) with Remove.
  - A (OpenCode): fill the picker from OpenCode's `integration.list` and
    save through `integration.connect.key`, which writes the profile's
    database where terminal login writes. Ottam keeps no copy. (`opencode
    auth login` needs a TTY.)
  - B (fallback for Claude, Codex, likely oh-my-pi): store the key in the
    macOS Keychain and set the provider's variable on the agent process at
    launch. Settings reports it from the Keychain entry.
  - Rules: a key never goes in argv, core's SQLite, logs, or the
    transcript. Under A, an existing setup's own OpenCode database is
    written, and the sheet says so.
  - Spike first: how core reaches the profile's server for
    `integration.connect.key` without the key in argv (a loopback
    `opencode serve`, or a stdin body for `opencode api`). Record the
    result in Decisions.
  - Try it: add an OpenCode Go key from Account. The row shows OpenCode Go
    · key, and a new conversation's model picker lists Go's models. Remove
    it, and the list drops back to free models. `ps` and the logs never
    show the key.
- **OpenCode background gap.** Slash commands with `subtask: true`, or a
  subagent-mode agent, always background their child
  (`config/plugin/command.ts`), which Ottam's plugin can't reach. Trigger:
  a user hits an invisible follow-up.
- **Ottam-owned ACP package on pi's SDK.** Contingency only, if omp's ACP
  fails across three pin bumps: a small TypeScript package on
  `@agentclientprotocol/sdk` and pi's SDK. Prior art: `@automatalabs/pi-acp`
  and Prime Agent's `modes/acp/`. Never a fork of omp.
- **omp notes** (source-verified at 18.1.20; re-check at the pin):
  - Under ACP, omp ignores the user's `~/.omp` values for its
    host-defaulted settings (`HOST_DEFAULTED_SETTING_PATHS`: task isolation,
    worktrees, concurrency, per-agent models). The host drives them.
  - The first `available_commands_update` is delayed
    (`ACP_BOOTSTRAP_RACE_GUARD_MS`), so don't assert it arrives before
    `session/new` settles.
  - `sessionCapabilities.fork` is advertised but untrusted; core doesn't
    use `session/fork`.

## Decisions

- 2026-09-13: the roster is Claude Code, Codex, OpenCode, oh-my-pi. Pi was
  removed on 2026-10-05: pi upstream refuses ACP and `pi-acp` is
  unmaintained. Prime Agent is ruled out (no `loadSession`, never asks
  permission, one session per process, unsigned install).
- 2026-09-13: presentation lives in the Rust definition, not a data file,
  because the wire quirks in `metadata.rs`, `refresh_lock.rs`, and
  `steering.rs` can't be data anyway.
- 2026-09-13: the compatibility table is a static client constant joined
  with the advertised capabilities, and it is the same table that builds
  `initialize_request()`. Every capability flip in the ACP specs is a row
  change there.
- 2026-09-14: the compatibility matrix is a sheet opened from a summary row
  in Settings' Adapter section, not inline, because ~60 rows would push
  Account and Home below the fold.
- 2026-10-05: default-pick order is registration order, not a
  `default_rank` field. Icon brand keys name vendors, so every instance of
  a harness shares a tile.
- 2026-10-13: OpenCode downloads the npm platform tarball with a pinned
  sha256, because v2 publishes no GitHub release assets. omp uses its
  signed release assets.
- 2026-10-13: OpenCode's background work is disabled, not surfaced. Neither
  OpenCode nor omp forwards the turn it resumes after background work.
  omp's async `bash` is left on: under ACP it holds a late result for the
  next prompt (observed live at 18.11.1), so nothing is lost.
- 2026-10-13: OpenCode child sessions use the default tagged projection,
  rewritten into native subagent sessions.
  `opencode/child-session-updates` stays unadvertised.
- 2026-10-13: an existing OpenCode setup is allowed, with a warning that
  v2 migrates the shared `opencode.db` in place.
