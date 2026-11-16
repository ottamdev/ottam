---
name: release
description: Cut and publish an Ottam release through the existing release scripts and the release workflow, or run a throwaway update walkthrough on the test feed. Use when the user types /release or asks to release, ship a version, publish to the appcast, or test an update end to end.
user-invocable: true
---

# Release

Drive the release scripts; never reimplement a step they own. Rules and their
reasons live in `docs/specs/app/distribution.md` (Decisions, Failure
recovery). Script refusals (`Release refused:`, `Publish refused:`) are the
contract: fix the cause and rerun, never work around one.

The maintainer alone approves the `release` environment, holds the
credentials, and says yes to each push and dispatch. Ask before every step
marked **ask**. Never enter, export, or print a credential.

## Production release

1. **Preconditions.** Clean tree on `main`, equal to `origin/main`
   (`git fetch --tags origin main`). No leftover walkthrough tags on origin
   (`git ls-remote --tags origin`): the cut counts every `v*` tag, so one
   would skip versions.
2. **Version.** The cut picks it; preview with
   `source scripts/lib/release-version.sh && release_next_version "$(date -u +%F)" $(git tag -l 'v*')`.
3. **Notes.** Draft `release-notes/<version>.md` from the user-facing changes
   since the previous tag (`git log <prev-tag>..origin/main`): short bullets
   a user would care about, no PR numbers or internal names. The notes
   become the appcast's embedded notes and the GitHub Release body, so the
   user reviews the text. **Ask**, then land it through a normal PR and pull
   `main` once it merges.
4. **Cut.** `./scripts/release-app.sh --cut` makes the `chore(release)`
   commit and an annotated tag, locally only. If it picked a different
   version than the notes (a month rolled over), rename the notes in a new
   PR and cut again after deleting the local tag and commit.
5. **Push.** **Ask**, then `git push origin main v<version>`. This bypasses
   main's PR rule as the maintainer on purpose: the tag must be on `main`
   with its own SHA, which a PR merge would rewrite.
6. **Wait for tests.** Production publishing refuses a commit without a
   passing `full-tests` push run on `main`. Find it with
   `gh run list --repo ottamdev/ottam --workflow full-tests.yml --branch main --event push --commit <sha>`
   and wait with `gh run watch <id> --exit-status` in the background. If it
   fails, stop: fix on a branch and release the next version. A tag never
   moves.
7. **Dispatch.** **Ask**, then
   `gh workflow run release.yml --repo ottamdev/ottam --ref v<version> -f feed=production`.
   Find the run with
   `gh run list --repo ottamdev/ottam --workflow release.yml --limit 1`.
   Tell the user it is waiting for their approval of `release`;
   `gh api repos/ottamdev/ottam/actions/runs/<id>/pending_deployments`
   shows whether it still waits. Watch the run in the background (about 11
   minutes after approval).
8. **Verify.** `gh release view v<version> --repo ottamdev/ottam` shows the
   `Ottam-<version>.dmg` asset, and
   `curl -fsS https://updates.ottam.dev/appcast.xml` lists the version. For
   package checks, download the image and run
   `./scripts/verify-release-app.sh <dmg> --version <version> --distribution --feed-url https://updates.ottam.dev/appcast.xml`.
   Report what ran; an update from the previous release in a real window is
   the user's check unless they ask for it.

On a failed run, read the failing step's log
(`gh run view <id> --log-failed`) and follow Failure recovery in the
distribution spec. Before the appcast push no client saw anything: delete a
release the run created (**ask**) and rerun the same tag. After it, ship a
higher version; never replace bytes under an old tag or URL.

## Update walkthrough (test feed)

A pair of throwaway versions proves an update in real windows without
touching production. Never add an ATS exception to reach a local feed.

1. In a scratch worktree off `main`, bump `[workspace.package] version` in
   `backend/Cargo.toml` to a high throwaway `26.<month>.90N`, run
   `cargo update --workspace --offline --manifest-path backend/Cargo.toml`,
   commit, and `git tag -a`.
2. Build N locally against the test feed:
   `source scripts/lib/release-feed.sh && ./scripts/release-app.sh --feed-url "$RELEASE_TEST_FEED_URL"`.
   Install it from the image with `release_attach_disk_image`
   (`scripts/lib/release-disk-image.sh`) and `ditto` into `/Applications`
   (**ask**: it replaces the installed app).
3. Tag N+1 the same way, committing a one-line
   `release-notes/<N+1>.md` with the version bump: publishing refuses a
   tag without notes on either feed. Then **ask**, push only that tag, and
   `gh workflow run release.yml --repo ottamdev/ottam --ref v<N+1> -f feed=test`.
   The user approves `release`. Wait for the run to succeed and
   `curl -fsS https://updates.ottam.dev/test/appcast.xml` to list N+1
   before going on.
4. Run the walkthrough with the user. A scheduled check needs automatic
   checks on: release builds leave them unset, so Sparkle asks for consent,
   and the user answers. Once on, it fires at launch when `SULastCheckTime`
   is more than a day old
   (`defaults write dev.ottam.Ottam SULastCheckTime -date "<two days ago>"`).
   Without consent, use **Check for Updates…**, which skips the reminder
   path.
5. Clean up, **ask** first: in a clone of `ottamdev/updates`,
   `git rm public/test/appcast.xml`, commit, and push `main`; then
   `gh release delete v<N+1> --repo ottamdev/ottam --cleanup-tag --yes`,
   delete N's tag on origin if it was pushed, and remove local tags, the
   scratch worktree, and its branch.
