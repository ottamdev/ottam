---
kind: index
status: active
---

# Ottam Documentation

Each document has one owner and one lifecycle:

| Folder | Holds |
|---|---|
| [architecture/](architecture/README.md) | How the system works today: ownership, invariants, boundaries, data flow. Present tense, no history. |
| [operations/](operations/) | Repeatable procedures and their rules: the [performance playbook](operations/performance/README.md) and [releases](operations/release.md). |

Unfinished work is not documented here. It lives in the repository's GitHub
issues: one parent issue per body of work, with its items as sub-issues.

## Rules

- Front matter holds `kind` (index, architecture, operations, reference)
  and `status` (active, proposed, deferred, exploratory).
- Architecture holds only what code can't show: decisions and deliberate
  absences, invariants that cross modules or the app/core boundary, external
  adapter behavior, and ownership, each with one code pointer. No pin tables,
  file inventories, constant tables, or step-by-step behavior. Edit it when a
  rule changes, not after every feature.
- When work ships, its issue closes; a new rule it established moves into
  architecture in the pull request that closes it. Git history keeps
  provenance; there are no archives or redirect stubs.
- No PR numbers, commit hashes, or test-run evidence in docs.

Check documentation changes with `./scripts/check-docs.sh` and
`git diff --check`.
