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
account, and has a release workflow that has not run yet. Nothing is
published:

- `scripts/release-app.sh` cuts versions locally, enforces the release
  refusals, signs, notarizes, staples, and archives; `--rehearsal` builds
  with Apple Development. `scripts/verify-release-app.sh` checks the build
  and the extracted archive.
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
- `scripts/smoke-app-bundle.sh` validates development bundles; `--release`
  also checks a release build for test seams, which the release verifier
  repeats on the shipped bundle.

Development builds must stay cheap and local: no Apple credentials,
notarization, GitHub access, or Sparkle key.

## Active

### Milestone 2 — update from inside the app

- [~] **Sparkle key backup.** The private key lives in the maintainer's login
  keychain under the `ottam` account (Sparkle's tools are under
  `.build/swift/artifacts/sparkle/Sparkle/bin/` after a build; `sign_update`
  takes the same `--account`). Left: export a backup with
  `generate_keys --account ottam -x <file>` and store it apart from the
  Developer ID backup.
  - Why: a lost key stops releases until a key rotation ships.
  - Done when: the maintainer confirms the backup exists.
- [ ] **Update walkthrough.** Try it 1–6 below.
  - Why: an install quits through the normal path, so the shutdown-readiness
    gate in `applicationShouldTerminate`
    (`app/Sources/OttamApp/App/Main.swift`) applies. Prove Sparkle waits for
    it rather than assuming.
  - Needs: the release workflow on `main` (dispatch requires it there). N is
    a local build of throwaway tag `v26.11.900` with `--feed-url` set to the
    test feed; N+1 is throwaway tag `v26.11.901`, published by the workflow
    with `feed=test`, which is also the workflow's first run. Never add an
    ATS exception to reach a local feed.
  - Done when: steps 1–6 pass in real windows with screenshots and AX, and
    the walkthrough's pre-release, tag, and test feed are deleted.

Try it, with two Developer ID-signed builds N and N+1 and a test feed injected
at assembly:

1. Launch N a second time: Sparkle asks once whether to check automatically.
2. Ottam › Check for Updates…: Sparkle's standard window offers N+1 with its
   release notes.
3. Choose Remind Me Later: N keeps running untouched. Check again and choose
   Skip This Version: automatic checks stop offering N+1; a manual check
   still does.
4. Install and Relaunch while idle: the app relaunches as N+1 (About shows
   it) with workspaces, chats, and settings intact. Record whether the
   workspace windows reopen with "Close windows when quitting an
   application" on and off.
5. Repeat with an agent mid-turn and a terminal running: the install waits on
   the same quit behavior as ⌘Q, and nothing is killed outside it.
6. Offline, Check for Updates… reports the failure and the app stays usable.
7. A `run-app.sh` build has no Check for Updates… item and requests no feed.

## Next

### Milestone 3 — first public release

- [ ] **Release only tested source.** The workflow builds the tag without
  running tests. Refuse a tag whose commit has no passing `full-tests` run
  on `main`.
- [ ] **First stable releases.** Commit `release-notes/<version>.md` for the
  next version, cut, push the release commit and tag, then
  `gh workflow run release.yml --ref v<version> -f feed=production`.
  Publish two releases before announcing downloads, so the public update
  path is proven from a build that actually shipped.

Try it:

1. Publish X and download it from the GitHub Release in a browser on a clean
   macOS 26 account. Opening it shows only the standard downloaded-app dialog,
   About shows X, onboarding installs an agent that completes a turn, and
   chats survive a quit.
2. Start publishing X+1 and cancel the run before the appcast push: the
   public feed is unchanged and X finds no update. Delete the release if one
   was created, and rerun to completion.
3. In the installed X, Check for Updates… offers X+1 with its notes, installs
   it, and relaunches into it over unauthenticated HTTPS only.

### Acceptance

Package checks: strict signature verification of every nested executable
and the app, `spctl` assessment, `stapler validate`, and a re-inventory after
archive extraction. Update matrix, with real independently signed builds:

| Case | Expected |
|---|---|
| Same / lower bundle version | Not offered. |
| Higher bundle version | Offered with correct version and notes. |
| Tampered archive or appcast | Rejected. |
| Missing archive | Candidate publication fails; feed unchanged. |
| Wrong Developer ID identity | Rejected. |
| Offline | Current app stays usable. |
| Core running | App and core replaced together; clean relaunch. |
| Agent mid-turn or terminal running | Install waits on the normal quit path. |
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
- Optional `.dmg` for first installs, separate from the Sparkle zip.
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
  becomes the GitHub Release body.- **The updater exists only in release bundles** (2026-11-09). Development,
  `run-app.sh`, and UI-test bundles carry no feed URL, so they never start
  Sparkle or need its key.
- **Sparkle 2 is the updater.** Standard controller and UI; no custom
  downloader, installer, dialog, badge, restart banner, or privileged helper.
  Sparkle replaces the whole `.app`. Ottam is not sandboxed, so no
  sandbox-only Sparkle XPC settings.
- **Releases host archives; a static appcast is the feed.** GitHub's release
  pages and Atom feeds are not appcasts. Each item points at an immutable
  tag URL
  (`https://github.com/<owner>/<repo>/releases/download/v26.11.0/Ottam-26.11.0.zip`),
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
- **Full zip archives first**, created with `ditto` so symlinks, permissions,
  signature xattrs, and the outer app directory survive.
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
