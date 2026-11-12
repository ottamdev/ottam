---
name: track
description: Create or update an agent-ready implementation spec under docs/specs/. Use when the user says /track, asks to track work in a doc, keep a checklist, or wants work items persisted across sessions.
---

# Track

Specs under `docs/specs/` are working records of unfinished work, specific
enough that a fresh agent knows the next action, why it matters, where to
look, and how to verify it. `docs/specs/README.md` is the status index: one
line per spec.

## Steps

1. Search `docs/specs/` for an existing spec before creating one.
2. Build a short queue: a few concrete, independently verifiable items, next
   action first. Group user-facing work into milestones that each end in
   something the user can try in the app (`AGENTS.md` › Milestones).
3. Write to `docs/specs/<backend|app|operations>/.../<kebab-case-name>.md`
   with front matter `kind: spec` and `status: active|proposed|deferred`.
4. Sections, in order: `Current Status` (2–5 lines), `Active`, `Next`,
   `Backlog`, `Decisions`. Items say what, why, done when, and verify with,
   one line each where possible. Keep a spec under about 300 lines; split by
   milestone when it grows past that.
5. Checkboxes: `- [ ]` not started, `- [~]` in progress or blocked with a
   short note.
6. Decisions record what was decided, the date, and a one-sentence reason.
7. Keep its line in `docs/specs/README.md` current.

## Finishing Work

When an item ships, delete it. Only a new rule the code can't show (a
decision, a deliberate absence, a cross-module invariant, an adapter quirk)
moves into its `docs/architecture/` owner; how the feature works stays in the
code. Delete a spec once
nothing is left, and remove its index line. Docs carry no PR numbers, commit
hashes, or test-run evidence; git history and PRs hold that record.

For a status update, use the `now` skill.
