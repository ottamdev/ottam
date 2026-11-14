---
kind: spec
status: active
---

# Open Source Release Readiness

## Current Status

The source is public at `ottamdev/ottam` under Apache 2.0, with its community
files and private vulnerability reporting. What remains before a first public
download is listed under Next.

## Next

Needed before a first public download, not before the source is public.

- **First-run sequencing.** Get in Settings › Agents is enabled before Node is
  ready ([agent onboarding](../app/agent-onboarding.md#next)).
- **Payload database upgrade protections.** Newer-schema rejection and the
  pre-migration backup cover only the main database
  ([payload storage](../backend/sqlite-payload-storage.md)).
- **Distribution through the first public release.** A notarized release
  build, the Sparkle updater, and a locally scripted publication to a signed
  appcast; the first public binary must already update itself
  ([distribution](../app/distribution.md)).

## Backlog

Not release blockers: a CI release workflow, a universal binary, the usage and cost UI, live
PTY restore across Core restarts, the Markdown editor, the streaming CPU goal, and conversation retention or export.

## Decisions

1. **License.** Apache 2.0 (2026-11-07), over MIT, for its patent grant,
   explicit inbound-contribution terms, and its exclusion of trademark rights.
2. **Terminfo overlay.** Dropped (2026-11-09); the bundle ships Ghostty's MIT
   terminfo only, which keeps GPL-licensed files out of an Apache 2.0 app.
3. **Architecture.** `arm64` only (2026-11-09), because the Ghostty artifact is
   built for the native target and a universal build adds a second Swift,
   Cargo and Ghostty target for no current user.
4. **Contributor terms.** DCO sign-off with no enforcing check (2026-11-09),
   because Apache 2.0 §5 already licenses inbound contributions and a CLA would
   add only relicensing rights.
5. **Security reporting.** GitHub private vulnerability reporting (2026-11-09),
   so reports stay private without publishing a personal email address.
6. **Publication.** One squashed snapshot commit (2026-11-06), with the
   pre-release repository kept private as the full-history archive, because
   its history holds pre-scrub paths, strategy documents, and pull request
   discussion that only a rewrite would remove.
7. **Canonical repository.** `ottamdev/ottam` (2026-11-12): development
   continues from its snapshot and the pre-release repository is a read-only
   archive, because post-snapshot changes already live only in `ottamdev/ottam`
   and another snapshot would drop them.
