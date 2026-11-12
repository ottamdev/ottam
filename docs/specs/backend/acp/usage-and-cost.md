---
kind: spec
status: active
---

# Usage And Cost

Usage and cost analytics: what agents report, how core records it, and the
work queue.

## Current Status

Per-run tokens, the context gauge, and plan-usage windows ship; the contract
is [acp.md](../../../architecture/backend/acp.md) § Token And Plan Usage.
Core records no cost: `ConversationRunSummary.cost` is `json!({})`
(`server/chat/wire.rs`), `usage_update.cost` reaches the app unshown, and
core reads no `_meta.quota`. Open: per-run cost (3), conversation totals
(4), and the plan-usage walkthrough (5).

## Adapter Facts

Observed live at claude-agent-acp 0.81.0 and codex-acp 1.13.0, and
source-checked at 0.85.1 and 2.1.1. The pins have moved since
(`agent/registry.rs`); re-verify before building on any of these.

- Claude sends `usage_update.cost` (USD) only on a turn's final update. It
  is the SDK's per-`query()` running total, not the run's, session's, or
  conversation's. A resume or fork continues from the transcript's saved
  total when it has one, and `/clear` resets it. The total can rise across
  a restore either way, so monotonicity cannot detect a reset.
- Claude's `_meta.quota.model_usage` rows are per-result increments of the
  SDK's per-query `modelUsage`, so they count subagents and compaction.
  `PromptResponse.usage` counts the main loop only.
- Codex reports `PromptResponse.usage` and `_meta.quota` from
  `tokenUsage.last`: the turn's **last model request**, not the turn. In one
  capture the response said 26,832 tokens while `/status` said 112.0K. The
  adapter keeps the total only for `/status`, so core cannot compute the
  turn. Codex sends no `cost`.
- OpenCode and Oh-My-Pi send `usage_update` with cost.
- Real Claude totals for the five runs of conversation
  `01a0cf4f-3237-7a02-8d32-63c4bab67652`, at full precision: 0.1032402
  (`new`), 0.115724 (`continued`), 0.2232684 (`restored`), 0.5132456
  (`continued`), 0.9960232 (`continued`).

Upstream asks that would change this: codex-acp#227 (rate limits over ACP,
open as of 2026-11-06), and claude-agent-acp carrying `costUSD` per turn
(not yet filed; item 3).

Probe sessions that need Claude auth must attach to the running app's core;
a `--spawn` scratch home carries no Claude login.

## Active

- [ ] 3. Per-run provider-reported cost.
  - Why: Claude reports dollars only as a query cumulative, and a
    cumulative is never a run's value.
  - Scope: first ask upstream claude-agent-acp to carry `costUSD` through
    `modelUsageIncrement` into `_meta.quota.model_usage`. That gives an
    authoritative per-turn cost and replaces the delta below. Until then,
    store the last reported cumulative per run as `sessionTotal` (raw, so
    derivation can change without a migration), and derive `amount` only
    when the baseline is certain:
    - `new`, or `restored` if the total resets on restore: `sessionTotal`.
    - `continued`, or `restored` if it continues: `sessionTotal` minus the
      previous run's `sessionTotal` on the same persisted agent-session row
      (not the external session id, not the previous conversation run),
      same currency.
    - Otherwise absent: the previous run has no recorded total, a clear
      happened in either run, or currencies differ. Absent makes totals
      partial; it is never zero and never folded into the next run.
  - First settle: does Claude's total continue across restore and
    clear-context? Is `size` right after `session/load`?
  - Done when: a fixture over the five runs above derives 0.1032402,
    0.0124838, 0.1075444 or 0.2232684 (by the restore answer), 0.2899772,
    and 0.4827776; and fixtures for cancelled-then-continued and
    interleaved Claude/Codex/Claude runs derive absent where the baseline
    is missing.
  - Verify with: `./scripts/test-rust-core.sh --lib --filter cost`.

- [ ] 4. Conversation totals.
  - Scope: a computed total on `conversation.get` over all stored runs,
    aggregated in SQL rather than over the bounded run page: token
    categories summed per scope label and cost per currency, each with a
    completeness flag when any run lacks the value. No new column.
  - Done when: a mixed Claude/Codex conversation with more runs than the
    detail page limit reports full token totals and cost marked partial.
  - Verify with: a Rust integration test over a seeded conversation.

