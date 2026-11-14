#!/usr/bin/env python3
"""Count the sessions each agent has stored, to catch a model-catalog probe
that leaves one behind.

The probe's cleanup (`catalog_probe` on each definition in
backend/crates/ottam-core/src/agent/registry.rs) rests on what a pinned
adapter stores for a bare `session/new`. Recheck it at every pin bump:

    scripts/check-probe-leftovers.py snapshot --ottam-home <dir> > before.json
    # Recheck each agent in Settings › Agents and wait for its models.
    scripts/check-probe-leftovers.py snapshot --ottam-home <dir> > after.json
    scripts/check-probe-leftovers.py diff before.json after.json

`diff` exits 1 when any count grew. Add `--cli` to include the CLIs' own
homes (~/.claude, ~/.codex, ~/.omp/agent, OpenCode's data directory), which
the probe reaches through "Use my CLI setup". Reads only; never writes to a
home.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path

HARNESSES = ("claude", "codex", "opencode", "omp")


def count_files(root: Path, pattern: str) -> int:
    return sum(1 for path in root.glob(pattern) if path.is_file()) if root.is_dir() else 0


def opencode_sessions(database: Path) -> int:
    """Rows in OpenCode's session table. An unreadable table is an error, not
    zero: a schema change at a pin bump is what this check exists to catch."""
    if not database.is_file():
        return 0
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        return connection.execute("select count(*) from session_v2").fetchone()[0]
    except sqlite3.OperationalError as error:
        raise SystemExit(f"cannot count OpenCode sessions in {database}: {error}") from error
    finally:
        connection.close()


def omp_handles(agent_dir: Path) -> int:
    path = agent_dir / "acp-session-handles.json"
    try:
        return len(json.loads(path.read_text()))
    except FileNotFoundError:
        return 0
    except (OSError, ValueError) as error:
        raise SystemExit(f"cannot count Oh-My-Pi session handles in {path}: {error}") from error


def counts(harness: str, home: Path, opencode_database: Path | None = None) -> dict[str, int]:
    """What `harness` has stored in `home`, by kind."""
    if harness == "claude":
        return {"transcripts": count_files(home / "projects", "**/*.jsonl")}
    if harness == "codex":
        return {
            "rollouts": count_files(home / "sessions", "**/*.jsonl"),
            "archived": count_files(home / "archived_sessions", "**/*.jsonl"),
        }
    if harness == "opencode":
        return {"sessions": opencode_sessions(opencode_database or home / "opencode.db")}
    if harness == "omp":
        return {
            "sessions": count_files(home / "sessions", "**/*.jsonl"),
            "handles": omp_handles(home),
        }
    raise ValueError(f"unknown harness {harness}")


def ottam_homes(ottam_home: Path) -> list[tuple[str, str, Path]]:
    """(label, harness, home) for every Ottam profile under `ottam_home`."""
    homes = []
    for home in sorted((ottam_home / "agents").glob("*/home")):
        agent = home.parent.name
        harness = agent.split(".", 1)[0]
        if harness in HARNESSES:
            homes.append((agent, harness, home))
    return homes


def cli_homes(user_home: Path) -> list[tuple[str, str, Path, Path | None]]:
    opencode_data = user_home / ".local/share/opencode"
    return [
        ("cli:claude", "claude", user_home / ".claude", None),
        ("cli:codex", "codex", user_home / ".codex", None),
        ("cli:opencode", "opencode", opencode_data, opencode_data / "opencode.db"),
        ("cli:omp", "omp", user_home / ".omp/agent", None),
    ]


def snapshot(ottam_home: Path | None, include_cli: bool, user_home: Path) -> dict:
    result: dict[str, dict] = {}
    if ottam_home is not None:
        for label, harness, home in ottam_homes(ottam_home):
            result[label] = {"home": str(home), **counts(harness, home)}
    if include_cli:
        for label, harness, home, database in cli_homes(user_home):
            if home.exists():
                result[label] = {"home": str(home), **counts(harness, home, database)}
    return result


def grown(before: dict, after: dict) -> list[str]:
    lines = []
    for label, entry in sorted(after.items()):
        for kind, value in entry.items():
            if kind == "home":
                continue
            previous = before.get(label, {}).get(kind, 0)
            if value > previous:
                lines.append(f"{label} {kind}: {previous} -> {value} ({entry['home']})")
    return lines


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    take = commands.add_parser("snapshot", help="print stored-session counts as JSON")
    take.add_argument("--ottam-home", type=Path, help="an OTTAM_HOME; counts each profile under agents/")
    take.add_argument("--cli", action="store_true", help="also count the CLIs' own homes")
    compare = commands.add_parser("diff", help="report counts that grew; exit 1 if any did")
    compare.add_argument("before", type=Path)
    compare.add_argument("after", type=Path)
    arguments = parser.parse_args(argv)

    if arguments.command == "snapshot":
        if arguments.ottam_home is None and not arguments.cli:
            parser.error("snapshot needs --ottam-home, --cli, or both")
        json.dump(snapshot(arguments.ottam_home, arguments.cli, Path.home()), sys.stdout, indent=2, sort_keys=True)
        print()
        return 0
    lines = grown(json.loads(arguments.before.read_text()), json.loads(arguments.after.read_text()))
    for line in lines:
        print(line)
    if not lines:
        print("no new sessions")
    return 1 if lines else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
