---
kind: spec
status: deferred
---

# Command Picker

## Current Status

The `/` picker has shipped; how it works is in
[chat architecture › Command Picker](../../../architecture/app/chat/README.md#command-picker).
What remains is in Backlog, with no trigger set
([ACP work plan › Not Scheduled](../../backend/acp/README.md#not-scheduled)).

## Backlog

- `@` file mentions, sent as `resource_link` blocks. ACP has no mention
  suggestions, so this needs its own picker source; it reuses
  `ComposerPanelChrome`.
- `$` skill mentions anywhere in the draft, as Codex CLI allows. Needs the
  same caret-word query as `@`.
- Arrow keys and a focus ring for the plan and question panels. Needs one
  highlighted-row style shared by all three panels; each keeps a private row
  view today (`PlanOptionRow`, `QuestionOptionRow`, the command panel's rows).
- Real-window check of the picks not yet seen in the window: arrow keys,
  Return and Tab picks, the inline hint, Claude's list (including a mode
  picked from `/`), and the question-panel slot swap. Verify with: a Codex
  and a Claude conversation, screenshots plus AX (rows readable, composer
  keeps focus).

## Decisions

1. **Card above the composer, focus stays in the composer (2026-10-17).**
   Typing keeps filtering; `ComposerTextView.keyDown` hands unmodified
   picker keys (`ComposerPickerKey`: ↑/↓, Return, Tab, Esc) to the card
   through `onPickerKey` before Return sends or Esc stops. Rejected: focus in the card (breaks
   filtering) and a caret-anchored `NSMenu` (no sections, doesn't match the
   panels).
2. **Skills where the agent marks them (2026-10-18).** ACP has no skill
   marker, but Codex names skills `$name`: they list in a Skills section
   under `/`, and `$` at the start of the draft lists only them. Agents
   without the marker (Claude) list skills under Commands.
3. **Commands persist on the agent session, like its mode (2026-10-18).**
   Agents announce commands when they choose, often outside a turn (codex-acp
   announces once, while `session/new` is in flight). Rejected: attaching
   them to the last run (a new conversation has none) and forwarding them
   live only (lost on relaunch).
