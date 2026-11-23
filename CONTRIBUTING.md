# Contributing to Ottam

Ottam is an early prototype and moves quickly, so please open an issue before
starting anything large. To report a vulnerability, follow
[SECURITY.md](SECURITY.md) instead of opening an issue.

[AGENTS.md](AGENTS.md) is the working guide for humans and coding agents alike:
conventions, logs, and UI verification. This file covers only what you need to
get a change in.

## Prerequisites

You need macOS 26 on Apple silicon, Xcode with its Swift 6.2+ toolchain
selected, and Rust. The [README](README.md#requirements) has the details and
the build steps.

Install the commit hooks once, and fetch GhosttyKit, which the Swift package
needs and the repository does not contain (`./scripts/run-app.sh` also fetches
it):

```bash
./scripts/install-git-hooks.sh
```

```bash
./scripts/fetch-ghosttykit.sh
```

## Checks

While working, run the focused check for your change from the
[Verification table](AGENTS.md#verification) in `AGENTS.md`. Before opening a
pull request against `main` that changes Rust or Swift code, run the full
gates:

```bash
./scripts/test-rust-core.sh
```

```bash
./scripts/test-app.sh
```

- For docs and scripts, run `./scripts/check-docs.sh` and `git diff --check`.
- For protocol changes, regenerate the Swift, schemas, and fixtures with the
  command in the Verification table. Never hand-edit them.
- Pull request CI does not run real-window UI checks. For UI changes, run the
  affected scenario locally and include what you saw in the pull request (see
  [UI Verification](AGENTS.md#ui-verification)). These runs also need XcodeGen
  2.46.0, an Apple Development signing identity, and Accessibility and Screen
  Recording access; [ui-testing/README.md](ui-testing/README.md) has the setup.
  If you can't run them, say so in the pull request so a maintainer can.

## Issues and pull requests

Issues start from a template in
[.github/ISSUE_TEMPLATE](.github/ISSUE_TEMPLATE): a milestone, a work item
under it, or a bug report. Pull requests start from
[the pull request template](.github/PULL_REQUEST_TEMPLATE.md). Keep the
headings. A pull request closes its issue with `Fixes #n`, lists the checks
you ran under Verification, and shows before and after under Visual proof, or
`N/A` with a reason when nothing visible changed.

## Contributor terms

Ottam is licensed under [Apache 2.0](LICENSE), and contributions are accepted
under the same terms (section 5 of the license). There is no CLA.

Every commit needs a [Developer Certificate of Origin](https://developercertificate.org/)
sign-off, which certifies you have the right to submit the work:

```bash
git commit -s
```

The sign-off is yours even when a coding agent wrote the change; agents
following `AGENTS.md` add it by default. No automated
check enforces it; missing sign-offs are caught in review.
