---
kind: spec
status: active
---

# Notifications

Tell the user an agent finished or is waiting on them when they are not
looking at that conversation, through native macOS banners, a sidebar mark,
and the Dock badge.

## Current Status

M1 (core-owned attention, native banners) is in progress. Core owns the
attention state, a persisted unseen marker, and the notify decision, so a
future remote client gets the same answer; the Mac app is a thin presenter.
Before M1 the only signal was the sidebar's "Approval required" mark.

## Active

### M1: core-owned attention, native macOS banners

Core tracks per conversation `needsInput` (a pending permission, question, or
plan ask), `unseen` (a run completed or failed while nobody looked), or none.
The app reports one focus value: the selected conversation of the main
workspace window while the app is active, that window is visible, and the
screen is awake and unlocked. Focus is also the seen signal.

Try it (approved). Run `./scripts/run-app.sh` with a real agent.

1. In conversation A send a task that takes about 30 s. macOS asks to allow
   notifications; allow. A's row shows a spinner.
2. Select conversation B. About 2 s after A finishes, a banner appears:
   A's title, the workspace as subtitle, body "Finished". A's row shows the
   accent dot. The Dock badge shows 1.
3. Click the banner. The window comes forward with A selected; the dot, the
   badge, and the Notification Center entry clear.
4. Send in A again and stay on A until it finishes. No banner, dot, or badge.
5. Send A a task that needs approval, then switch to Finder. A banner says
   approval is needed and names the tool; badge 1. Click it: Ottam comes
   forward on A with the approval card. Approve; the badge clears.
6. Cancel: start a long task in A, select B, come back and press Stop. No
   banner and no dot.
7. Start a long task in A and open Settings. When A finishes a banner
   appears, because the workspace window is no longer main.
8. Start a long task, close that workspace's window, leave Ottam running. A
   banner appears when it finishes; clicking it reopens the window on A.
9. Restart: let A finish while on B (dot, badge 1). Quit and relaunch. The
   dot and badge are still there; selecting A clears them.
10. Settings › App › Notifications: turn off "Agent finished" and repeat
    step 2. No banner; the dot and badge still appear.

Failed runs are covered by the protocol test, not by hand.

- [ ] 1. Core attention state and protocol (`attention.list`,
  `attention.setFocus`, `attention.changed`).
  - Done when: unfocused completion yields unseen and focus clears it; a
    focused completion yields nothing; asks yield needsInput then none;
    cancel, interrupt, and shutdown yield nothing; a delegation tree
    notifies once on the parent.
  - Verify with: `./scripts/test-rust-core.sh --lib --filter attention`;
    `./scripts/test-rust-core.sh --test jsonl_protocol --filter attention`.
- [ ] 2. Persisted unseen marker (`conversations.unseen_run_id`).
  - Done when: unseen survives a core restart and an upgrade from the
    baseline schema keeps existing rows.
  - Verify with: the restart protocol test and the migration upgrade test.
- [ ] 3. App presenter: focus reporting, banners, sidebar dot and spinner,
  Dock badge, Settings › App › Notifications toggles.
  - Done when: Try it steps 1–10 pass in the real window.
  - Verify with: `./scripts/test-app.sh --filter Attention`; the walkthrough.

Known limit: an approval for a workspace whose window is closed is still
answered `cancelled` by the app, so it never reaches needsInput. Parking
approvals at the app level is not part of M1.

## Backlog

- Durable multi-client approvals, so an ask survives a closed window or a
  disconnected client and any client can answer it.
- The final assistant message as a preview in the banner body.
- Usage-limit and sign-in-required triggers.
- Mark as Unread.
- Allow and Deny actions on approval banners.
- Remote delivery (iOS client, APNs relay).

## Decisions

- 2026-11-17: Core owns attention, the notify edge, and the unseen marker;
  clients only present. A backgrounded phone cannot hold a socket, so the
  decision must not live in a client.
- 2026-11-17: Focus is the main workspace window's selection, not the key
  window's, so sheets, alerts, and the quit panel do not drop it.
- 2026-11-17: Focus doubles as the seen signal; there is no mark-seen call.
- 2026-11-17: Banners are click-through only, one replacing banner per
  conversation and kind, grouped by workspace.
- 2026-11-17: System notification authorization is requested on the first
  banner while undetermined, never at launch.
- 2026-11-17: Sidebar priority is approval, running, unseen; unseen uses the
  accent dot and running becomes a small spinner.
