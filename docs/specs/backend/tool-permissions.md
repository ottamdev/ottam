---
kind: spec
status: active
---

# Tool Permissions

## Current Status

Every `session/request_permission` becomes a permission sheet in the app
(`server/agents/permission.rs`, `ServerPermissionBroker`); with no client that
supports permissions, the request is cancelled. P1, a per-chat "Always allow
tool actions" switch modelled on Zed, ships; its contract is
[ACP › Always Allow Tool Actions](../../architecture/backend/acp.md#always-allow-tool-actions).
Its walkthrough passed on OpenCode; the Oh-My-Pi run and the rendered Claude
and Codex rows remain.

This spec owns Ottam's policy for answering requests that reach Ottam. Mode
changes inside an agent (Plan, `bypassPermissions`) stay in
[live-session-configuration](live-session-configuration.md).

## P1. Always allow tool actions

Covered agents are those without their own bypass mode
(`AgentDefinition::permission_bypass_mode()`): OpenCode and Oh-My-Pi. Claude
(`bypassPermissions`) and Codex (`agent-full-access`) are not covered.

- [ ] **Finish the walkthrough.** Run Try it on Oh-My-Pi with a signed-in
  instance, and render step 6 in the real window instead of checking only
  `engine.list`. Verify with screenshots plus AX.

### Try it

1. In a new Oh-My-Pi chat, open the agent popover. It shows "Always allow
   tool actions", off.
2. Turn it on and ask for `echo hi`, then a file delete. Both run without a
   sheet; each permission record reads "Allowed automatically".
3. Open a second, existing Oh-My-Pi chat that is off and ask for a command.
   The sheet appears.
4. Start a new chat. The toggle starts on, the last value set.
5. Turn it off mid-turn in the first chat. The next command in that turn shows
   the sheet.
6. OpenCode behaves the same. With Claude or Codex selected the row is hidden,
   and Claude in Default mode still asks.
7. Quit and relaunch. Each chat keeps its value; new chats default to the last
   one set.
8. `./scripts/logs-app.sh --backend` shows auto-answers with agent and option
   kind only.

## Backlog

Nothing is planned. Each item needs its own milestone.

- A per-agent default in place of the single last-set default.
- Ottam's own allow, deny and confirm rules per tool and per pattern, modelled
  on Zed's `tool_permissions`, applied before any switch.
- Built-in guards that still ask for dangerous commands (`sudo`, `rm -rf /`,
  device writes) while auto-approving.
- An auto-approve mode in which a model reviews each request.
- Ask omp upstream for `tools.approvalMode` as an ACP session option, so
  Oh-My-Pi gains a real bypass mode and leaves P1's scope.

## Reference

**Zed.** Its switch answers ACP requests with an allow-once option, so turning
it off restores prompts instead of leaving remembered grants. It skips external
agents with their own permission modes; its granular `tool_permissions` rules
apply only to Zed's own agent
([tool permissions](https://zed.dev/docs/ai/tool-permissions.md)).

**omp**, read at v18.11.1 (current pin 18.16.0): under ACP an unset
`tools.approvalMode` gates shell commands, `edit`, `delete` and `move`; only an
explicit `yolo` (settings or `--approval-mode yolo`) disables the gate, and
settings are process-wide. ACP mode starts no config watching and advertises
only `default` and `plan`, with no extension method or slash command for
approval (`session/session-tools.ts`, `session/acp-permission-gate.ts`).

## Decisions

- 2026-10-18: Ottam grows its own permission system, possibly with its own
  auto-approve; P1 replicates Zed's switch first.
- 2026-10-18: like Zed, the switch skips agents with their own bypass mode.
- 2026-10-18: core owns the setting and the auto-answer, not the app.
- 2026-10-18: the switch is per conversation; new chats start from the last
  value set in any chat. It lives in the composer's agent popover, not Settings.
- 2026-10-18: not chosen for Oh-My-Pi: a second omp process per approval level,
  or a per-profile `--approval-mode yolo` restart. Both stay possible if omp
  never exposes the setting over ACP.
