# Security Policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub:
[open a private report](https://github.com/ottamdev/ottam/security/advisories/new),
or use **Report a vulnerability** on the repository's Security tab. Please do
not describe the problem in a public issue. If the form is unavailable, open an
issue asking for a private way to reach the maintainers, with no details.

Include the commit you tested, steps to reproduce, and the impact you expect.

## Support status

Ottam is an early prototype with no release builds, so only the current `main`
branch is supported. Fixes land on `main` and are not backported. Reports are
handled on a best-effort basis, with no guaranteed response time.

## Scope

Ottam runs on your Mac and has no Ottam account. Agents run as your macOS user:
Ottam adds no sandbox of its own, so an agent's reach is set by its permission
mode and the approvals you give it.

In scope:

- The macOS app and `ottam-core`, including how they handle the app-to-core
  protocol, local data under `~/.ottam`, and terminals.
- ACP adapter handling: launching adapters, relaying their traffic, and any
  permission request answered without you, other than by the conversation's
  **Always allow tool actions** setting.
- Installing adapters and the managed Node runtime, including pinned versions
  and download verification.
- MCP server secrets and sign-in.
- Ottam's GhosttyKit build: its manual-IO patch and the artifact
  `scripts/fetch-ghosttykit.sh` downloads.

Out of scope:

- Bugs in Claude Code, Codex, OpenCode, oh-my-pi, their ACP adapters, or
  upstream Ghostty. Report those upstream, unless Ottam's handling is what
  makes them exploitable.
- Actions an agent takes after you approved them, turned on **Always allow tool
  actions** for its conversation (delegated child conversations use the
  parent's setting), or chose a mode that bypasses approvals.
