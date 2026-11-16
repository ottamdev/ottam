---
kind: spec
status: active
---

# App Update & Distribution

Goal: ship notarized Ottam builds that update themselves through Sparkle, with
GitHub Releases hosting archives and a signed static appcast on a
project-owned domain as the feed. The first public binary already contains
the updater. The bundled `ottam-core`, GhosttyKit, adapter pins, resources,
and schema migrations move with the `.app`; there is no separate backend
updater. Repository policy (license, notices, community files) belongs to the
[open source release spec](../operations/open-source-release.md).

## Current Status

Ottam builds notarized archives that install and run on a clean macOS 26
account, and has a release workflow that has published to the test feed and
updated a real install. Nothing is published to production:

- `scripts/release-app.sh` cuts versions locally, enforces the release
  refusals, signs, notarizes, and staples the app, and packages it in a disk
  image beside an Applications link, then signs, notarizes, and staples the
  image;
  `--rehearsal` builds with Apple Development. `scripts/verify-release-app.sh`
  checks the build, then the image and the app mounted from it.
- Sparkle is pinned and embedded in every bundle. Release assembly gives the
  bundle its feed and public key (`--feed-url` points a test build at another
  feed), and only then does the app start an updater and show **Check for
  Updates…**. The Sparkle public key is `app/Signing/sparkle-public-key`.
- `.github/workflows/release.yml` runs on a dispatched `v*` tag after the
  maintainer approves the `release` environment: it builds through
  `release-app.sh`, then `scripts/publish-release.sh` signs the archive and
  appcast, creates the GitHub Release, and pushes the appcast to
  `ottamdev/updates`, which a Cloudflare static-assets Worker serves at
  `updates.ottam.dev` (`public/appcast.xml`, walkthroughs under
  `public/test/`). The feeds are named in `scripts/lib/release-feed.sh`.
  The `release` skill drives a release and the throwaway test-feed update
  walkthrough.
- `scripts/smoke-app-bundle.sh` validates development bundles; `--release`
  also checks a release build for test seams, which the release verifier
  repeats on the shipped bundle.

Development builds must stay cheap and local: no Apple credentials,
notarization, GitHub access, or Sparkle key.

## Active

### Milestone 3 — first public release

- [ ] **First stable releases.** Commit `release-notes/<version>.md` for the
  next version, cut, push the release commit and tag, wait for `full-tests`
  to pass on that commit, then
  `gh workflow run release.yml --ref v<version> -f feed=production`.
  Publish two releases before announcing downloads, so the public update
  path is proven from a build that actually shipped.

Try it:

1. Publish X and download its disk image from the GitHub Release in a browser
   on a clean macOS 26 account. The image opens to its background, with an
   arrow from Ottam to Applications; dragged across and opened, Ottam shows
   only the standard downloaded-app dialog, About shows X, onboarding
   installs an agent that completes a turn, and chats survive a quit.
2. Start publishing X+1 and cancel the run before the appcast push: the
   public feed is unchanged and X finds no update. Delete the release if one
   was created, and rerun to completion.
3. In the installed X, Check for Updates… offers X+1 with its notes, installs
   it, and relaunches into it over unauthenticated HTTPS only.

### Acceptance

Package checks: strict signature verification of every nested executable,
the app, and the disk image; `spctl` assessment; `stapler validate` on the
image and the app; and a re-inventory of the app as mounted from it. Update matrix, with real independently signed builds:

| Case | Expected |
|---|---|
| Same / lower bundle version | Not offered. |
| Higher bundle version | Offered with correct version and notes. |
| Tampered archive or appcast | Rejected. |
| Missing archive | Candidate publication fails; feed unchanged. |
| Wrong Developer ID identity | Rejected. |
| Core running | App and core replaced together; clean relaunch. |
| Agent mid-turn or terminal running | Install asks first, then takes the normal quit path. |
| Existing database | Startup migrations run; durable state remains. |
| Adapter pin moved | Old adapter keeps launching until the new pin lands. |
| Read-only or relocated install | Sparkle's authorization path or a recoverable error. |

### Failure recovery

- Bad asset before feed publication: delete the draft or publish a new
  version; no client saw it.
- Bad appcast: restore the previous signed appcast.
- Bad installed app: ship a higher `CFBundleVersion`; never replace bytes
  under an old tag or URL.
- Lost Sparkle key: stop releases and follow Sparkle's key rotation using the
  still-trusted Developer ID. Rotate the Apple identity or the Sparkle key in
  a release, never both at once.
- Host outage: repoint the feed domain at another HTTPS host and move the
  archives; the appcast names the new archive URLs, so no shipped build
  changes.

## Backlog

- Beta channel through a Sparkle channel with in-app opt-in (not GitHub's
  `latest`): `generate_appcast --channel beta` into the same production
  appcast, plus an `allowedChannels(for:)` delegate behind a Settings
  checkbox. Never through the test feed, which only walkthrough builds read.
- Phased rollouts and critical-update policy.
- Deltas, once several releases exist and the release script can retrieve
  the exact previous archives (`generate_appcast` needs them).
- An Updates settings section, if Sparkle's standard UI proves insufficient.
- A Homebrew cask marked `auto_updates`, once releases are regular.

## Decisions

- **The first public binary contains the updater** (2026-11-09). A build
  without one can only be upgraded by hand and cannot be told a newer build
  exists.
- **Releases publish from a workflow dispatched on a tag** (2026-11-14),
  replacing local-first publishing. Nobody depends on the repository yet, so
  the workflow can be proven on real releases; the logic stays in scripts the
  workflow calls, so a local run remains possible. Dispatch only, so pushing
  a tag never starts a production run.
