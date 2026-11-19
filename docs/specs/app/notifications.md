---
kind: spec
status: active
---

# Notifications

Tell the user an agent finished or is waiting on them when they are not
looking at that conversation, through native macOS banners, a sidebar mark,
and the Dock badge.

## Current Status

M1's core attention state, its persisted unseen marker, and the app presenter
are in. The rules live in [Attention](../../architecture/app/macos-ui.md#attention).
What is left is the part of the walkthrough only a person can drive: the
system permission prompt, banner clicks, and the banner and Dock pixels.

## Active

### M1: core-owned attention, native macOS banners

Try it (approved). Run `./scripts/run-app.sh` with a real agent.

1. In conversation A send a task that takes about 30 s. macOS asks to allow
   notifications; allow. A's row shows a spinner.
2. Select conversation B. About 2 s after A finishes, a banner appears:
   A's title over the start of its last message ("Finished" when it wrote
   none). A's row shows the accent dot. The Dock badge shows 1.
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
10. Settings › App › Notifications: turn off "Agent Finished" and repeat
    step 2. No banner; the dot and badge still appear.

Failed runs are covered by the protocol test, not by hand.

- [ ] 3. Banners checked by hand.
  - Done when: step 1's prompt appears on a bundle that has never asked;
    the banners in steps 2, 5 and 8 read as written; clicking them brings
    Ottam forward on the conversation (step 8 reopens the window); the Dock
    badge shows the count; with banners denied, Settings' "Open System
    Settings…" lands on Ottam's Notifications pane.
  - Verify with: the walkthrough, driven by a person. Background automation
    keeps Ottam inactive and cannot see system UI.

## Backlog

- Durable multi-client approvals, so an ask survives a closed window or a
  disconnected client and any client can answer it.
- Usage-limit and sign-in-required triggers.
- Mark as Unread.
- Allow and Deny actions on approval banners.
- Remote delivery (iOS client, APNs relay).
