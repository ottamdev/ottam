"""check-probe-leftovers.py counts each agent's stored sessions and reports growth."""
from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "check-probe-leftovers.py"


def run(*arguments: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(SCRIPT), *arguments],
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def touch(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{}\n")


class ProbeLeftovers(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ottam-probe-leftovers-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        agents = self.root / "ottam" / "agents"
        touch(agents / "claude/home/projects/-tmp-x/a.jsonl")
        touch(agents / "codex/home/sessions/2026/11/13/rollout-a.jsonl")
        touch(agents / "omp.work/home/sessions/-tmp/a.jsonl")
        (agents / "omp.work/home/acp-session-handles.json").write_text(json.dumps({"a": "b"}))
        (agents / "opencode/home").mkdir(parents=True)
        self.database = agents / "opencode/home/opencode.db"
        with sqlite3.connect(self.database) as connection:
            connection.execute("create table session_v2 (id text)")
            connection.execute("insert into session_v2 values ('one')")
        (agents / "unrelated/home").mkdir(parents=True)

    def snapshot(self, name: str) -> Path:
        result = run("snapshot", "--ottam-home", str(self.root / "ottam"))
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.root / name
        path.write_text(result.stdout)
        return path

    def test_snapshot_counts_each_profile_by_harness(self):
        counts = json.loads(self.snapshot("before.json").read_text())
        self.assertEqual(set(counts), {"claude", "codex", "omp.work", "opencode"})
        self.assertEqual(counts["claude"]["transcripts"], 1)
        self.assertEqual(counts["codex"]["rollouts"], 1)
        self.assertEqual((counts["omp.work"]["sessions"], counts["omp.work"]["handles"]), (1, 1))
        self.assertEqual(counts["opencode"]["sessions"], 1)

    def test_diff_is_quiet_when_nothing_grew(self):
        before = self.snapshot("before.json")
        after = self.snapshot("after.json")
        result = run("diff", str(before), str(after))
        self.assertEqual(result.returncode, 0)
        self.assertIn("no new sessions", result.stdout)

    def test_diff_fails_on_a_leftover(self):
        before = self.snapshot("before.json")
        with sqlite3.connect(self.database) as connection:
            connection.execute("insert into session_v2 values ('probe')")
        touch(self.root / "ottam/agents/omp.work/home/sessions/-tmp/probe.jsonl")
        after = self.snapshot("after.json")
        result = run("diff", str(before), str(after))
        self.assertEqual(result.returncode, 1)
        self.assertIn("opencode sessions: 1 -> 2", result.stdout)
        self.assertIn("omp.work sessions: 1 -> 2", result.stdout)

    def test_a_moved_opencode_table_fails_instead_of_counting_zero(self):
        with sqlite3.connect(self.database) as connection:
            connection.execute("alter table session_v2 rename to session_v3")
        result = run("snapshot", "--ottam-home", str(self.root / "ottam"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot count OpenCode sessions", result.stderr)

    def test_an_unreadable_handle_file_fails_instead_of_counting_zero(self):
        (self.root / "ottam/agents/omp.work/home/acp-session-handles.json").write_text("{not json")
        result = run("snapshot", "--ottam-home", str(self.root / "ottam"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot count Oh-My-Pi session handles", result.stderr)

    def test_snapshot_needs_a_home(self):
        self.assertNotEqual(run("snapshot").returncode, 0)


if __name__ == "__main__":
    unittest.main()
