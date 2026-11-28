---
name: track
description: Create or update tracked work as GitHub issues, a parent issue per body of work with its items as sub-issues. Use when the user says /track, asks to track work, keep a checklist, or wants tasks persisted across sessions.
---

# Track

Unfinished work lives in GitHub issues, specific enough that a fresh agent
knows the next action, why it matters, where to look, and how to verify it.
A body of work is a parent issue; its items are sub-issues. There is no
status file, board, or index.

## Steps

1. Search before creating:
   `gh issue list --state all --search '<terms>' --json number,title,state,parent`.
2. Build a short queue: a few concrete, independently verifiable items, next
   action first. Group user-facing work into features that each end in
   something the user can verify in the app (`AGENTS.md` › Features).
3. Write the parent issue. The title names the feature.
   The body fills `.github/ISSUE_TEMPLATE/feature.md`, keeping its
   headings. No status section and no checklist: the sub-issues are the list.
4. Issues are public. Show the user the title and body, then create it with
   `gh issue create --title <title> --body-file <file> --type Feature`.
   `gh` skips the template's type, so pass it. Add `--label app` and/or
   `--label core` for the areas the work touches; add no other labels.
5. Create one sub-issue per item with `gh issue create --parent <n>
   --type Task` plus its area labels (`--type Bug` instead for a defect). Its
   body fills `.github/ISSUE_TEMPLATE/task.md`, one line per section
   where possible. Order items with `gh issue edit <n> --add-blocked-by <m>`,
   only where one really waits on another.
   `gh issue create` files the issue before it sets `--type` and `--parent`,
   so a failure there still leaves an issue, without printing its URL. Before
   retrying, find it with step 1's search and finish it with `gh issue edit
   <n> --type <type> --parent <parent>`.
6. Keep the body current. When a finding or decision changes, edit the body
   (`gh issue edit <n> --body-file <file>`) so it always reads as the present
   state; comments are for discussion, not for the record.
7. Status is open or closed. Add no status labels. An item is in progress
   when it has an open pull request.

## Finishing Work

A pull request fills `.github/PULL_REQUEST_TEMPLATE.md` and closes its item
with `Fixes #<n>` in its description. Only a new rule the code can't show (a
decision, a deliberate absence, a cross-module invariant, an adapter quirk)
moves into its `docs/architecture/` owner, in that same pull request; how the
feature works stays in the code.
Close the parent when its last sub-issue closes
(`gh issue view <n> --json subIssuesSummary`). Docs carry no issue or PR
numbers; issues and git history hold that record.

For a status update, use the `now` skill.
