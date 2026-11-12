---
kind: index
status: active
---

# Ottam Documentation

Each document has one owner and one lifecycle:

| Folder | Holds |
|---|---|
| [architecture/](architecture/README.md) | How the system works today: ownership, invariants, boundaries, data flow. Present tense, no history. |
| [specs/](specs/README.md) | Unfinished work, one line each in the index. Deleted when done. |
| [operations/](operations/) | Repeatable procedures, such as the [performance playbook](operations/performance/README.md). |

## Rules

- Front matter holds `kind` (index, architecture, spec, operations,
  reference) and `status` (active, proposed, deferred, exploratory).
- Architecture holds only what code can't show: decisions and deliberate
  absences, invariants that cross modules or the app/core boundary, external
  adapter behavior, and ownership, each with one code pointer. No pin tables,
  file inventories, constant tables, or step-by-step behavior. Edit it when a
  rule changes, not after every feature.
- When work ships, the spec item is deleted; a new rule it established moves
  into architecture. Git history keeps provenance; there are no archives or
  redirect stubs.
- No PR numbers, commit hashes, or test-run evidence in docs.
- Specs stay under about 300 lines; split by milestone past that.

Check documentation changes with `./scripts/check-docs.sh` and
`git diff --check`.
