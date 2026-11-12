---
name: push
description: "Scan all git changes, group them into logical commits, validate Ottam, and push to the remote. Use when the user types /push or asks to push, commit and push, or ship changes."
user-invocable: true
---

# Push

Scan uncommitted changes, group them by unit of work, commit, and push the current branch.

## Steps

1. Gather only the context needed:
   - `git status --short`
   - `git branch --show-current`
   - `git diff --stat`

2. Safety-check file names before staging. Flag suspicious names such as `.env*`, `*key*`, `*secret*`, `*credential*`, or `*.pem`; inspect contents only when the name is ambiguous.

3. Test the affected behavior before committing/pushing. Inspect the branch diff
   against its base as well as uncommitted changes so earlier commits are covered.
   Follow `AGENTS.md` for applicable build, protocol, packaging, and UI checks.
   Prefer focused commands:
   - `./scripts/test-rust-core.sh --lib --filter <pattern>`
   - `./scripts/test-rust-core.sh --test <target> [--filter <pattern>]`
   - `./scripts/test-app.sh --filter <pattern>`

   Reuse passing results while relevant code is unchanged. Full suites
   (`./scripts/test-rust-core.sh` and `./scripts/test-app.sh`) belong in CI;
   `full-tests` runs on every PR, using Linux first and macOS for affected
   Apple-specific checks. New commits cancel superseded runs. Do not claim
   pending CI passed or repeat full suites just to push.
   Install the lightweight hook with `./scripts/install-git-hooks.sh` if needed;
   fix hook failures rather than bypassing them. There is no pre-push hook.

4. Do not broaden UI validation simply because the branch is being pushed.
   Native Swift tests are the default local gate. Real-window scenarios are
   manual and per-scenario; run more than one only when separately affected
   behaviors require it or the user explicitly asks. The bundle smoke is the
   only complete visual run.

5. Group changes by unit of work, not by directory. Keep one coherent change as one commit; split unrelated work into separate commits.

6. Stage by explicit path, never `git add .` or `git add -A`.

7. Commit with a concise conventional message:
   - `feat: ...`
   - `fix: ...`
   - `docs: ...`
   - `test: ...`
   - `chore: ...`
   - `refactor: ...`

   Add `-s` only when the user asks; the DCO sign-off is their certification
   (see `CONTRIBUTING.md`).

8. Push:

```sh
git push origin <branch>
```

Use `-u` when no upstream exists. Never force push unless the user explicitly asks and the risk has been called out.

9. Summarize commits, branch, push result, and the exact validation commands.
