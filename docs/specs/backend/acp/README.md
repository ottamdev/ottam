---
kind: index
status: active
---

# ACP Work Plan

## Current Status

M1–M9 are shipped; their behavior is in
[acp.md](../../../architecture/backend/acp.md). M10 (context compaction) is
next and its Try it awaits approval. Four shipped milestones still owe one
real-window check each, listed under Active.

## How Milestones Work

This index groups the ACP specs' items into milestones. Each milestone ends
in behavior the user can try in the app. The specs own scope, context, and
done-when; this doc owns order and dependencies. Planning, verification,
and handoff follow `AGENTS.md` › Milestones:

1. Write the milestone's Try it walkthrough (app actions, expected results,
   restart and cancel behavior) and get it approved with the plan.
2. Implement its items, usually as one PR with core and app together.
3. Run the walkthrough in the real window and report rendered and AX
   evidence, plus what remains limited.
4. Move durable facts into `docs/architecture/`, then delete the milestone
   from this plan.

Kickstart: "Start milestone M<n> of the ACP work plan: write its Try it
walkthrough for approval, then implement its items per `AGENTS.md` ›
Milestones."

Item ids are `<spec>:<n>`, where `<n>` is the item number in that spec:

| Prefix | Spec |
|---|---|
| `roster` | [agent-roster-compatibility.md](agent-roster-compatibility.md) |
| `sub` | [subagent-sessions.md](subagent-sessions.md) |
| `inst` | [agent-instances.md](agent-instances.md) |
| `usage` | [usage-and-cost.md](usage-and-cost.md) |
| `cmp` | [context-compaction.md](context-compaction.md) |

## Active

### M10. See when an agent compacts its context

Needs nothing. Scope and wire evidence are in
[context-compaction](context-compaction.md).

| Item | What |
|---|---|
| `cmp:1` | Advertise `session.compaction` and map the standard updates Claude, Codex, and OpenCode already send. |
| `cmp:2` | Translate Oh-My-Pi's `_omp/compaction/*` attempt chains into one compaction. |
| `cmp:3` | A persisted `compaction` transcript item (protocol change). |
| `cmp:4` | The transcript row, its summary disclosure, and the gauge update. |

`cmp:5`, the upstream ask to Oh-My-Pi, is outside the milestone.

Try it (draft, awaiting approval):

1. On Claude, chat a few turns, then send `/compact`. A "Compacting
   context…" row appears. It becomes "Context compacted · ~25K → ~6K
   tokens", and the gauge drops right away. Expanding the row shows
   Claude's summary.
2. Do the same on OpenCode. The summary streams into the expanded row,
   then settles.
3. Do the same on Codex. The row shows no counts and no disclosure. The
   gauge updates on the next turn.
4. On Oh-My-Pi, with `compaction.thresholdTokens` around 20K in its
   profile, chat until it compacts on its own. One row appears per
   compaction, never one per attempt. A compaction between turns sits
   between those runs.
5. Press Stop during a Claude `/compact`. The row ends as stopped, not
   spinning.
6. Quit and relaunch. The rows and their summaries are still there. A chat
   from before M10 still shows its old "Compact conversation" card.

### Open checks on shipped milestones

Each is one walkthrough step that never ran in the real window. Run it,
record the result in the owning spec, then delete the entry here.

- **M4, existing Claude setup.** Claude's 72-hour coexistence check on a
  shared home ([`inst:4`](agent-instances.md#active)). Release is gated on
  it.
- **M6, OpenCode.** With an OpenCode 2 CLI already signed in, choose "Use
  my CLI setup." The sheet shows the database warning. The conversation
  uses the CLI's providers without asking for sign-in
  ([roster](agent-roster-compatibility.md#current-status)).
- **M7, oh-my-pi.** Settings › Add agent › Oh-My-Pi › Use existing setup
  on a Mac that has `~/.omp`. Ottam detects it, and the agent is signed in
  without a new login and chats against the user's own sessions and
  settings. It ran only on a Mac with no `~/.omp`, where the sheet
  correctly reported none.
- **M9, context gauge.** Continue a chat past 90% and then 95% of its
  window (a recorded fixture may stand in). The gauge turns orange, then
  red. Unit tests cover the thresholds; the window has shown only grey and
  yellow.
- **Plan usage (`usage:5`).** Implemented; owes its walkthrough
  ([usage-and-cost](usage-and-cost.md#active)).
- **Always allow tool actions.** The Oh-My-Pi run of its Try it
  ([tool-permissions](../tool-permissions.md)); it passed on OpenCode.

## Not Scheduled

Each item stays in its spec and has a trigger; none is planned until it
fires.

| Item | Where | Trigger |
|---|---|---|
| Run cost and conversation totals (`usage:3`, `usage:4`) | [usage-and-cost](usage-and-cost.md) | `usage:3` waits on an upstream ask |
| Grok Build (`roster:7`) | [roster › Next](agent-roster-compatibility.md#next) | the user's go or no-go |
| Which `AgentOnly` rows to close first | [roster › Next](agent-roster-compatibility.md#next) | trigger fired: all four `initialize` answers are recorded |
| API keys in Account | [roster › Backlog](agent-roster-compatibility.md#backlog) | proposed; a spike comes first |
| Ottam-owned ACP package on pi's SDK | [roster › Backlog](agent-roster-compatibility.md#backlog) | only if omp's ACP fails across three pin bumps |
| Per-instance defaults (model, mode, thought level) | [live-session-configuration](../live-session-configuration.md) | picked up there |
| Packaged agents | [inst › Backlog](agent-instances.md#backlog) | once publishing and trust rules are decided |
| Command picker: `@` file and `$` skill mentions | [command picker](../../app/chat/command-picker.md) | no trigger set |
| Quote reply M2: reply to a selection in the Plan tab | [quote reply](../../app/chat/quote-reply.md) | its Try it is approved |
