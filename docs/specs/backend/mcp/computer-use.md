---
kind: spec
status: proposed
---

# Computer Use

Goal: an agent in an Ottam chat sees and drives other macOS apps in the
background, with its own cursor. The user's pointer, keyboard focus and
frontmost app are never taken. Ottam asks before an agent touches an app,
remembers the answer per app, and never lets an agent drive Ottam itself.

Ottam writes no accessibility, input or capture code. It embeds
[cua-driver](https://github.com/trycua/cua) (`libs/cua-driver`, MIT, Rust)
and serves it through the [managed MCP proxy](managed-servers.md) on a
reserved `computer` route, like the `ottam` route of
[Ottam's own tools](ottam-tools.md).

## Current Status

Proposed; nothing is built. A trial registered the standalone
`CuaDriver.app` 0.38.0 as a managed stdio server, and a Claude chat drove
Calculator in the background end to end. Its gaps were grants (the
standalone app's, not Ottam's) and prompts (one harness sheet per click).
C1 waits on the four open decisions under Decisions.

## Active

Nothing approved.

## Next

Item ids are `cu:<n>`. Run each walkthrough in the real window before
handoff (`AGENTS.md` › Milestones).

### C1: Drive an app in the background with Ottam's own grants

Shape:

```text
ottam-core ──spawns──▶ cua-driver serve --embedded --socket <private>   (daemon: AX, input, capture, overlay)
core proxy ──stdio──▶ cua-driver mcp --embedded --socket <private>      (stateless MCP to the daemon)
agent ──Streamable HTTP, per-session token──▶ core proxy /mcp/computer
```

- macOS attributes Accessibility and Screen Recording to the top of the
  launch chain. The app starts core with `Process()`
  (`LocalProcessConnection.swift`), and core starts the daemon with
  `CUA_DRIVER_EMBEDDED=1`, which turns off the driver's responsibility
  disclaim. The grants are Ottam's; the daemon never prompts, and
  `check_permissions` reports `source.attribution: "host"`.
- macOS caches TCC answers per process, so the daemon restarts after a
  grant changes.
- Core offers `computer` only when its client is the app
  (`InitializeParams.clientInfo.name`), never under the `ottam` CLI or
  `inspect-core.sh --spawn`, where a terminal tops the chain.
- Permission mode (`standard`, `bounded`, `unrestricted`) and tunables
  (key gap, batch pacing, `--no-overlay`, cursor theme) are fixed at
  daemon launch, never per call.

Try it:

1. Settings › Computer Use starts off. Turn it on: Accessibility and
   Screen Recording show Not granted with Open System Settings, and the
   macOS prompts name Ottam. Grant both: rows show Granted and status
   reads Ready with the driver version. Privacy & Security lists Ottam,
   no CuaDriver.
2. In a Claude chat: "Open Calculator in the background and compute
   12 × 34. Don't bring it to the front." Claude reads the window, clicks
   and reads 408. Calculator stays behind, the agent cursor glides over
   it, and the final tool card shows the window screenshot.
3. Type in another app meanwhile: the pointer and frontmost app never change.
4. Codex and OpenCode chats get the same tools.
5. Revoke Accessibility: the next call fails saying so and Settings shows
   Not granted. Grant again: the next call succeeds without relaunching.
6. Quit: `pgrep cua-driver` is empty. Relaunch: the switch is still on and
   the next call starts the daemon.
7. Turn the switch off: new chats get no `computer` tools, and a running
   chat's next call fails with "Computer use is off".
8. Rebuild with the Apple Development identity: grants survive (an
   ad-hoc build loses them, per `scripts/build-app.sh`).
9. `./scripts/inspect-core.sh call computerUse.get --spawn` reports the
   route unavailable to a non-app client.

- [ ] 1. **Install lane.** A `cua-driver` pin (version, target triples,
  checksums) in `agent/registry.rs`, downloaded and verified on first
  enable into `$OTTAM_HOME/computer-use/<version>/`, like OpenCode's
  prebuilt binary. Needs decision 3. Done when a fresh home ends with a
  verified binary and offline enable fails with a clear state. Verify:
  `./scripts/test-rust-core.sh --lib --filter computer_use`.
- [ ] 2. **Daemon slot and route.** One slot shared by every workspace in
  `mcp/runtime.rs`: spawn `cua-driver serve --embedded --socket
  $OTTAM_HOME/computer-use.sock --permission-mode standard
  --host-bundle-id <bundle id>`, wait for the socket, then run
  `cua-driver mcp --embedded --socket <same>` as the stdio upstream. A
  reserved `computer` route kept by `sync_routes`; idle stop and quit stop
  both processes. Needs decision 1. Done when Try it 2, 6, 7 and 9 pass.
  Verify: `./scripts/test-rust-core.sh --lib --filter mcp::`.
- [ ] 3. **Grants and state.** `computerUse.get` (state, grants, version,
  route available to this client), `computerUse.set {enabled,
  showAgentCursor}`, `computerUse.refresh`, `computerUse.stateChanged`. The
  app calls `AXIsProcessTrustedWithOptions` and
  `CGRequestScreenCaptureAccess()`; core restarts the daemon on `refresh`
  and on a missing-grant failure; add `NSScreenCaptureUsageDescription`
  and `NSAppleEventsUsageDescription` to `app/Resources/Info.plist`. Done
  when Try it 1, 5 and 8 pass. Verify: `./scripts/test-app.sh --filter
  ComputerUse`, and `log stream --debug --predicate 'subsystem ==
  "com.apple.TCC" AND eventMessage BEGINSWITH "AttributionChain"'` names
  Ottam during a call (the driver's `health_report` bundle-identity check
  expects an app parent and fails under core).
- [ ] 4. **Settings › Computer Use.** A pane beside MCP Servers
  (`Settings/SettingsView.swift`): switch, two grant rows, version and
  state, Show agent cursor (restarts with `--no-overlay`), and the per-app
  list C2 fills. Done when Try it 1 and 7 pass with rendered and AX
  evidence. Verify: `./scripts/smoke-app-bundle.sh`.
- [ ] 5. **Walkthrough and handoff.** Try it 1–9 with Claude, Codex and
  OpenCode; add the route and slot to
  [MCP](../../../architecture/backend/mcp.md).

### C2: Ask before an agent touches an app

Consent per target app, enforced at the proxy whatever the harness's own
mode, with an indicator, Stop, and transcript records.

Try it:

1. Ask Claude to click in Calculator. An Ottam sheet reads "Claude Code
   wants to control Calculator": Allow for this turn, Always allow this
   app, Don't allow. Allow for this turn: the rest of the turn asks no more.
2. Next turn asks again. Always allow: no sheet now or later; Settings
   lists Calculator with Revoke. Revoke; the next turn asks.
3. Ask Claude to click in Ottam: the call fails at once with "Ottam can't
   be controlled".
4. Type into Terminal: per decision 4. Don't allow fails the call with a
   message the agent reports.
5. With Claude in `bypassPermissions`, and OpenCode under Always allow,
   the Ottam sheet still appears.
6. Stop while a sheet is up: the sheet closes, the call cancels, the turn ends.
7. The header reads "Controlling Calculator" during calls and clears after.
8. Transcript permission rows read Allowed for this turn, Allowed (always
   allow Calculator) or Denied.
9. `list_apps`, `list_windows`, `get_screen_size` and cursor tools never
   ask; `get_window_state` asks like a click, since it reads app content.

- [ ] 6. **Target resolution.** `launch_app` `bundle_id`; `target.pid` to
  a bundle id via `proc_pidpath` and the enclosing `.app`;
  `target.kind: desktop` as a consent of its own. Unit tests on recorded
  call shapes.
- [ ] 7. **Rules and storage.** `computer_use_app_rules (bundle_id, rule,
  decided_at)`; a fixed deny for Ottam's own bundle ids, `run-app.sh`
  instances included; per-turn allows in memory keyed by run;
  `computerUse.setAppRule`.
- [ ] 8. **The gate.** In the `computer` endpoint's `call_tool`: resolve,
  look up, then pass, fail, or ask through `ServerPermissionBroker` with a
  core-originated request recorded as a permission item; Stop cancels it.
  The per-chat Always allow switch ([tool permissions](../tool-permissions.md))
  does not answer it.
- [ ] 9. **Indicator and Stop.** Count in-flight `computer` calls per
  conversation; the header names the app; Stop cancels upstream as for
  other MCP calls (`mcp/calls.rs`).
- [ ] 10. **Walkthrough and handoff.** Try it 1–9 in the real window.

## Backlog

- **Prompts for an unselected chat.** The app answers a permission
  request in a non-selected chat "cancelled" within milliseconds, so a
  background-driving agent hits it every time. Belongs to
  [tool permissions](../tool-permissions.md); land before C2.
- Per-workspace off switch, as other servers have.
- Agent guidance: `run_steps` for sequences plus a state read after, since
  accessibility clicks report `effect: "unverifiable"`; harnesses prompt
  once per batch instead of per click.
- Attach a window (`get_window_state` result) to the composer as context.
- Cursor theme and motion settings beyond on/off.
- A `bounded`-mode capability manifest generated from the app rules, so
  the daemon itself refuses what Ottam denies.
- Developer ID signing for release builds
  ([distribution](../../app/distribution.md)); without it a rebuilt ad-hoc
  app loses both grants.

## Decisions

Open, waiting on the maintainer (recommendation first):

1. **Who spawns the daemon.** *A (recommended): core*, as one more
   `mcp/runtime.rs` slot with the same start, idle-stop and quit rules;
   the chain Ottam.app → core → daemon keeps attribution on the app.
   *B: the Swift app*, matching cua-driver's reference host, but leaving a
   child process and socket outside core.
2. **Where consent lives.** *A (recommended): a core gate at the proxy,
   per target app*, applied whatever the harness mode, so it is Ottam's
   guarantee. *B: harness prompts only*: no new UI, but Claude in bypass
   mode and OpenCode under Always allow would drive any app unasked.
3. **How the binary arrives.** *A (recommended): installed at a pin on
   first enable* under `$OTTAM_HOME`, so the bundle doesn't grow by 80 MB.
   *B: bundled at build time* like GhosttyKit (`scripts/fetch-ghosttykit.sh`):
   offline and signed with the app, at the cost of size and a CI cache.
   Requiring the user's own `CuaDriver.app` is out: its standalone mode
   disclaims responsibility, so grants would be CuaDriver's.
4. **Terminals' default rule.** *A (recommended): deny* Terminal, iTerm2,
   Ghostty and Warp, since typed text is a shell command the harness never
   saw. *B: ask*, like every other app.

Decided:

- **2026-10-30: cua-driver, embedded behind the managed proxy.** It
  already has background delivery, exact window targeting, element
  tokens, a cursor overlay and an embedded mode for host apps, and the
  proxy gives core every call for consent. Rejected: Peekaboo, mac-cua,
  open-codex-computer-use, MacosUseSDK, and building on
  `ui-testing/Sources/MacAutomationKit` (no background input, element
  tokens or embedded mode).
