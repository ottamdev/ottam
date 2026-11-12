---
kind: spec
status: deferred
---

# Engine-Switch Handoff

## Current Status

Shipped. A new agent session in an existing conversation gets the earlier
turns as a text replay, and the transcript marks each switch with a divider
(`HostedContinuationDividerCard.swift`). The contract is
[acp.md](../../architecture/backend/acp.md#engine-switch-handoff) and
[mcp.md](../../architecture/backend/mcp.md#history-tools).

## Backlog

Nothing is planned. Each item needs its own milestone.

- **Catch-up after A → B → A.** A resumed session gets no replay of the turns
  between; the divider says so. Send only the missed turns. Done when switching
  back to A after a B turn lets A answer a follow-up about B's turn.
- **Images in the replay.** The replay is text; images in earlier messages are
  not resent.
- **No-MCP sessions.** A session whose MCP proxy failed to bind still gets the
  replay, and its tool-output hint names `get_session_toolcall_output`, which
  it lacks. Drop the hint when the session has no Ottam tools.

## Decisions

- 2026-11-02: replay instead of per-engine continuity, because sessions are
  scoped to one engine (`resumable_session`). The replay carries tool calls as
  one line each, with no input or output; agents read output through
  `get_session_toolcall_output`.
