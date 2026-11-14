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

Ottam cuts a version and produces a notarized, stapled archive locally that
installs and runs on a clean macOS 26 account. Nothing is published yet:

- `scripts/release-app.sh` cuts versions, enforces the release refusals,
  signs, notarizes, staples, and archives; `--rehearsal` builds with Apple
  Development for local testing. `scripts/verify-release-app.sh` checks the
  build and the extracted archive.
- No appcast or Sparkle dependency. No workflow has a tag or release trigger
  or uses secrets.
- `scripts/smoke-app-bundle.sh` validates development bundles; `--release`
  also checks a release build for test seams, which the release verifier
  repeats on the shipped bundle.

Development builds must stay cheap and local: no Apple credentials,
notarization, GitHub access, or Sparkle key.

## Active

### Milestone 2 — update from inside the app

- Pin an exact Sparkle 2 release, 2.9 or later (signed feeds need 2.9), in
  `app/Package.swift` for `OttamApp`.
- Embed it in the hand-built bundle: copy `Sparkle.framework` to
  `Contents/Frameworks` without dereferencing symlinks, add an
  `@executable_path/../Frameworks` runpath, keep its helper apps and XPC
  services, and sign nested Sparkle code, `ottam-core`, the executable, the
  framework, then the app.
- Release assembly injects `SUFeedURL`, `SUPublicEDKey`,
  `SURequireSignedFeed = YES`, and `SUVerifyUpdateBeforeExtraction = YES`.
  Keep Sparkle's second-launch consent prompt for automatic checks.
- One `SPUStandardUpdaterController` for the process lifetime, owned at the
  app seam, not a workspace window, and built only when the bundle has a feed
  URL. **Check for Updates…** sits in the standard application command group,
  enabled by Sparkle's state.
- An install quits through the normal path, so the shutdown-readiness gate in
  `applicationShouldTerminate`
  (`app/Sources/OttamApp/App/Main.swift`) applies; prove Sparkle waits for it
  rather than assuming.
- Log updater lifecycle under the app's subsystem, never feed contents,
  signed URLs, or secrets.
- Generate and back up the Ed25519 keypair; extend bundle smoke for the
  framework and helpers.

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

- Serve the feed from a project-owned domain
  (`https://updates.ottam.dev/appcast.xml`), hosted on GitHub Pages by CNAME.
- A local publish script; the appcast is the commit point:
  1. Validate a signed `vX.Y.Z` tag newer than the stable feed.
  2. Build and test the tagged source.
  3. Assemble, sign, notarize, staple, archive.
  4. Extract the archive and re-verify the bundle.
  5. Sign the archive, build the candidate appcast with the release notes
     embedded, and sign the appcast.
  6. Create the GitHub Release with immutable filenames.
  7. Fetch the public asset without credentials; check size and digest.
  8. Probe the candidate appcast against the previous release.
  9. Publish the signed appcast last, refusing if the public feed changed
     since the candidate was built.
  10. Fetch the public feed and probe again.
- Release notes are a reviewed file per version in the repository, not
  GitHub's release body.
- Publish two releases before announcing downloads, so the public update path
  is proven from a build that actually shipped.

Try it:

1. Publish X and download it from the GitHub Release in a browser on a clean
   macOS 26 account. Opening it shows only the standard downloaded-app dialog,
   About shows X, onboarding installs an agent that completes a turn, and
   chats survive a quit.
2. Start publishing X+1 and interrupt before step 9: the public feed is
   unchanged and X finds no update. Rerun to completion.
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

- A tag-triggered macOS release workflow that calls the same scripts, with
  concurrency control so two publications cannot race the appcast.
  Credentials live only in a protected environment and import into an
  ephemeral keychain deleted on every exit path; YAML holds only credential
  import, notarization submission, publication, and Pages deployment.
- Beta channel through a Sparkle channel with in-app opt-in (not GitHub's
  `latest`).
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
- **Releases are cut by a local script first; the CI workflow is backlog**
  (2026-11-09). The release rules were always meant to be locally runnable,
  and this keeps signing credentials out of a newly public repository.
- **The feed lives on a project-owned domain** (2026-11-09). `SUFeedURL` is
  the one address baked into every shipped build, and a `github.io` URL
  breaks on a repository rename or transfer.
- **Release notes are embedded in the appcast** (2026-11-09), so the archive
  and the appcast are the only signed artifacts.
- **The updater exists only in release bundles** (2026-11-09). Development,
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
- **Stable channel only at first.** Pre-releases stay out of the stable
  appcast.
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
- **Release credentials stay in the maintainer's keychain** (2026-11-10): the
  Developer ID identity and the `ottam-notary` notarytool profile, so a
  release needs no secret in the repository or its environment.
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
