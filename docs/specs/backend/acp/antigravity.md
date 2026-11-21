---
kind: spec
status: proposed
---

# Antigravity

Add Google's Antigravity CLI (`agy`) as a roster agent, so its Gemini 4
models can be used in Ottam. It ships as a managed binary at an exact pin,
like OpenCode, and supports both a managed profile and the user's own CLI
setup. This is [M11 of the ACP work plan](README.md#m11-chat-with-antigravity).

## Current Status

Not started. The Try it in the work plan awaits approval. Credential
isolation is decided: managed profiles share the Keychain login (see
Decisions).
The claims below were observed live at 1.5.13, with a direct ACP client on
a Mac already signed in to `agy`.

## What The CLI Offers

- **Native ACP.** `agy acp` is a stdio server (protocol 1, `agentInfo.name`
  "Antigravity ACP Server"). There is no npm adapter.
- **`initialize`.** `loadSession`. `sessionCapabilities` holds `resume`,
  `load`, `list`, `close` and `additionalDirectories`. Prompts take images
  and embedded context but not audio. `auth.logout`. No `mcpCapabilities`,
  so assume stdio MCP only. `_meta.antigravity` advertises `tier` and
  `systemPrompt.append`.
- **Auth methods.** `oauth-personal`, `oauth-business`, `gemini-api-key`
  and `agent-platform` (ADC with `GOOGLE_CLOUD_PROJECT` and
  `GOOGLE_CLOUD_LOCATION`). Google's docs add `terminal-login`, offered
  only to clients that declare ACP terminal auth. `agy acp --login` is a
  paste-the-code login that then exits. A session with no credential fails
  with `-32000` "Authentication required".
- **`session/new`.** Models come in the `models` field
  (`availableModels`), and the effort is part of the id: `gemini-4-neon-high`
  and so on, plus Argon, Hydrogen, Orchestrator, and 3.8 Flash. The list
  depends on the account and auth method. Then `available_commands_update`
  (13 commands, including `/compact`, `/goal`, `/plan`), and a
  `usage_update` with a 1,048,576-token window.
- **Home.** State lives in `~/.gemini` (`antigravity-cli/` holds
  conversations, `conversation_summaries.db`, settings, and logs, and
  `config/` holds `config.json` and `mcp_config.json`). `--gemini-dir <path>`
  moves all of it. There is no environment variable for this.
- **Credentials do not follow the home.** OAuth tokens live in the macOS
  Keychain. An empty `--gemini-dir` opened a signed-in session, and the
  log reads "authenticated via keyring". The binary has a file-based token
  store, and `composite_token_storage.go` picks it for four reasons only:
  a detected SSH session or container (an `SSH_CONNECTION` in a scratch
  home switched it), a keyring timeout recorded recently (a time-bound
  marker), an earlier failed keyring write, or "the caller requested it".
  The last one is internal: no flag, environment variable, or `settings.json`
  key sets it. `AGY_ACP_FORCE_FILE_STORAGE=1` has no effect on `agy acp`.
  The Keychain item is service `gemini`, account `antigravity`.
  `security find-generic-password -s gemini -a antigravity` without `-g`
  reads only its attributes, so it raises no access prompt.
- **Inherited API keys.** A `GEMINI_API_KEY` or `GOOGLE_API_KEY` in agy's
  environment switches it to key auth, and its models then reject requests
  with 401. Orca's integration strips both before launching `agy`.
- **Self-update.** The installed CLI updates itself in the background.
  `AGY_CLI_DISABLE_AUTO_UPDATE=true` turns that off. It is undocumented,
  but Orca's integration sets it for every `agy` launch.
- **The standalone ACP server.** Google also publishes `agy-acp-server`,
  the ACP registry's `antigravity-acp` entry: a zip from
  `dl.google.com/agy-extensions/releases` holding `agy_acp_server.par` and
  `localharness_external`. Under `GEMINI_HOME` with
  `AGY_ACP_FORCE_FILE_STORAGE=1`, it keeps its sign-in in
  `<GEMINI_HOME>/antigravity-acp/acp_token.json`. That gives a real
  per-home account, and Orca runs its isolated chats this way. It has
  stayed at 1.5.2 while the CLI moved on: no later version is in the
  registry or at the download URL. Orca calls it "the legacy ACP server".
- **Distribution.** GitHub releases (`google-antigravity/antigravity-cli`)
  are versioned, with sha256 digests in the asset metadata.
  `agy_cli_mac_arm64.tar.gz` and `agy_cli_mac_x64.tar.gz` each hold one
  member, `antigravity` (227 MB unpacked, about 85–91 MB compressed).
  Releases land almost daily. The repo declares no license.

## Next

- [ ] 1. Shared sign-in in the UI (see Decisions, 2026-11-21). A managed
  profile shows the Mac's one Google sign-in and never offers sign-out.
  `session/logout` there would sign the user's own CLI out. Sign-in runs
  only when no working login exists, because a new login replaces the
  CLI's token. Add a home-is-not-an-account note to the add-agent sheet and to the
  agent's Account row. Purge deletes only the managed home's files.
  - Recovery: the Keychain item's attributes prove only that a login
    exists, not that it works. A session that fails with `-32000`
    "Authentication required" marks the profile signed out, whatever the
    Keychain shows, and offers Sign in (the terminal login, with the home
    argument). That one path covers both failure states. With a revoked
    token, the login replaces it, which also repairs the user's CLI. If
    `agy` fell back to the file store (a recent keyring timeout or a
    failed keyring write), the login writes the token into the managed
    home, but only while that reason still holds. The timeout marker
    expires, and then the login may write to the Keychain and replace the
    CLI's token. Ottam cannot choose the store, so the Sign in copy says
    it may replace this Mac's Google sign-in. Neither path signs anything
    out.
  - Verify with: a core test that sign-out is refused on a managed
    Antigravity instance, a core test that an auth-required session marks
    it signed out with the Keychain item present, and the Try it's sign-in
    step.
  - Upstream ask: a supported switch (a flag or a `settings.json` key)
    for the file token store under `--gemini-dir`. That would make managed
    homes real accounts. Ask the user before posting.
- [ ] 2. Core: a home passed as a launch argument. `HomeVariable` and
  `ExistingSetup.variables` only set environment variables, and `agy`
  takes `--gemini-dir`. Add `home_arguments` to `AgentDefinition`, using
  the existing `HomeTarget`. Apply it to the ACP launch and to the terminal
  login. An existing layout at the default `~/.gemini` passes no flag. One
  at another path passes it.
  - Verify with: unit tests in `agent/instances.rs` for managed, existing
    default, and existing custom paths. The login command carries the flag.
- [ ] 3. The definition in `agent/registry.rs`. Binary lane, version
  `1.5.13`. The two mac artifacts use their GitHub sha256 digests, with
  `BinaryArchive::Tgz { member: "antigravity" }` installed as command `agy`.
  Launch is `agy acp --disable-telemetry` plus the home argument. The
  launch environment unsets `GEMINI_API_KEY` and `GOOGLE_API_KEY` until
  API keys land in Account.
  Presentation: product "Antigravity", vendor "Google", `vendor_home`
  `~/.gemini`, a new `google` brand and icon, and `AccountKind` for a
  single account. Sign-in proof is `SignInProof::Session`, since there is
  no `_auth/status_update`. Terminal login is `agy acp --login`
  (`TerminalLoginReveal::Immediate`, since it prints a URL and waits for a
  code).
  - Re-download both tarballs and check the digests before committing the
    pin. The asset `digest` field is GitHub's, not Google's.
  - Verify with: `./scripts/test-rust-core.sh --lib --filter registry`.
    The managed install handshakes and probes the catalog.
- [ ] 4. Hold the pin. Set `AGY_CLI_DISABLE_AUTO_UPDATE=true` in the
  launch environment, as OpenCode's definition sets
  `OPENCODE_DISABLE_AUTOUPDATE`. Every launch carries it, login included.
  - Done when: after a long session and a relaunch, the managed binary's
    sha256 and `agy --version` are unchanged.
- [ ] 5. Existing setup. Add an `"agy"` arm to `existing_setup` with no
  variables. Detection reads `~/.gemini/antigravity-cli` and the Keychain
  item's attributes (see above), and never launches `agy`. The sheet warns
  that the pinned binary may be older than the user's self-updated CLI and
  shares its conversation database. Copy the wording of OpenCode's database
  warning.
- [ ] 6. Record fixtures: the `initialize` answer as
  `agent/fixtures/agy-acp-1.5.13.json`, and a `session/new` answer, per the
  roster convention.
- [ ] 7. Model picker. Ids carry the effort (`-low` … `-xhigh`), so
  check whether the picker should group by family with effort as the
  thought level, or list each id flat. Check whether `agy` also sends
  `configOptions` for thinking level, which Google's docs describe. Decide
  from the recorded `session/new`.
- [ ] 8. Terms-of-service guardrail. A forum answer, not confirmed as
  Google's, says outer harnesses that inject system prompts, tool
  definitions, replayed history, or automated loops on consumer sign-in
  risk suspension. Core must never send `_meta.antigravity.systemPrompt`.
  Add a test that a session request to `agy` carries none. Read
  antigravity.google/terms before M11 ships, and record anything that
  changes this rule.
- [ ] 9. MCP. With no `mcpCapabilities`, check that Ottam-managed MCP
  servers reach `agy` over stdio. HTTP servers would need
  `--process-per-session`. Leave that out of M11 unless the check needs it.
- [ ] 10. Reopen with `session/resume`. Google's docs say resume skips the
  history replay, and Orca prefers it because `session/load` replays the
  whole conversation. Check which one core sends for `agy` on relaunch, and
  that the transcript isn't duplicated.

## Backlog

- API-key sign-in through Account, once
  [roster › API keys in Account](agent-roster-compatibility.md#backlog)
  lands. `gemini-api-key` reads `GEMINI_API_KEY` at launch, so it stays
  hidden until then.
- Usage: `agy` sends `usage_update`. Plan quota (`/usage`, `tier` meta)
  belongs to [usage-and-cost](usage-and-cost.md).
- Subagents and background tasks (`/teamwork`, `/boost`). Record their
  wire shape before mapping them in [subagent-sessions](subagent-sessions.md).
- Isolated managed accounts, by whichever trigger fires first:
  - `agy` gains a supported file-store switch. Managed profiles keep
    `agy acp` and set the switch with `--gemini-dir` at the profile. This
    is the preferred path.
  - Google ships the standalone ACP server past 1.5.2 while `agy` still
    has no switch. Reconsider that server for managed profiles, with
    `GEMINI_HOME` at the profile and `AGY_ACP_FORCE_FILE_STORAGE=1`.
    Existing setups keep `agy acp`. Its known quirks, from Orca's
    integration:
    - it unpacks about 1 GB per start
    - newer builds write a workflow skill into the workspace unless the
      session sends `_meta.antigravity.disabledTools: ["run_workflow"]`
    - sign-in prints its URL through `BROWSER`

## Decisions

- 2026-11-20: Antigravity installs as a pinned managed binary from the
  GitHub release tarballs, not the auto-updating install script. Reason:
  Ottam must own the version it was tested against.
- 2026-11-20: Antigravity supports both a managed profile and the user's
  existing CLI setup, like the other roster agents. Reason: the user's own
  setup carries their plugins, skills, and Google sign-in.
- 2026-11-20: Use the native `agy acp`, not a community adapter. Reason:
  the community adapters wrap the TUI or read Antigravity's private
  database, and the native server covers the protocol.
- 2026-11-21: Managed profiles share the Mac's Keychain Google login and
  isolate state only, as oh-my-pi shares `~/.omp`. Reason: the spike found
  no clean way to point the file token store at a managed home. Faking an
  SSH session would leak into agent shells, and the timeout marker
  expires. Rejected: API key or ADC only (a plan subscriber couldn't use a
  managed profile). That path stays open once API keys land in Account.
- 2026-11-21: Both layouts run `agy acp`. Google's standalone ACP server
  stays out of M11, though it isolates sign-in per home. Reason: it has
  stopped at 1.5.2 and is a second, heavy binary. Orca uses it for
  isolated chats and calls it legacy.
