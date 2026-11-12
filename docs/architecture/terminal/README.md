---
kind: architecture
status: active
---

# Terminal Architecture

Ghostty renders terminal surfaces. Rust `ottam-core` owns PTYs,
process-lifetime terminal identity, in-memory scrollback, replay, and protocol
output. Swift and Ghostty own the visible surface only.

## Bundled Resources And Licensing

`scripts/build-app.sh` copies Ghostty runtime resources from the pinned
artifact in `app/Vendor/ottam-ghostty`: `Contents/Resources/ghostty` (themes
only) and `Contents/Resources/terminfo`. The Swift host points
`GHOSTTY_RESOURCES_DIR` and `TERMINFO` at those directories and defaults
`TERM` to `xterm-ghostty`; core resolves the same `TERMINFO` for shells and
agent children.

Ottam ships no shell integration. The runtime config sets
`shell-integration = none`, and the app sets no `ZDOTDIR`, `PROMPT_COMMAND`,
or any other integration hook. Prompt marks would need an Ottam-owned script,
because upstream Ghostty's zsh and bash scripts are GPLv3; the artifact leaves
them out. Licensing and attribution are in the bundled
`THIRD_PARTY_NOTICES.md`.

`scripts/fetch-ghosttykit.sh` fetches a checksummed artifact from
`ottamdev/ghosttykit`: upstream Ghostty at a release tag plus one Ottam-owned
patch for the manual-IO surface API (`GHOSTTY_SURFACE_IO_MANUAL`) that
core-owned PTYs need. Upstream has no such API, and `include/ghostty.h` is its
internal embedder interface, so each tag bump can break the Swift host.
libghostty-vt has a stable ABI but no renderer, so it cannot replace this.

## Ghostty Config

`GhosttyRuntime.makeConfiguration` loads layers in order, last wins: Ghostty's
default files, `ottam-defaults` (the Settings code font), the Ottam overlay
`~/Library/Application Support/<bundle id>/config.ghostty` with its includes,
then `ottam-runtime` overrides (`shell-integration = none`,
`window-decoration = false`, hidden titlebar proxy icon). So the overlay beats
the Settings font and runtime overrides beat everything.

The overlay is created once and never rewritten; each bundle id has its own,
and dev builds do not fall back to the release one. `font-family` is inert in
the embedded surface. `font-size` must also feed
`ghostty_surface_config_s.font_size` from the finalized config, because that
field overrides every config layer. A reload that cannot build a config keeps
the old one (`lastReloadError`); see the
[config reload spec](../../specs/app/terminal/ghostty-config-reload.md).

## Ownership Model

`ottam-core` owns terminal identity and process state:

- Core generates each `TerminalID` (lowercase UUIDv7) and returns it from
  `terminal.open`. `TerminalManager`'s session registry is the only record
  that a terminal exists, for the lifetime of the core process.
- Each session owns its PTY, shell process, resize/write operations, metadata,
  exit status, output reader, and bounded scrollback.
- Partial PTY startup has one cleanup owner, the `PendingPtySession` guard:
  dropping it uncommitted kills and waits for the child and releases handles.
  After commit, the `TerminalSession` lifecycle owns termination.
- Swift and Ghostty own the visible surface only. Recreating a renderer may
  replay a terminal still held by the same Core process, but does not
  persist terminal state or require invoking a backend detach method.
- SQLite and `${OTTAM_HOME}/workspaces` hold no terminal records, transcript
  indexes, or terminal directories.

`terminal.write` queues bytes on the session and returns: success means
accepted in order, not yet written. A per-session writer drains the queue as
the PTY becomes writable, so a program that stops reading stdin stalls only
its own input; past the queue bound the write fails with recoverable
`terminalInputBusy`, and closing drops unwritten input.

Close and delete both remove the terminal and terminate a live shell; both
exist because the app calls them from different lifecycle paths, and delete is
idempotent for an unknown `TerminalID`.

Ghostty owns selection, mouse reporting and the clipboard bindings; focus
follows the view's first-responder status. A paste Ghostty flags as unsafe
goes through without a prompt, as in Terminal; an OSC 52 clipboard read under
the default `clipboard-read = ask` gets an empty clipboard. Ottam has no
permission prompt, so writes under `clipboard-write = ask` are refused.

## Live And Completed Terminals

A terminal has two process-lifetime states: **live** (PTY running, a surface
may attach; hidden tabs keep their surface warm) and **completed** (shell
exited; the manager keeps metadata, exit status and scrollback until the
client closes or deletes it).

There is no cold terminal state. Local PTYs end with `ottam-core`, and a fresh
Core starts with an empty terminal list. Terminal requests reach only a
terminal the current Core owns; an unknown or stale `TerminalID` returns
recoverable `terminalNotFound`.

Project-backed terminals keep their `{ProjectID, ProjectSourceID}` only in
memory. Removing that Project from the Workspace closes its live terminals;
re-adding it does not restore them.

## Output Delivery And Retention

Terminal output has five separate bounded layers. Keep them separate; none is
a proxy for another:

- **Live delivery queue** (`OTTAM_TERMINAL_OUTPUT_MAX_BYTES`) bounds queued
  JSONL output notifications. It is a backpressure guard, not a PTY lifetime
  cap.
- **Core scrollback** (`SCROLLBACK_MAX_BYTES`) bounds the per-terminal
  in-memory ring. Eviction is oldest-first and sets retention-truncation
  metadata.
- **Completed scrollback aggregate** (`COMPLETED_SCROLLBACK_MAX_BYTES`)
  bounds scrollback kept across exited sessions. Older completed buffers go
  before the just-completed one.
- **Replay response budget**: `terminal.replay` returns a byte-bounded tail
  from the ring, with response- and retention-truncation metadata plus
  retained sequence/byte bounds.
- **Ghostty scrollback**: `scrollback-limit` separately bounds renderer
  scrollback.

Chunks are the unit of both eviction and replay: a chunk is kept and returned
whole, never split, so a returned `sequenceEnd` means the client holds every
byte up to that sequence. Eviction never drops the newest chunk. The PTY read
loop assigns one monotonic sequence and byte range per chunk, appends it to
the ring, then queues the live notification. No terminal output is written to
SQLite or disk.

Chunk size is the OS PTY buffer (1024 bytes on macOS), not `READ_BUFFER_BYTES`,
and is left alone deliberately: coalescing reads would wait on the shell and
add latency to interactive echo.

Live output drops must not kill the PTY. When delivery degrades, core reports
`deliveryState`, dropped counts and `replaySuggested`; Swift treats degraded
delivery and sequence gaps as replay boundaries instead of feeding
discontinuous bytes into Ghostty.

## Replay And Resync

Replay is an in-session recovery path for recreated views, degraded live
delivery and sequence gaps. It is not restart restoration.

- Swift buffers live deltas while replay is in flight, then applies only
  deltas still valid after the replayed sequence boundary.
- A completed terminal stays replayable until the client closes it.
- Core restart destroys the ring with the session, so replay of a stale
  `TerminalID` returns `terminalNotFound`.
