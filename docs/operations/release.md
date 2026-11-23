---
kind: operations
status: active
---

# Release

The rules behind an Ottam release, their reasons, and how to recover from a
failed one. The `release` skill is the procedure and the release scripts own
each step; this page is what both assume.

## Failure recovery

- Bad asset before feed publication: delete the draft or publish a new
  version; no client saw it.
- Bad appcast: restore the previous signed appcast.
- Bad installed app: ship a higher `CFBundleVersion`; never replace bytes
  under an old tag or URL.
- Lost Sparkle key: stop releases and follow Sparkle's key rotation using the
  still-trusted Developer ID. Rotate the Apple identity or the Sparkle key in
  a release, never both at once. `scripts/install.sh` pins the Developer ID
  team, so a new Apple identity updates it in the same change.
- Host outage: repoint the feed domain at another HTTPS host and move the
  archives; the appcast names the new archive URLs, so no shipped build
  changes.

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
  stays macOS 26; nothing in CI runs on 26, so a clean-account check on
  macOS 26 covers it.
- **The feed lives on a project-owned domain** (2026-11-09). `SUFeedURL` is
  the one address baked into every shipped build, and a `github.io` URL
  breaks on a repository rename or transfer.
- **A Cloudflare static-assets Worker serves the feed from its own
  repository** (2026-11-14). The domain, its DNS, and the planned private
  website are already on Cloudflare, git-integrated deploys need no release
  token, and Cloudflare now treats Pages as legacy. The feed stays out of the
  website's project so no site deploy can touch it. Archives stay on GitHub
  Releases, since static assets cap files at 25 MiB.
- **ottam.dev offers a `curl | sh` installer** (2026-11-15).
  `scripts/install.sh` lives here, where it is reviewed and tested; the
  website repository, a separate Cloudflare static-assets Worker, redirects
  `/install.sh` to it on `main`. It reads the stable appcast rather than the
  GitHub API, which has rate limits and no stable filter; with no release in
  the feed it points to building from source. A curl download carries no
  quarantine flag, so Gatekeeper never assesses the app; the script checks
  the image and app signatures, team, and notarization itself before copying.
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
  (`https://github.com/<owner>/<repo>/releases/download/v26.11.1/Ottam.dmg`),
  never `/releases/latest/download/`. The app embeds only the feed URL and
  has no GitHub API dependency. Assets and feed are public; never embed a
  GitHub token.
- **Every release's disk image is named `Ottam.dmg`** (2026-11-16), so
  `/releases/latest/download/Ottam.dmg` is a stable download link for the
  website; the tag already keeps each feed URL distinct. Local builds stay
  `Ottam-<version>.dmg`. 26.11.0 shipped as `Ottam-26.11.0.dmg` and also
  carries an `Ottam.dmg` copy.
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
  ([persistence](../architecture/backend/persistence.md)). Installed
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
