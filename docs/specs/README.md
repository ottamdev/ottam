---
kind: index
status: active
---

# Specs

Unfinished work, one line per spec: status, then the next step. A spec is
deleted once nothing is left; its lasting facts live in
[architecture](../architecture/README.md). The `track` skill keeps this list
current.

## Release

- [Open Source Release Readiness](operations/open-source-release.md) — active — two consecutive `macos-ui-validation` dispatches, then Next
- [App Update And Distribution](app/distribution.md) — active — Milestone 1: clean-account acceptance

## Agents (ACP)

- [ACP Work Plan](backend/acp/README.md) — active — approve M10's Try it; run the six open real-window checks
- [Agent Instances](backend/acp/agent-instances.md) — active — Claude's 72-hour coexistence check (`inst:4`)
- [Agent Roster And Client Compatibility](backend/acp/agent-roster-compatibility.md) — active — go/no-go on Grok Build (`roster:7`)
- [Context Compaction](backend/acp/context-compaction.md) — active — `cmp:1`–`cmp:4` once the Try it is approved
- [Subagent Sessions](backend/acp/subagent-sessions.md) — active — handle non-terminal `subagent_update` states
- [Usage And Cost](backend/acp/usage-and-cost.md) — active — approve and run the plan-usage Try it (`usage:5`), then per-run cost (`usage:3`)

## MCP

- [Ottam-Managed MCP Servers](backend/mcp/managed-servers.md) — active — walkthrough steps 3, 4, and 9 once oh-my-pi and Codex copies are installed
- [Computer Use](backend/mcp/computer-use.md) — proposed — four open decisions, then C1's Try it
- [Ottam As An MCP Server](backend/mcp/ottam-tools.md) — deferred — `@mentions` and a task-list tool, unscheduled

## Backend

- [Agent Runtime And Setup Acceptance](backend/agent-runtime-setup.md) — active — clean-machine first-install acceptance
- [Permission Changes In Existing Conversations](backend/live-session-configuration.md) — active — probe a generating turn and a pending permission, then two-client ordering
- [SQLite Payload Storage](backend/sqlite-payload-storage.md) — active — pick A or B on the incompressible-write tradeoff, then guard payload database upgrades
- [Tool Permissions](backend/tool-permissions.md) — active — finish the P1 walkthrough on oh-my-pi
- [Local Transcript Capture And Persistence](backend/adapter-history-enrichment.md) — deferred — waits on an adapter reasoning-lifecycle signal
- [Engine-Switch Handoff](backend/engine-switch-handoff.md) — deferred — catch up missed turns after A → B → A

## App

- [Agent Onboarding](app/agent-onboarding.md) — active — gate Settings › Agents Get on Node readiness (item 4)
- [AppKit Workspace Shell](app/workspace-shell.md) — active — open spikes S2, S3, S6, S7 and the Add Tool flicker
- [Plan Review](app/chat/plan-review.md) — active — M-c, Claude `planFile`
- [Quote Reply](app/chat/quote-reply.md) — active — M2 in the Plan tab once its Try it is approved
- [Markdown Renderer Improvements](app/chat/markdown-renderer-improvements.md) — active — keyboard and VoiceOver acceptance
- [Review Rendering](app/review-rendering.md) — proposed — choose the TextKit host, then measure
- [Markdown Composer And Editor](app/editor/markdown-editor.md) — proposed — inline styling in the composer
- [Command Picker](app/chat/command-picker.md) — deferred — `@` and `$` mentions, no trigger set
- [Ghostty Config Overlay And Reload](app/terminal/ghostty-config-reload.md) — deferred — load-order tests and an effective preview

## Operations

- [Agent Efficiency](operations/agent-efficiency.md) — active — P1-4 inline Rust tests to siblings, then P0-8 generated-artifact markers
- [Agent Efficiency: File Splits And Cleanup](operations/agent-efficiency-splits.md) — active — P1-4 (transcript, registry, model catalog, auth)
- [macOS UI Validation Follow-Ups](operations/macos-ui-validation.md) — deferred — restore automatic CI after a manual run executes its steps
