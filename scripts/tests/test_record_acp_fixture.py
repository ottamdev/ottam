"""record-acp-fixture.py scrubs a recording, refuses leaks, and keeps the
fixture framing the Rust tests read. Offline: nothing spawns an adapter."""
from __future__ import annotations

import importlib.util
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "record-acp-fixture.py"
spec = importlib.util.spec_from_file_location("record_acp_fixture", SCRIPT)
recorder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recorder)

HOME = "/Users/someone"
WORKSPACE = Path("/var/folders/xy/T/ottam-fixture-abc")
NODE = f"{HOME}/.asdf/installs/nodejs/25.9.0/bin/node"
NPX = f"{HOME}/.npm/_npx/0123abcd/node_modules/.bin/claude-agent-acp"


def auth(email="someone@corp.test", organization="someone@corp.test's Organization"):
    return {"jsonrpc": "2.0", "method": "_auth/status_update", "params": {"authStatus": {
        "kind": "account", "label": f"Claude Max · {email}",
        "account": {"email": email, "organization": organization}}}}


def update(kind, session="s"):
    return {"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": session, "update": {"sessionUpdate": kind}}}


class Scrub(unittest.TestCase):
    def scrubbed(self, frames):
        hidden = recorder.secrets(frames)
        pairs = recorder.replacements(WORKSPACE, NODE, HOME, hidden)
        return recorder.scrub(frames, pairs), hidden

    def test_account_values_become_placeholders_by_field(self):
        frames, _ = self.scrubbed([auth()])
        account = frames[0]["params"]["authStatus"]["account"]
        self.assertEqual(account["email"], recorder.EMAIL)
        self.assertEqual(account["organization"], recorder.ORGANIZATION)
        # A copy inside another field is caught by value, not by key.
        self.assertEqual(frames[0]["params"]["authStatus"]["label"],
                         f"Claude Max · {recorder.EMAIL}")

    def test_paths_scrub_to_the_fixture_layout(self):
        method = {"command": NODE, "args": [NPX, "--cli"], "cwd": f"{WORKSPACE}/src",
                  "log": f"{HOME}/.claude/x"}
        frames, _ = self.scrubbed([{"jsonrpc": "2.0", "id": 0, "result": method}])
        result = frames[0]["result"]
        self.assertEqual(result["command"], recorder.NODE)
        self.assertEqual(result["args"], ["/npx/claude-agent-acp", "--cli"])
        self.assertEqual(result["cwd"], f"{recorder.WORKSPACE}/src")
        self.assertEqual(result["log"], f"{recorder.HOME}/.claude/x")

    def test_scoped_npx_packages_keep_their_name(self):
        path = f"{HOME}/.npm/_npx/ff/node_modules/@agentclientprotocol/codex-acp/dist/index.js"
        frames, _ = self.scrubbed([{"jsonrpc": "2.0", "id": 0, "result": {"p": path}}])
        self.assertEqual(frames[0]["result"]["p"], "/npx/codex-acp")

    def test_a_surviving_value_is_reported_by_kind_only(self):
        rendered = recorder.render([{"note": f"{HOME}/left"}])
        found = recorder.leaks(rendered, {"home": HOME, "absent": "/nowhere", "none": None})
        self.assertEqual(found, ["home"])

    def test_placeholders_are_not_secrets(self):
        _, hidden = self.scrubbed([auth(recorder.EMAIL, recorder.ORGANIZATION)])
        self.assertEqual(hidden, {})

    def test_a_short_account_value_scrubs_only_as_a_whole_word(self):
        frames, _ = self.scrubbed([
            {"jsonrpc": "2.0", "method": "x", "params": {"username": "aj"}},
            {"jsonrpc": "2.0", "method": "y", "params": {"text": "major aj ajar"}}])
        self.assertEqual(frames[0]["params"]["username"], "user")
        self.assertEqual(frames[1]["params"]["text"], "major user ajar")
        rendered = recorder.render(frames)
        self.assertEqual(recorder.leaks(rendered, {"v": recorder.whole_word("aj")}), [])

    def test_emails_outside_an_account_are_left_as_sent(self):
        frames, hidden = self.scrubbed([{"jsonrpc": "2.0", "id": 0, "result": {
            "stdout": "origin git@github.com:o/r.git icon@2x.png"}}])
        self.assertEqual(hidden, {})
        self.assertEqual(frames[0]["result"]["stdout"],
                         "origin git@github.com:o/r.git icon@2x.png")

    def test_managed_home_and_agent_state_scrub_apart(self):
        agents = Path("/var/folders/xy/T/ottam-fixture-abc/agents")
        home = agents / "omp/home"
        pairs = recorder.replacements(WORKSPACE, None, HOME, {}, scratch=home, agents=agents)
        frames = recorder.scrub([{"a": f"{home}/agent.db", "b": f"{agents}/omp/plugins/p"}],
                                pairs)
        self.assertEqual(frames, [{"a": f"{recorder.SCRATCH_HOME}/agent.db",
                                   "b": f"{recorder.SCRATCH_AGENTS}/omp/plugins/p"}])


