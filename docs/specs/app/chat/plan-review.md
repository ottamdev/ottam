---
kind: spec
status: active
---

# Plan Review

## Current Status

A plan request shows a plan panel above the composer (`ChatPlanPanel`) and
the plan itself in a Plan tab in the tools pane, auto-opened for the
conversation on screen. Markdown files open in their own read-only tabs, and
Add Tool lists Plan. M-c, following Claude's plan file, is next; nothing
blocks it.

## Next

### M-c: follow Claude's plan file

- [ ] `plan:4` Support AIR `planFile` for Claude.
  - Why: the tab should follow Claude's edits to the plan instead of a copy
    of the plan text.
  - Context: core marks `AirPlanFile` Unsupported
    (`agent/acp/features.rs`), so Claude's plan arrives inline in
    `rawInput.plan`. With `planFile` advertised, Claude drops `rawInput.plan`
    and sends only `rawInput.planFilePath`; Claude Code writes plans under
    the agent home's `plans/<slug>.md`.
  - Scope: advertise `planFile`. Core's `plan_markdown`
    (`server/agents/permission.rs`) must also detect a plan by
    `planFilePath`, or the request falls back to the orange permission card.
    The Plan tab reads the path (inside the agent home only) and falls back
    to `rawInput.plan`. Carry the path on `agent.requestPermission` and
    `TranscriptPermission`, and snapshot the file text on the record so
    history survives later edits.
  - Done when: a Claude Plan mode plan renders from the file and updates
    when Claude edits it.
  - Verify with: core feature and permission tests; a real-window run.

Try it for M-c is written when it starts.

## Backlog

- Move every permission into the panel above the composer. Trigger: the
  plan panel proves out in use.
- When Codex also streams the plan into the transcript, collapse the streamed
  copy into a pointer to the Plan tab. Needs reliable matching of streamed
  text to `rawInput.plan`. Trigger: the duplicate bothers in use.
- If auto-opening the pane in a narrow window proves disruptive, stop
  auto-opening and draw attention to Open plan instead.
- Undecided findings: with a draft in the composer, ⌘↩ sends it while a plan
  is pending; arrow keys don't move the panel's selected row and it shows no
  focus ring (see [command picker](command-picker.md#backlog)); the Plan tab
  scrolls to the top on a conversation switch; after a relaunch, Codex once
  printed `<proposed_plan>` as text and sent no review request.
- An unexplained exit (status 1, no crash report) followed a conversation
  switch once during the Add Tool → Plan flow, after SwiftUI
  `AttributeGraph: Cycle detected` warnings. Not traced to plan review or
  ruled out. Trigger: it recurs; check `app-session-exit.log` and
  `./scripts/logs-app.sh --swift`.
- Scroll a file tab to the line a `:12` or `#L12` suffix names. Trigger:
  agents link to lines in markdown files.
- Inline-code paths to a file that appears later become links only on the
  next layout (scroll, resize, edit).
- Edit mode for the viewer belongs to the
  [Markdown Editor spec](../editor/markdown-editor.md).
- Replying to selected plan text is
  [quote reply M2](quote-reply.md#m2-quote-reply-in-the-plan-tab).

## Decisions

1. **Presentation (2026-10-17).** The plan in a Plan tab, auto-opened for the
   conversation on screen; the decision in a panel above the composer built
   like the question panel (title, first line, Open plan, the adapter's
   options plus Revise). The agent is blocked as on a question, and Revise
   lands in the composer below. A background conversation gets only the
   sidebar badge. Rejected: markdown in a transcript card (hosted-card height
   risk) and a modal plan sheet (blocks the conversation).
2. **Plan detection in core (2026-10-17).** A non-empty string
   `rawInput.plan`, with `kind` absent or `"switch_mode"`; any other kind is
   not a plan. Claude's AIR requests carry no `kind`. Core sends the markdown
   as `plan` on `agent.requestPermission` and on the record; the app reads
   only that.
3. **Viewer host (2026-10-17): a tools pane tab**, beside Review, Terminal,
   and Subagent ([workspace shell](../workspace-shell.md)). Rejected: sheet
   (modal), separate window, Quick Look (no live plan source).
4. **Plan records (2026-10-17).** A plan's record names the decision: "Plan
   approved" plus the option, "Plan sent back for feedback", or "Plan
   cancelled", with the plan icon; clicking it shows that plan in the Plan
   tab, also after a relaunch. A turn that ends on a declined plan gets no
   Stopped chip. Rejected: matching adapter titles in the app (brittle).
5. **No timeout for plans (2026-10-17).** Plans wait for the user, a Stop,
   or the agent, like questions; a timeout cancelled plans while they were
   being read.
6. **One tab per file (2026-10-22).** Each `.md` file gets its own tab, keyed
   by its resolved absolute path; reopening selects it; same-name files get
   labels with parent folders until they differ. The Plan tab stays one per
   conversation and auto-open never touches file tabs. Rejected: sharing the
   Plan tab (a pending plan would replace the file being read).
7. **Entry points (2026-10-22).** Transcript and in-file markdown links, the
   Read row's open button, and inline-code paths that resolve to an existing
   `.md` under the working root (in a file tab, from its folder first).
   Links resolve from the file's folder and stay inside the working root.
   Other files open through `NSWorkspace`.
8. **Reload by watching the file (2026-10-22).** A `DispatchSource`
   file-system source on the file, reopened when the path names a new file,
   plus one on the nearest existing folder, so a file whose folder appears
   later still loads; reloads at most every 100 ms. Rejected:
   `NSFilePresenter`, which sees only coordinated writes.
9. **File access stays in the app until remote ProjectSources (2026-10-22).**
   File tabs read, watch, and canonicalize paths on the Mac's own disk.
   Before remote ProjectSources ship, Core must own `file.read` (text plus a
   stat token, containment and canonical path decided in Core), a
   `file.watch` subscription, and an async existence answer that layout
   never waits on. Rejected for now: a Swift file-source seam with one local
   implementation, which would guess the async interface.
10. **Plan in Add Tool, disabled without a plan (2026-10-22).** Add Tool and
    the empty pane's picker always list Plan after Review and Terminal,
    disabled when there is no plan, so the menu keeps its shape. It opens
    the pending plan, else the one the tab last showed, else the newest
    main-thread plan record in the loaded transcript. Rejected: listing Plan
    only when a plan exists.