- [ ] 5. Plan usage limits for Claude and Codex: walkthrough.
  - State: implemented (`agent/plan_usage.rs`, `agent.planUsage`,
    `agent.planUsageChanged`, `ChatPlanUsageGauge`). Done after the Try it
    below passes in the real window; no run is recorded.
  - Try it (draft, awaiting approval):
    1. In a Claude chat with a signed-in managed profile, the plan gauge
       beside the context gauge shows the five-hour and weekly windows
       within a few seconds of launch, before any turn.
    2. Send a turn. The windows update from the turn's live rate-limit
       data without a probe.
    3. Open the gauge's popover: each window has a reset time
       ("Resets in 4d 10h"); per-model weekly caps are listed there and
       not drawn as bars. Press Refresh: a spinner runs until the probe
       answers, and `./scripts/logs-app.sh --backend` shows one probe, not
       two, when Refresh is pressed twice.
    4. Switch to a Codex chat: the windows come from the probe only, and
       Codex's own `/status` in the terminal agrees with the percentages.
    5. Sign out of the profile and press Refresh: the popover reads
       "Couldn't refresh." with the last update time; signing back in and
       refreshing repopulates it.
    6. Quit and relaunch: the gauge shows the last snapshot's values until
       the next probe or turn replaces them.
  - Unverified: a Codex probe running beside a live codex-acp session on the
    same `CODEX_HOME`.
  - Deferred: OpenCode Go usage (its endpoint needs the Go key from
    OpenCode's internal `credential` table), overage credits, and an age per
    window (the snapshot's one `observedAt` moves with each live update,
    which never carries the per-model windows).
  - Verify with: `./scripts/test-rust-core.sh --lib --filter plan_usage`;
    `./scripts/test-app.sh --filter ChatUsageTests`; the walkthrough in the
    real window.

## Next

- Show reported cost once item 3 derives it per run, and surface
  `MaxTokens` with continue-vs-start-fresh guidance.
- Move `UsageUpdate` to full support once cost shows. Infer support from
  observed fields and adapter versions, not the unwired `CostReporting`
  capability: context, tokens, and money are independent.

## Backlog

- A versioned model-pricing catalog for API-equivalent estimates where no
  cost is reported (Codex), plus daily, weekly, per-engine, and per-model
  rollups that degrade to usage-only views. Needs trustworthy model
  identity; record catalog version, calculation time, and assumptions.
- Recover spending that arrives after attribution is released (late cancel
  responses, background tasks) as unattributed session cost. Coordinate with
  [background task attribution](../../../architecture/backend/acp-attribution.md).
- Context-hotspot aggregates from persisted tool calls (`transcript_tools`,
  deduped by `toolCallID`): repeated reads and large payloads, with a Chat
  prompt to fix the hotspot. Heuristic weights only.
- Context breakdown like Claude Code's `/context` (messages, system prompt,
  tools, skills, memory, free space). Blocked upstream: `usage_update`
  carries only `used`, `size`, and `cost`. Trigger: an agent sends the
  breakdown as data.
- Vendor sidecar parsing, only when ACP can't expose needed data: explicit
  opt-in, Ottam-owned sessions only, recorded parser versions.
- Friction analytics (permission stalls, retry loops, abandoned runs) from
  persisted permission events.

## Decisions

- Four confidence tiers, kept visually and structurally distinct:
  1. **Reported cost** from the adapter.
  2. **Estimated API-equivalent cost** from token categories and a
     versioned pricing catalog. Not the user's subscription bill.
  3. **Usage only**: context and token categories.
  4. **Context-pressure heuristics**: optimization leads, never token or
     dollar attribution.
- Reported cost beats estimates; authoritative per-turn cost beats derived
  deltas.
- A cumulative value is never summed as a run value. Derive a delta only
  across a known-continuous counter; otherwise the amount is absent.
- Missing fields render as unavailable, never zero, and make totals partial.
- Token usage carries its scope (main loop versus all model calls); totals
  never mix scopes.
- Every normalized record keeps adapter identity and version.
- Plan-limit percentages come only from the agent's own software: what the
  adapter reports during a turn, or a usage request to the CLI it bundles.
  Core never calls vendor endpoints, reads credentials, or scrapes command
  text.
- 2026-11-06: plan usage is one account-wide snapshot per agent, fed live by
  Claude's `_claude/rateLimit.unifiedWindows` and by a read-only probe of
  the bundled CLI. This replaces the earlier Codex options, since codex-acp
  keeps its limits internal. The probe is a separate process, never a turn,
  and holds no session lock.
- Skipped: exact per-tool token or dollar attribution (no protocol source),
  subscription-dollar accounting, and OTel export as a first-class feature.
