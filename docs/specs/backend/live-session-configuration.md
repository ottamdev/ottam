---
kind: spec
status: active
---

# Permission Changes In Existing Conversations

## Current Status

Shipped: a conversation's mode follows the agent's confirmed mode
(`agent_sessions.current_mode_id`), and `conversation.setMode` applies a pick
to a live session at once or defers it to the next message. The contract is
[ACP › Permission Configuration Limits](../../architecture/backend/acp.md#permission-configuration-limits).

Known limits: mode changes the agent makes with no Run active are not
captured; other windows see a change only on reopen; no revision orders two
clients' writes; model and effort have no immediate apply. Next: two-client
ordering and idle adapter-originated updates.

## Active

- [ ] **Verify adapter behavior without touching user conversations.**
  - Why: ACP permits configuration changes while idle or generating, but
    accepting a setting differs from applying it to an in-flight turn.
  - Covered so far: an idle existing Claude session applied a Plan pick in place
    (same session id) in the real window.
  - Remaining: a generating turn, an outstanding permission request, and a
    more permissive change. Record any rejected or substituted mode.
  - Done when: request/response and notification evidence exists for each case;
    the session id is unchanged and later behavior follows the acknowledged
    choice.
  - Verify with: the current pins in `agent/registry.rs`, a scratch home and
    repository, `scripts/inspect-core.sh --spawn` for core-supported calls, and
    a bounded adapter-boundary diagnostic. Record commands, versions and
    outcomes here. Don't extend the inspector for this probe.

### Adapter behavior

Inspected at `claude-agent-acp@0.85.1` and `codex-acp@2.1.1`; current pins
are 0.100.0 and 2.5.0, so recheck before relying on this table.

| Adapter | Implementation | Required product behavior |
| --- | --- | --- |
| Claude | `setSessionConfigOption` → `SessionModeManager.selectMode` → SDK `query.setPermissionMode`; no idle-only guard. | Send to the open session at once and confirm the returned value. The probe must establish in-flight and pending-approval behavior. |
| Codex | `applyModeChange` sets `sessionState.agentMode`; `sendPrompt` applies that preset's policy when a turn starts. | Update the session; while a turn runs, say the change applies to the next turn. |

## Next

- [ ] **Own settings per conversation and session; order changes with turn
  admission.**
  - Remaining: updates from the adapter with no active Run, two-client
    ordering, and model/effort. A saved Run's mode stays historical start state.
  - Scope: desired, acknowledged and pending/error state keyed by conversation,
    engine and session. Per-engine preferences stay new-chat defaults only.
    Reuse `ConversationSessionMetadata` storage and publication; no second owner.
  - Done when: the contract below holds.
  - Verify with: focused core integration and storage tests for continuation,
    restoration, fresh-session recovery, queued input, retries and two clients;
    Swift tests for confirmed vs pending state, failure, conversation switching,
    unavailable modes and adapter-originated updates; regenerated protocol
    artifacts with `./scripts/check-protocol-alignment.sh`; the affected
    real-window scenario with screenshots plus AX.

### Contract still to build

1. A write names the expected session identity and a configuration revision;
   core rejects stale targets or revisions and the client refetches, rather
   than applying to a replacement session.
2. Configuration writes serialize per session, and published snapshots are
   monotonically ordered so a delayed reply cannot overwrite newer state.
3. A turn submitted after a change waits for it to settle; on failure it does
   not start under the previous policy. Serialize turn admission with
   configuration application in core, not only in the Swift button action.
4. A historical retry keeps its captured choices and reconciles before
   prompting; old Runs are never rewritten to match the picker.
5. An unloaded session keeps a clearly deferred choice. On next use, load,
   discover configuration, apply, then prompt. A dead session is a recoverable
   state; a picker change alone never starts a fresh conversation or a prompt.
6. A fresh recovery session applies the saved choice. An unsupported or
   rejected value blocks the dependent turn with an actionable error, never a
   more permissive default.
7. After an ambiguous timeout or disconnect, mark configuration unconfirmed and
   reconcile before a dependent turn, using documented responses, load metadata
   or idempotent reapplication. Don't invent an ACP configuration-get method.
   Reconcile idle updates through session ownership, not run-attributed events.
8. Consume complete returned configuration and `config_option_update`
   snapshots: a fallback can change other selectors. Normalize once in core and
   refresh all clients.

### Pending permissions

The Claude probe must establish whether its SDK cancels or reevaluates an
outstanding request on a mode change. If the request stays pending, keep it
actionable and say it still needs an answer. Test the race between a mode
change and answering the card so each request resolves once. Auto is an
approval policy that can ask; never describe it as bypass.

## Backlog

- Immediate model and thought-level picks. A changed pick already reaches a
  held session before the next prompt (`ApplyModel`); immediate apply would add
  a `conversation.setModel`-style call and its metadata.
- Generic rendering of all adapter configuration types, permission-rule
  editing, and adapter upgrades are out of scope. An Ottam-defined permission
  system lives in [tool-permissions](tool-permissions.md).

## Decisions

- Use ACP `session/set_config_option` with advertised IDs and values. Don't
  infer permission support from the legacy `session/set_mode` method.
- Existing-session changes keep session identity and history; no automatic
  cancel/restart to fake mid-turn application.
- A new-chat default, a pending selection, an acknowledged session setting and
  a Run's start policy are distinct states.
- Adapter acknowledgments and updates determine displayed effective values; a
  saved local preference is never shown as live application.
- Pending approvals stay independently actionable until resolved or cancelled.
