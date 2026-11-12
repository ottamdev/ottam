---
kind: spec
status: deferred
---

# Ghostty Config Overlay And Reload

## Current Status

Layered loading and reload have shipped
(`GhosttyRuntime.makeConfiguration` / `reloadConfiguration` in
`OttamGhostty/GhosttyEmbedding.swift`); the layer order is in
[Terminal Architecture](../../../architecture/terminal/README.md). Settings
shows the overlay path and a Reload button that alerts with `lastReloadError`
on failure and says nothing on success. The
polish below is deferred with no trigger set. Backend output, retention, and
replay policy stay in the terminal architecture doc, not here.

## Backlog

1. **Tests** for load order, recursive `config-file` include cycles, symlink
   handling, and reload failure keeping the previous config.
   Verify with: focused Swift tests.
2. **A small parser** for the keys Ottam inspects (`font-size`, `theme`,
   colors, `background-opacity`, `scrollback-limit` with `_` separators,
   `config-file`), cached with an explicit invalidation hook on reload.
3. **A read-only effective preview** in Settings: parsed key/value entries in
   Ghostty key order with last-wins precedence, each with its source, for
   example `font-size = 14  # from: ~/.config/ghostty/config:12`. Built from
   parsed files, never by serializing Ghostty's opaque runtime config.
4. **Config diagnostics.** Surface Ghostty's config diagnostics, which are
   only logged today; a config with diagnostics still loads, so the reload
   alert never shows them.
5. **Surface-scoped reload and a safe fallback**, if needed: update one
   surface with a temporary config without the app-level notification, and
   retry app creation with a minimal inline config when the resolved config
   fails.

Done when: tests cover the load order and failures, and the preview shows
winning values with their source paths. Verify with the app build, focused
tests, `./scripts/smoke-app-bundle.sh` if bundle resources change, and a
real-window terminal capture after changing a visible setting such as
`background`.

## Decisions

- **The overlay is Ottam-owned** (`config.ghostty` under Application Support
  for the bundle id), never the user's standalone Ghostty config, which is
  loaded but not copied or mutated. An `OTTAM_HOME`-scoped overlay is
  deferred until a portability need appears.
- **`scrollback-limit` is bytes, not lines.** The default overlay sets
  `10000000`; never describe Ghostty retention in lines.
