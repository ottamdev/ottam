---
kind: architecture
status: active
---

# Backend Core

The bundled Rust `ottam-core` process is Ottam's local coordinator and the
authority for durable state. The Swift app launches one Core child and talks
to it through `AgentConnection`.

## Ownership

Core owns Workspace, Project, ProjectSource, Conversation, Run and RunInput
services; canonical path validation and ProjectSource identity; both SQLite
databases and their migrations ([Persistence](persistence.md)); Git status,
diffs, branches and worktrees; process-lifetime PTY Terminals
([Terminal](../terminal/README.md)); ACP adapters ([ACP](acp.md)); the MCP
proxy and server sign-ins ([MCP](mcp.md)); and the ordered event stream.

The Swift app may seed an initial path for development or automation, but
Core resolves it through `project.openPath` and owns the resulting identity
and Workspace membership. IDs (`WorkspaceID`, `ProjectID`,
`ProjectSourceID`) are never derived from a path; a ProjectSource is one
concrete checkout of a Project.

Exactly one Core holds `${OTTAM_HOME}/core.lock` before using that home.
User Git checkouts contain no Ottam database. Terminals have no database rows
or transcript files.

While Core runs, `core.sock` beside the home accepts attached JSONL clients
(`ottam-inspect`) with the primary's dispatcher; it is 0600 and
unauthenticated, the sqlite file's trust domain. `OTTAM_ATTACH=off` disables
it (`server/attach.rs`).

## Release Contents

A release build cannot be told by its environment to run or download code it
did not ship. Test seams in Core (test agents, the adapter-install and Node
download overrides, the in-memory secret store, ACP wire logging) are
`cfg(test_seams)`: on in every debug build, and in optimized builds only with
the `test-seams` feature that `build-app.sh --profile` passes
(`backend/crates/ottam-core/build.rs`). UI-test hooks and the core-path
override in the app are `#if DEBUG`, and a release app launches only the core
in its own bundle. `scripts/check-release-seams.sh` proves the absence on a
built bundle. `OTTAM_NODE` and `OTTAM_ATTACH` are user settings and stay.

## Request Handling

- Every handler decodes params through `request_params::decode_params`: a
  bad payload answers `-32602` with the request id. Dispatch is an explicit
  match, not a registry.
- Deferred work runs on non-queuing bounded lanes (`DeferredLane`,
  `server/deferred.rs`). A full lane rejects at once; nothing queues. The
  agent cancellation lane is separate from the agent regular lane so request
  saturation cannot starve cancellation. Conversation workers hold their
  regular permit until they exit.

## Protocol

The Swift–Core boundary is JSONL over stdio: JSON-RPC-style request,
response, error and notification envelopes, concurrent in-flight requests,
`initialize` capability negotiation, and ordered server events.

- `backend/crates/ottam-protocol` is the source of truth. It generates golden
  fixtures and `app/Sources/OttamBackendClient/ProtocolTypes.generated.swift`;
  neither is hand-edited. No JSON Schema file is committed: the Swift
  generator reads the schema in memory (decided 2026-11-10; nothing else read
  the 958 KB file). Cross-language tests decode Rust fixtures in Swift and Swift
  fixtures in Rust. A protocol change regenerates them before
  `scripts/check-protocol-alignment.sh` runs.
- Both sides must agree on the frame limit: Core reads lines of at most
  1 MiB (`MAX_JSONL_MESSAGE_BYTES`, `server/transport.rs`); a longer line
  gets `-32001` with a null id and Core keeps serving. The Swift transport
  accepts 16 MiB, so senders of large payloads (terminal pastes) must stay
  under Core's limit themselves.
- Bytes stay in the protocol: bounded stream chunks carry PTY, command and
  log output; large evidence is fetched out of band. There is no second
  binary framing protocol.
- On a handled fatal error Core emits and flushes `core.exitError`
  (`kind`, `message`, optional `details`) before exiting non-zero. It may
  arrive before `initialize`; the Swift transport retains it through stdout
  EOF so pending requests and recovery UI see the typed error. Crashes and
  signals supply none; the UI then falls back to captured core logs.
- The removed POC structured-object methods (`conversation.promoteArtifact`,
  `conversation.captureDecision`, `spec.*`, `task.*`, `taskGraph.get`,
  `memory.*`, `decision.*`) have no aliases: `initialize` does not advertise
  them and calls return `-32601`. Reviving one is a product decision plus
  append-only migrations, not a compatibility shim.

## Shutdown

Stdin EOF, SIGTERM and a closed stdout pipe all take the same graceful path
and exit 0: cancel adapter and runtime installs (killing npm's process
group), stop agents, MCP and deferred work, drain transcript checkpoints
under a bounded timeout (`TRANSCRIPT_DRAIN_TIMEOUT`), close terminals, unlink
`core.sock`. Adapter shutdown is not capped: it waits for an in-flight auth
refresh to release its lock. The app's escalation order is stdin close, then
SIGTERM, then SIGKILL.
