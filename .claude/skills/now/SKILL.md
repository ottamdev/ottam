---
name: now
description: Give an ad-hoc status update on Ottam from recent commits, open branches and PRs, and the issues they close. Use when the user says /now or asks what's going on, what shipped, what's in flight, or what's next. Read-only; writes no status file.
---

# Now

Status is assembled on demand, not kept in a file. Nothing here edits
issues; use the `track` skill to change one.

## Steps

1. Pick the window: the user's ("since Monday", "this week"), otherwise the
   last 7 days.
2. Gather, in parallel:
   - `git fetch -q origin` then `git log origin/main --since=<window> --format='%h %ad %s' --date=short`
   - `gh pr list --state all --search 'updated:>=<date>' --json number,title,state,headRefName,mergedAt,closingIssuesReferences`
   - `gh issue list --state all --search 'updated:>=<date>' --json number,title,state,closedAt,parent,subIssuesSummary`
   - `gh issue list --state open --limit 200 --json number,title,parent,blockedBy,subIssuesSummary`
     for what is open and what it waits on
   - `git branch -r --sort=-committerdate --format='%(committerdate:short) %(refname:short)' | head -20`
     for unmerged work in flight
   - `git worktree list` for local work in progress
3. Map the work to issues through each PR's closing references and each
   issue's parent. Read only the parent issues that moved in the window or
   that the user asked about (`gh issue view <n>`).
4. Report briefly, lead with the answer:
   - **Shipped**: merged in the window, grouped by parent issue.
   - **In flight**: open PRs and recently active branches, with what each
     waits on (review, CI, its verification steps, a decision).
   - **Next**: open sub-issues with nothing blocking them, under the parents
     that moved, plus anything blocked on the user.
   - **Stale**: issues that contradict the commits (work merged but the
     issue still open, a parent open with every sub-issue closed, a body
     that describes a plan since replaced). Offer to fix them with `track`;
     don't edit.

Never claim CI passed or verification steps ran without seeing it.
