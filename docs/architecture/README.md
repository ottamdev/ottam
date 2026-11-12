---
kind: architecture
status: active
---

# Ottam Architecture

Ottam is a native macOS app backed by one bundled Rust `ottam-core`
app-server. The Swift app owns presentation and transient window state; Core
owns durable domain state and operations. One shared `AgentConnection`
multiplexes every open Workspace window onto the same Core process and
`OTTAM_HOME`.

This directory records only what code can't show: decisions, cross-module
invariants, external adapter behavior, ownership and current limits. Read the
code for how things work. Work status lives in the
[spec index](../specs/README.md).

## Runtime Shape

```text
Ottam.app
  SwiftUI/AppKit windows, navigation, interaction and rendering
  one Workspace per native window
  one shared app-level AgentConnection
             │
             │ async JSONL requests, responses and notifications
             ▼
ottam-core
  exclusive OTTAM_HOME/core.lock
  main Diesel pool and ottam.sqlite
  separate payload worker/connections and payloads.sqlite
  Workspace, Project, Conversation, Run and Terminal handlers
  Git, local files, PTYs and ACP adapter processes
  loopback MCP proxy and MCP server processes
```

The app launches Core as a child process and shares it across windows. Core
holds `core.lock` before opening or migrating either database. Durable
Conversation history survives a Core restart. Live agent prompts, PTYs,
Terminal identities and Terminal scrollback do not.

## System Boundaries

- [Backend Core](backend/core.md): domain services, canonical local paths, Git
  and process-lifetime resources.
- [ACP and agent runtimes](backend/acp.md): adapter discovery, authentication
  and harness sessions.
- [Background task attribution](backend/acp-attribution.md): which run owns
  each agent update, background task, and autonomous follow-up.
- [MCP](backend/mcp.md): the loopback proxy agents reach MCP servers through,
  and core's connections and sign-ins to those servers.
- [Persistence](backend/persistence.md): SQLite/Diesel schema, migrations,
  delivery state and deletion policy.
- [Protocol](backend/core.md#protocol): the typed Swift↔Rust boundary and
  generated artifacts.
- [macOS Native UI](app/macos-ui.md): the Swift app shell, window policy and
  transient UI state.
- [Chat](app/chat/README.md): Chat presentation, composer and attachments,
  with the [Markdown transcript renderer](app/chat/transcript/markdown-renderer.md).
- [Terminal](terminal/README.md): cross-layer. Core owns PTYs and replay;
  Ghostty/AppKit owns presentation.

## Principles

- The app/core boundary is typed, versioned, testable, and reusable by future
  clients.
- Durable authority stays behind the Core protocol. Swift does not own Git
  operations, canonical paths, databases, transcript files, or remote
  workspace lifecycle.
- A Workspace is a durable grouping, a Project is a stable logical identity,
  and a ProjectSource is one concrete checkout or future endpoint.
- There is exactly one local Core owner per `OTTAM_HOME`.
- SSH, cloud execution, structured artifacts and concurrent orchestration need
  reviewed product and architecture work first.

## Current Limits

There is no persistent broker or multi-client daemon. A second process cannot
share the live home, Terminal processes do not survive Core restart, and remote
or cloud ProjectSources are not implemented.
