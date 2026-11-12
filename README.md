<p align="center">
  <img alt="Ottam logo" src="assets/brand/mark.svg" width="128">
</p>

<h1 align="center">Ottam</h1>

Ottam is a native macOS app for running coding agents. It brings Claude Code,
Codex, OpenCode and oh-my-pi into one local workspace: talk to any of them,
review what they change, and hand work between them, while Ottam asks for as
little of your attention as it can.

Everything runs on your Mac. Agents run through their ACP adapters, signed in
with your own accounts; there is no Ottam account. A Rust core owns state,
Git, terminals and agent processes, and the SwiftUI/AppKit app talks to it
over a local JSONL protocol.

Ottam is an early prototype. There is no release build yet; you build it from
source.

## Features

- **Agent chat.** Conversations with Claude Code, Codex, OpenCode and
  oh-my-pi that persist across restarts. Send a follow-up while an agent is
  working, switch agents mid-conversation, and pick modes, commands and skills
  with `/`.
- **Permissions and questions in place.** Tool approvals and agent questions
  appear above the composer, with a per-conversation approval setting.
- **Plan review.** An agent's proposed plan opens in a Plan tab where you
  approve it or send it back.
- **Quote reply.** Select text in the transcript to quote it, with a comment,
  in your next message.
- **Tools pane.** Review the Git working tree's diff, open files, and run
  terminals (Ghostty) beside the conversation.
- **Delegation.** Agents can hand a task to another installed agent. Each
  child is its own conversation, nested under its parent, and its report comes
  back to the parent when it finishes.
- **MCP servers.** Add MCP servers once in Settings and every agent gets them,
  including servers that need you to sign in.
- **Usage.** Context-window and plan-usage gauges for the current agent.
- **Agent setup.** Settings › Agents installs each adapter at a pinned version
  and, if you have no suitable Node, a managed Node runtime.

## Setup

### Requirements

- macOS 26 on Apple silicon (`arm64` only).
- Xcode with its Swift 6.2+ toolchain selected
  (`sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`). The
  Command Line Tools alone are not enough: SwiftUI's macro plugins ship only
  inside Xcode.
- Rust and Cargo (`rust-toolchain.toml` pins the channel).
- For agents: nothing up front. An existing Node works too (Claude needs 22+,
  Codex 18+; OpenCode and oh-my-pi need none); `OTTAM_NODE` points at a
  specific `node`.
- For the test gates: the pinned diesel CLI. When it is missing,
  `./scripts/diesel-cli.sh` prints the install command; run that from the
  repository root.
- ripgrep (`brew install ripgrep`): several scripts and the agent workflows
  in `AGENTS.md` call `rg`.

### Build and run

```bash
./scripts/run-app.sh
```

This fetches GhosttyKit when it is missing, builds the Rust core and the app,
and launches an **Ottam Dev** instance named for the worktree, so several
checkouts can run side by side. To open a specific repository:

```bash
./scripts/run-app.sh /path/to/repo
```

`./scripts/build-app.sh` builds the bundle at `.build/app/Ottam.app`. For
Xcode, run `./scripts/fetch-ghosttykit.sh` first (Xcode loads the package
before any build step can fetch it), then open `app/Package.swift` and run
`OttamApp`; the shared scheme's pre-action builds the Rust core.

### First launch

Open Settings › Agents, install the agents you want, and sign in to each
inside Ottam. Ottam keeps its own agent homes under `~/.ottam`, so sign in
there even if the CLI is already signed in.

To start over, quit Ottam and run `./scripts/reset-app-data.sh --dry-run`,
then without `--dry-run`. It deletes `~/.ottam` after confirmation;
`--db-only` resets only the databases and keeps a backup under
`~/.ottam/backups/`.

## Contributing

[AGENTS.md](AGENTS.md) is the working guide for humans and coding agents alike:
conventions, which checks to run for which change, logs, and UI verification.
Install the commit hooks once with `./scripts/install-git-hooks.sh`. Docs start
at [docs/README.md](docs/README.md).

## License

Ottam is licensed under the [Apache License 2.0](LICENSE). Third-party
components keep their own licenses; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
