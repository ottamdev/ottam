---
kind: spec
status: active
---

# Worktree Conversations

Parallel conversations and delegated children share one checkout and stomp
each other. A Worktree checkbox at conversation creation makes Core run
`git worktree add` on a new branch, and the conversation works there.

## Current Status

W1 (create, work, delete) is in progress. W2 (a per-project setup command)
starts once W1 ships; its Try it is below.

## Active

### W1: work in a worktree

- [~] `wt:1` Create, work in and delete a worktree conversation.
  - Why: two conversations on one project need separate checkouts.
  - Done when: every Try it step below works in the real window.
  - Verify with: `./scripts/test-rust-core.sh --filter worktree`,
    `--test jsonl_protocol --filter worktree`, focused Swift tests, the
    `chat.worktree-conversation` UI scenario, and the Try it with
    screenshots plus AX.

#### Try it

1. New chat on a git project: a Worktree checkbox sits under the composer.
   It is absent for Workspace files.
2. Tick it and send. The send button spins, the conversation opens, and the
   subtitle reads `<project> · <branch> · Worktree`.
3. Tools › Terminal: `pwd` is the worktree and `git branch --show-current`
   is the new branch. In the main checkout `git status` is unchanged and
   `git worktree list` shows the new entry.
4. The agent edits a file: Review shows it, the main checkout does not.
5. A delegated child runs in the same worktree.
6. Quit and relaunch: same subtitle, and the next message runs in the
   worktree.
7. Right-click › Delete Worktree…: the alert counts uncommitted changes.
   Confirm: the directory is gone, the transcript stays readable, the
   composer reads "This conversation's folder is unavailable".
8. Delete Conversation… on another one: the alert names the worktree.
   Afterwards the branch survives only if it has commits.
9. On a repo with no commits, ticking Worktree and sending shows an error
   under the composer, keeps the draft, and leaves nothing on disk.

## Next

### W2: setup command

- [ ] `wt:2` Run a per-project setup command before the first message.
  - Why: a fresh worktree lacks ignored files (vendored builds, `.env`).
  - Done when: every Try it step below works in the real window.
  - Verify with: `--test jsonl_protocol --filter worktree_setup`, focused
    Swift tests for the failure card, `./scripts/smoke-app-bundle.sh` for the
    new Settings page, and the Try it with screenshots plus AX.

#### Try it

1. Settings › Projects lists each project with a Worktree setup command
   field. Set `./scripts/setup-worktree.sh --links-only`.
2. New worktree chat: your message shows, then "Running setup…" with Show
   Output (live). The agent starts when it exits 0, and
   `app/Vendor/ottam-ghostty` is linked in the worktree.
3. Set the command to `exit 3`. A new worktree chat shows a card: setup
   failed, exit 3, output tail, and Retry Setup / Fix Here / Fix in New Chat.
4. Fix Here posts the failure plus your original request, and the agent runs.
5. Fix in New Chat opens a second conversation on the same worktree with
   that text. Fix the command, go back, Retry Setup: the held message
   delivers.
6. Stop while waiting cancels the run; setup keeps going until stopped from
   Show Output, which then shows the card.
7. Quit during setup, relaunch: the card says setup was interrupted, and
   Retry works.
8. A project with no command behaves as W1.

## Backlog

- Review diffs uncommitted changes only, so an agent that commits shows an
  empty Review ([Review Rendering](review-rendering.md) › Diff base).
- Worktrees made outside Ottam are not adopted.
- Removing a project from a workspace leaves its worktrees on disk.
- A crash mid-create can leave a directory with no row. It is logged, not
  swept: a sweep could delete real work after a database restore.
- Waiting on `git worktree add` has no cancel.

## Decisions

1. **Worktrees under Ottam's home (2026-11-18).** They live at
   `$OTTAM_HOME/worktrees/<project>/<name>` on a new branch off the primary
   checkout's HEAD, so Ottam owns their lifetime and never writes beside the
   user's checkout.
2. **A worktree is a non-primary ProjectSource (2026-11-18).** The
   conversation links that source, so availability, terminals and Review
   follow the existing source model instead of a parallel one.
3. **Setup is an Ottam setting (2026-11-18).** The command is per project in
   Settings › Projects, not a checked-in file, so it works on repositories
   the user does not own.
4. **The first message waits for setup (2026-11-18).** On failure the app
   shows the error with Retry, Fix Here and Fix in New Chat, so the agent
   never starts in a half-built checkout unless the user chooses it.
5. **Last conversation removes the worktree (2026-11-18).** Deleting a
   conversation removes its worktree only when no other conversation uses
   it, and the confirmation says so; the branch is kept unless it has no new
   commits, so committed work is never lost.
6. **No sidebar indicator (2026-11-18).** The window subtitle reads
   `<project> · <branch> · Worktree`, which keeps the sidebar uncluttered.
7. **Out of scope (2026-11-18).** Copy-on-write cloning of ignored files,
   merge-back, PR creation, per-subagent worktrees, adopting outside
   worktrees and a base-aware Review diff; each needs its own milestone.