class Framing(unittest.TestCase):
    def test_one_compact_frame_per_line(self):
        frames = [{"jsonrpc": "2.0", "id": 0, "result": {"a": "é"}}, update("plan")]
        rendered = recorder.render(frames)
        lines = rendered.splitlines()
        self.assertEqual(lines[0], "[")
        self.assertEqual(lines[-1], "]")
        self.assertEqual(lines[1], '{"jsonrpc":"2.0","id":0,"result":{"a":"é"}},')
        self.assertEqual(json.loads(rendered), frames)

    def test_mode_switch_and_private_updates_are_dropped(self):
        frames = [update("available_commands_update"), update("plan"),
                  {"jsonrpc": "2.0", "id": recorder.AUX_ID, "result": {}}]
        kept = recorder.kept(frames, recorder.PRIVATE_UPDATES)
        self.assertEqual(kept, [update("plan")])

    def test_an_agent_request_reusing_the_switch_id_is_kept(self):
        request = {"jsonrpc": "2.0", "id": recorder.AUX_ID,
                   "method": "session/request_permission", "params": {}}
        self.assertEqual(recorder.kept([request], ()), [request])

    def test_a_permission_without_options_is_cancelled(self):
        sent = []
        adapter = object.__new__(recorder.Adapter)
        adapter.send = sent.append
        adapter.answer({"id": 7, "method": "session/request_permission",
                        "params": {"options": []}})
        self.assertEqual(sent, [{"jsonrpc": "2.0", "id": 7,
                                 "result": {"outcome": {"outcome": "cancelled"}}}])

    def test_a_passed_deadline_stops_reading_queued_output(self):
        adapter = object.__new__(recorder.Adapter)
        adapter.inbox = recorder.queue.Queue()
        adapter.frames = []
        adapter.inbox.put(json.dumps(update("plan")))
        self.assertIsNone(adapter.next(recorder.time.monotonic() - 1))
        self.assertEqual(adapter.next(recorder.time.monotonic() + 5), update("plan"))

    def test_mode_switch_targets_the_mode_select(self):
        session = {"sessionId": "s", "configOptions": [
            {"id": "model", "category": "model", "currentValue": "a", "options": []},
            {"id": "mode", "category": "mode", "currentValue": "auto",
             "options": [{"value": "default"}, {"value": "auto"}]}]}
        self.assertEqual(recorder.mode_switch(session, "default"),
                         {"sessionId": "s", "configId": "mode", "value": "default"})
        self.assertIsNone(recorder.mode_switch(session, "auto"))
        self.assertIsNone(recorder.mode_switch(session, "missing"))


class ManagedHome(unittest.TestCase):
    def test_seeds_core_s_home_with_only_the_pruned_database(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "cli"
            source.mkdir()
            database = sqlite3.connect(source / "opencode.db")
            database.execute("CREATE TABLE credential (value TEXT)")
            database.execute("CREATE TABLE account (value TEXT)")
            database.execute("INSERT INTO credential VALUES ('token')")
            database.execute("INSERT INTO account VALUES ('refresh')")
            database.commit()
            database.close()
            (source / "other.json").write_text("{}")
            home = Path(directory) / "agents/opencode/home"
            home.mkdir(parents=True)
            managed = {"home": str(home),
                       "environment": {"OPENCODE_DB": str(home / "opencode.db")}}
            env = recorder.seed_home(managed, {"root": str(source), "copy": ["opencode.db"],
                                               "prune": {"opencode.db": ["DELETE FROM account"]}})
            self.assertEqual(env, managed["environment"])
            copied = sqlite3.connect(home / "opencode.db")
            self.assertEqual(copied.execute("SELECT value FROM credential").fetchall(),
                             [("token",)])
            self.assertEqual(copied.execute("SELECT value FROM account").fetchall(), [])
            copied.close()
            self.assertFalse((home / "other.json").exists())
            source_db = sqlite3.connect(source / "opencode.db")
            self.assertEqual(source_db.execute("SELECT value FROM account").fetchall(),
                             [("refresh",)])
            source_db.close()


class Coverage(unittest.TestCase):
    def test_compare_names_what_the_new_recording_lost(self):
        old = [{"jsonrpc": "2.0", "id": 9, "method": "session/request_permission", "params": {}},
               update("tool_call"), {"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "cancelled"}}]
        new = [update("tool_call"), {"jsonrpc": "2.0", "id": 2, "result": {"stopReason": "end_turn"}}]
        self.assertEqual(recorder.compare(old, new), [
            "methods: session/request_permission",
            "requests: session/request_permission",
            "stops: cancelled",
        ])
        self.assertEqual(recorder.compare(new, new), [])


if __name__ == "__main__":
    unittest.main()
