---
name: now
description: Give an ad-hoc status update on Ottam from recent commits, open branches and PRs, and the specs they touch. Use when the user says /now or asks what's going on, what shipped, what's in flight, or what's next. Read-only; writes no status file.
---

# Now

Status is assembled on demand, not kept in a file. Nothing here edits docs;
use the `track` skill to change a spec.

## Steps

1. Pick the window: the user's ("since Monday", "this week"), otherwise the
   last 7 days.
2. Gather, in parallel:
   - `git fetch -q origin` then `git log origin/main --since=<window> --format='%h %ad %s' --date=short`
   - `gh pr list --state all --search 'updated:>=<date>' --json number,title,state,headRefName,mergedAt`
   - `git branch -r --sort=-committerdate --format='%(committerdate:short) %(refname:short)' | head -20`
     for unmerged work in flight
   - `git worktree list` for local work in progress
3. Map the work to specs: the files each commit or PR touches
   (`git log --name-only`), `docs/specs/**` paths it edits, and milestone or
   item ids in titles (`usage:3`, `M10`). Read `docs/specs/README.md`, then
   only the specs that moved in the window or that the user asked about.
4. Report briefly, lead with the answer:
   - **Shipped**: merged in the window, grouped by milestone or spec.
   - **In flight**: open PRs and recently active branches, with what each
     waits on (review, CI, a walkthrough, a decision).
   - **Next**: the next step from the specs that moved, plus anything blocked
     on the user.
   - **Stale**: specs whose index line or Current Status contradicts the
     commits (work shipped but still listed, or listed as next but already
     merged). Offer to fix them with `track`; don't edit.

Never claim CI passed or a walkthrough ran without seeing it.
