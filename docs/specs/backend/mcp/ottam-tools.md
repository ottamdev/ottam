---
kind: spec
status: deferred
---

# Ottam As An MCP Server

Goal: core exposes its own tools to every agent session over MCP, so an
agent can act on things Ottam owns. The first use is delegation: hand a
task to another agent, keep working, and have its report pushed back when
it finishes, the way the Claude desktop app runs child sessions. The tools
are the reserved `ottam` route on the
[managed MCP proxy](managed-servers.md); how they work is in
[MCP](../../../architecture/backend/mcp.md).

## Current Status

Delegation ships: `delegate`, the child and parent messaging tools, the
history tools, nested sidebar rows, and deferred reports
([MCP › Delegation](../../../architecture/backend/mcp.md#delegation)).
Nothing is scheduled; the backlog is below.

## Active

None.

## Next

None.

## Backlog

- `@mentions` of another agent in the composer as a second delegation
  trigger.
- A task-list tool, so every agent's todo list renders as one checklist.
  Agents' own todo tools are too uneven to build on: Oh-My-Pi sends ACP
  `plan` updates; OpenCode's `todowrite` is off unless `experimental.todo`
  and then arrives as a plain tool call; Claude Code gives current models
  no TodoWrite in SDK sessions unless `CLAUDE_CODE_ENABLE_TODO_TOOLS=true`;
  Codex reported no `update_plan` through codex-acp. Leave OpenCode's todo
  tool off.
- Archiving a parent stops its working children and keeps them nested,
  once conversations can be archived.
- A native instructions channel per agent in place of prepending a named
  agent's instructions (claude-agent-acp reads `_meta.systemPrompt`;
  codex-acp has none yet).
- An allow-list of delegation targets in Settings, if any installed agent
  proves too open.
- Known limits:
  - The agents pill sits at the toolbar's trailing end, not beside the title.
  - A "Received message" row sits where it was posted, which in a working
    child is above the reply of the turn that read it.
  - Codex history entries for some calls (List files) carry no command text.
  - A conversation deleted from outside the app stays shown under its old
    title until another is chosen.
  - Before an ad-hoc child exists, its card names the agent by the id the
    parent passed ("Delegated to codex").
  - OpenCode's `list_children` call shows only as an `execute` row.
  - A child turn's report is judged owed or deferred from that turn alone,
    so a child that asked a question, was answered by the user and then
    finished leaves a deferred report.

## Decisions

- **2026-10-21: Ottam's tools are one endpoint on the managed proxy.** The
  per-session token identifies the calling conversation and session.
- **2026-10-21: push-back, not waiting** (user's choice). Every tool
  returns at once and a report wakes the parent with a follow-up run, so no
  harness MCP timeout applies; the tightest, Oh-My-Pi's, is 30 s. Replaces
  an `await_delegation` tool, which should not come back.
- **2026-10-21: prompt-only context** (user's choice over a file list).
  The parent writes what the child needs; the child pulls history through
  the history tools when it needs more.
- **2026-10-21: one level deep, small caps.** Children get no `delegate`.
  4 working children per parent and 5 automatic follow-ups in a row per
  child since the user's last message (narrowed from per chat on
  2026-10-22). A child is a full agent session, heavier than an
  in-harness subagent (harness caps range from 3 to 32).
- **2026-10-21: a child is its own conversation**, nested under its
  parent in the sidebar and linked from the parent's card (user's choice
  over a thread); M3's Subagent tab is not used.
- **2026-10-21: tools offered to every session**, not gated on named
  agents (user's choice). Their definitions cost context on every request.
- **2026-10-22: named agents' instructions are prepended** to the child's
  first prompt on every agent (user's choice). ACP has no instructions
  field.
- **2026-10-22: notes arrive as a core-written `session/prompt`** marked
  as from Ottam, with a status block; the user's own messages are never
  altered.
- **2026-10-25: task lists come through Ottam, not agents' todo tools.** A
  checklist fed by each agent's todo tool was dropped after its
  walkthrough: each agent needed its own workaround.
- **2026-10-27: permissions inherit, then own.** A child copies the
  parent's approval setting once, then owns it; requests show in the
  child's chat. Replaces forwarding requests to the parent, which Claude
  and Codex (no Always allow switch) could not use.
- **2026-10-27: closing the parent is deleting only**, asking to delete
  or keep the children, until conversations can be archived.
- **2026-10-27: the parent's transcript follows the Claude desktop app's
  child sessions:** a child card per `delegate` call and "Received message"
  rows grouped with tools. The task rides in the existing `note_json`
  column, not a mostly empty new one.
- **2026-10-28: a read-only child copies no auto-approval**, so a parent
  in a bypass mode cannot make a read-only child edit.
- **2026-10-28: Claude's `SendMessage` and `ListAgents` stay available**,
  since they also serve Claude's own subagents; tool text steers Claude to
  Ottam's tools.
- **2026-10-29: indented child rows, no tree line, three per parent then
  "Show N More…"** (user's choice). Shorter child rows are not possible in
  a sidebar `List`, so don't re-propose them.
- **2026-10-30: delegate to any installed agent** (T3, user's choice), or
  the caller's own by default, read-only unless the parent asks. Named
  agents stay for roles set up once. Rejected: the caller's own agent only.
- **2026-10-31: quieter push-back** (T4). A parent wakes only when it is
  owed a reply, can acknowledge with `::ottam-ack{}`, and the user can push
  a report up with Send to Parent.
- **2026-11-01: ad-hoc Oh-My-Pi children start in `default`**, because
  its `plan` mode makes every turn end with `ask` or a plan proposal.
