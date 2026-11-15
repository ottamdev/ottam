"""Seed a new isolated run-app.sh home so it skips first-launch onboarding.

Clones the source home's installed adapters and managed Node with APFS
clonefile (`cp -c`, never a symlink: core rewrites install roots), then
points each agent whose CLI setup is detected at that setup. Credentials stay
in the CLI's own home; managed agent homes are never copied, since their
rotating refresh tokens would fork.

Usage: seed_dev_home.py --home DIR --source DIR --inspect CMD [--core-bin PATH]
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


class SeedError(Exception):
    pass


def clone(source: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    completed = subprocess.run(["/bin/cp", "-cR", str(source), str(target)],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if completed.returncode != 0:
        raise SeedError(f"could not clone {source}: {completed.stderr.strip()}")


def clone_installs(source: Path, home: Path) -> list[str]:
    """Published adapter versions and managed Node; in-flight `.pending-*`,
    `.rollback-*` and staging entries stay behind."""
    cloned = []
    for adapter in sorted((source / "agents").glob("*/adapter")):
        harness = adapter.parent.name
        for version in sorted(adapter.iterdir()):
            if version.name.startswith(".") or not version.is_dir():
                continue
            target = home / "agents" / harness / "adapter" / version.name
            if not target.exists():
                clone(version, target)
                cloned.append(f"{harness} {version.name}")
    node = source / "runtime" / "node"
    if node.is_dir() and not (home / "runtime" / "node").exists():
        clone(node, home / "runtime" / "node")
        cloned.append("managed Node")
    return cloned


class Core:
    def __init__(self, inspect: str, home: Path, core_bin: str | None):
        self.command = [inspect, "--spawn-home", str(home)]
        if core_bin:
            self.command += ["--core-bin", core_bin]

    def call(self, method: str, params: dict | None = None) -> dict:
        completed = subprocess.run(
            [*self.command, "call", method, json.dumps(params or {})],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if completed.returncode != 0:
            raise SeedError(f"{method} failed: {completed.stderr.strip()}")
        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise SeedError(f"{method} answered non-JSON: {error}") from error


def use_cli_setups(core: Core) -> list[str]:
    """Each detected CLI setup becomes the harness's agent: the seeded
    unclaimed instance switches layout, as onboarding's CLI choice does."""
    setups = [setup for setup in core.call("agent.detectSetups")["setups"]
              if setup.get("available")]
    agents = {agent["harnessID"]: agent for agent in core.call("agent.list")["agents"]}
    used = []
    for setup in setups:
        harness = setup["harnessID"]
        agent = agents.get(harness)
        if agent and agent.get("homeKind") == "existing":
            pass
        elif agent and agent.get("canSetLayout"):
            core.call("agent.setLayout", {"agentID": agent["agentID"], "layout": "existing"})
        else:
            core.call("agent.create", {"harnessID": harness, "layout": "existing"})
        used.append(f"{setup.get('displayName', harness)} ({setup.get('path', '?')})")
    return used


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--inspect", required=True)
    parser.add_argument("--core-bin")
    arguments = parser.parse_args(argv)
    try:
        cloned = clone_installs(arguments.source, arguments.home)
        used = use_cli_setups(Core(arguments.inspect, arguments.home, arguments.core_bin))
    except SeedError as error:
        print(f"seed: {error}", file=sys.stderr)
        return 1
    print(f"Seeded {arguments.home}")
    print(f"  cloned from {arguments.source}: {', '.join(cloned) or 'nothing installed'}")
    print(f"  agents on CLI setups: {', '.join(used) or 'none detected'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
