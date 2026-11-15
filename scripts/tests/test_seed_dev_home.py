"""seed_dev_home.py clones installs and moves detected CLI setups onto agents."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "lib" / "seed_dev_home.py"

# Answers like ottam-inspect's `call`, and logs each call's method and params.
FAKE_INSPECT = r'''#!/usr/bin/env python3
import json, os, sys
arguments = sys.argv[1:]
method, params = arguments[-2], json.loads(arguments[-1])
with open(os.environ["FAKE_INSPECT_LOG"], "a") as log:
    log.write(json.dumps({"argv": arguments[:-3], "method": method, "params": params}) + "\n")
if os.environ.get("FAKE_INSPECT_FAIL") == method:
    sys.stderr.write("core refused\n")
    sys.exit(70)
answers = {
    "agent.detectSetups": {"setups": [
        {"harnessID": "claude", "displayName": "Claude Code", "path": "/cli/claude", "available": True},
        {"harnessID": "codex", "displayName": "Codex", "path": "/cli/codex", "available": True},
        {"harnessID": "omp", "displayName": "Oh-My-Pi", "path": "/cli/omp", "available": False},
    ]},
    "agent.list": {"agents": [
        {"agentID": "claude", "harnessID": "claude", "homeKind": "managed", "canSetLayout": True},
        {"agentID": "codex", "harnessID": "codex", "homeKind": "managed", "canSetLayout": False},
        {"agentID": "omp", "harnessID": "omp", "homeKind": "managed", "canSetLayout": True},
    ]},
}
print(json.dumps(answers.get(method, {"agentID": params.get("agentID", "new")})))
'''


def write(path: Path, text: str = "x") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


class SeedDevHome(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ottam-seed-dev-home-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.source = root / "source"
        self.home = root / "home"
        write(self.source / "agents/claude/adapter/0.100.0/node_modules/a.js")
        write(self.source / "agents/claude/adapter/.pending-0.101.0/b.js")
        write(self.source / "agents/claude/home/.credentials.json", "secret")
        write(self.source / "agents/codex/adapter/2.5.0/c.js")
        write(self.source / "runtime/node/bin/node")
        self.inspect = root / "fake-inspect"
        write(self.inspect, FAKE_INSPECT)
        self.inspect.chmod(0o755)
        self.log = root / "calls.jsonl"

    def run_seed(self, **environment: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--home", str(self.home), "--source", str(self.source),
             "--inspect", str(self.inspect), "--core-bin", "/bundle/ottam-core"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            env={**os.environ, "FAKE_INSPECT_LOG": str(self.log), **environment})

    def calls(self) -> list[dict]:
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_clones_published_installs_but_no_agent_homes(self):
        completed = self.run_seed()
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertTrue((self.home / "agents/claude/adapter/0.100.0/node_modules/a.js").is_file())
        self.assertTrue((self.home / "agents/codex/adapter/2.5.0/c.js").is_file())
        self.assertTrue((self.home / "runtime/node/bin/node").is_file())
        self.assertFalse((self.home / "agents/claude/adapter/.pending-0.101.0").exists())
        self.assertFalse((self.home / "agents/claude/home").exists())
        self.assertFalse((self.home / "agents/claude/adapter/0.100.0").is_symlink())

    def test_moves_available_setups_onto_agents_in_the_target_home(self):
        self.assertEqual(self.run_seed().returncode, 0)
        calls = self.calls()
        self.assertTrue(all(call["argv"] == ["--spawn-home", str(self.home),
                                             "--core-bin", "/bundle/ottam-core"] for call in calls))
        self.assertEqual(
            [(call["method"], call["params"]) for call in calls],
            [("agent.detectSetups", {}),
             ("agent.list", {}),
             ("agent.setLayout", {"agentID": "claude", "layout": "existing"}),
             ("agent.create", {"harnessID": "codex", "layout": "existing"})])

    def test_a_refused_call_fails_the_seed(self):
        completed = self.run_seed(FAKE_INSPECT_FAIL="agent.setLayout")
        self.assertEqual(completed.returncode, 1)
        self.assertIn("agent.setLayout failed: core refused", completed.stderr)


if __name__ == "__main__":
    unittest.main()
