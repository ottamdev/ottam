---
name: track
description: Create or update tracked work as GitHub issues, a parent issue per body of work with its items as sub-issues. Use when the user says /track, asks to track work, keep a checklist, or wants work items persisted across sessions.
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
   action first. Group user-facing work into milestones that each end in
   something the user can try in the app (`AGENTS.md` › Milestones).
3. Write the parent issue. The title names the body of work or milestone.
   Body sections, in order: `Goal` (2–5 lines), `Try it` (for a milestone),
   `Findings` (lasting research an implementer needs), `Decisions` (what was
   decided, the date, a one-sentence reason). No status section and no
   checklist: the sub-issues are the list.
4. Issues are public. Show the user the title and body, then create it with
   `gh issue create --title <title> --body-file <file>`.
5. Create one sub-issue per item with `gh issue create --parent <n>`. Its
   body says what, why, done when, and verify with, one line each where
   possible. Order items with `gh issue edit <n> --add-blocked-by <m>`, only
   where one really waits on another.
6. Keep the body current. When a finding or decision changes, edit the body
   (`gh issue edit <n> --body-file <file>`) so it always reads as the present
   state; comments are for discussion, not for the record.
7. Status is open or closed. Add no status labels. An item is in progress
   when it has an open pull request.

## Finishing Work

A pull request closes its item with `Fixes #<n>` in its description. Only a
new rule the code can't show (a decision, a deliberate absence, a
cross-module invariant, an adapter quirk) moves into its `docs/architecture/`
owner, in that same pull request; how the feature works stays in the code.
Close the parent when its last sub-issue closes
(`gh issue view <n> --json subIssuesSummary`). Docs carry no issue or PR
numbers; issues and git history hold that record.

For a status update, use the `now` skill.