- **Production releases ship only tested source** (2026-11-15): publishing
  to the production feed refuses a tagged commit without a passing
  `full-tests` push run on `main`, which runs every gate. The workflow does
  not run tests itself, so a release reuses CI's result rather than running
  every suite again. Test-feed walkthrough tags sit off `main` and skip it.
- **CI and releases run on GitHub's `xcode-27` preview image** (2026-11-14),
  until `macos-27` is generally available. Ottam is developed with Xcode 27,
  and a build linked against the 26 SDK behaves differently on macOS 27, so
  tests and releases share the developer toolchain. The deployment target
  stays macOS 26; nothing in CI runs on 26, so the clean-account check in
  milestone 3's Try it covers it.
- **The feed lives on a project-owned domain** (2026-11-09). `SUFeedURL` is
  the one address baked into every shipped build, and a `github.io` URL
  breaks on a repository rename or transfer.
- **A Cloudflare static-assets Worker serves the feed from its own
  repository** (2026-11-14). The domain, its DNS, and the planned private
  website are already on Cloudflare, git-integrated deploys need no release
  token, and Cloudflare now treats Pages as legacy. The feed stays out of the
  website's project so no site deploy can touch it. Archives stay on GitHub
  Releases, since static assets cap files at 25 MiB.
- **Release notes are embedded in the appcast** (2026-11-09), so the archive
  and the appcast are the only signed artifacts. They are a reviewed
  Markdown file per version, `release-notes/<version>.md`, which also
  becomes the GitHub Release body.
- **The updater exists only in release bundles** (2026-11-09). Development,
  `run-app.sh`, and UI-test bundles carry no feed URL, so they never start
  Sparkle or need its key.
- **Sparkle 2 is the updater.** Standard controller and UI; no custom
  downloader, installer, dialog, restart banner, progress indicator, or
  privileged helper. The one addition is the sidebar update badge, through
  Sparkle's gentle reminders (2026-11-15): a scheduled check should not cover
  a workspace with a window, and the badge still opens Sparkle's own.
  Sparkle replaces the whole `.app`. Ottam is not sandboxed, so no
  sandbox-only Sparkle XPC settings.
- **Releases host archives; a static appcast is the feed.** GitHub's release
  pages and Atom feeds are not appcasts. Each item points at an immutable
  tag URL
  (`https://github.com/<owner>/<repo>/releases/download/v26.11.0/Ottam-26.11.0.dmg`),
  never `/releases/latest/download/`. The app embeds only the feed URL and
  has no GitHub API dependency. Assets and feed are public; never embed a
  GitHub token.
- **Stable channel only at first** (reaffirmed 2026-11-14). Pre-releases stay
  out of the stable appcast. There is no beta audience yet, and a later
  stable build can add the opt-in, so the first binary doesn't need it.
- **Versions are `YY.M.N`** (2026-11-09): the UTC year and month of the cut
  plus a per-month release counter, with no leading zeros because Cargo
  requires semver. Ottam has no public API for semver to promise anything
  about, and a date plus a counter needs no human choice.
- **The tag is the version** (2026-11-09). Only the cut reads the date;
  everything after reads the tag `vYY.M.N`, so a rerun keeps its version.
  `CFBundleShortVersionString` and `CFBundleVersion` both equal it, the cut's
  release commit moves the Cargo workspace version to match, and a tag never
  moves.
- **The `ottam` CLI is not distributed** (2026-11-09), so it is not a release
  input.
- **Release credentials live in the protected `release` environment**
  (2026-11-14): the Developer ID `.p12`, an App Store Connect notary key, the
  Sparkle private key, and a write deploy key for `ottamdev/updates`. Only
  `v*` tags deploy and every run waits for the maintainer's approval, so a
  pushed tag alone cannot ship. `scripts/setup-release-secrets.sh` loads and
  rotates them; local runs keep using the keychain.
- **One disk image is both the download and the update** (2026-11-14),
  replacing the zip, because dragging to Applications keeps people from
  running Ottam out of Downloads and Sparkle installs from the same image.
- **The app and the image are each notarized and stapled** (2026-11-14), so
  Gatekeeper accepts a download opened offline, at the cost of a second
  notary submission per release.
- **The image's window layout is committed** (2026-11-14), made once by
  `scripts/layout-disk-image.sh`, because Finder scripting is unreliable on
  headless CI and writing `.DS_Store` without Finder needs non-stdlib code.
- **Trust layers.** HTTPS; Sparkle Ed25519 signatures on the archive and the
  appcast; Developer ID signing of all nested code; notarization and
  stapling. The Sparkle key and Developer ID are separate roots, backed up
  separately, never committed; rotation is a planned release.
- **An update replaces code only.** It never moves `~/.ottam`, workspace
  registration, agent homes, managed runtimes, logs, or the database;
  migrations run at normal startup
  ([persistence](../../architecture/backend/persistence.md)). Installed
  adapters follow a moved pin through core's background reinstall
  (`backend/crates/ottam-core/src/agent/agent_install_service.rs`).
- **Non-goals:** per-component updates; installs from branches, PR or
  Actions artifacts, or unsigned nightlies; GitHub source archives as
  updates; a Mac App Store flavor, which an unsandboxed app that spawns
  arbitrary processes cannot be; treating ad hoc signing,
  `codesign --deep --verify`, or a Release upload as production readiness;
  automatic install without Sparkle-managed consent.

References: [Sparkle docs](https://sparkle-project.org/documentation/),
[publishing](https://sparkle-project.org/documentation/publishing/),
[customization](https://sparkle-project.org/documentation/customization/),
[GitHub release links](https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases).
