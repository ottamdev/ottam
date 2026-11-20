#!/usr/bin/env python3
"""Record a pinned adapter's ACP session as a fixture in
backend/crates/ottam-core/src/agent/fixtures/.

Every pin bump re-records the fixtures that `agent/acp/tests.rs` and
`agent/metadata.rs` replay, one agent at a time:

    scripts/record-acp-fixture.py <agent> --compare <old fixture>

The script reads Ottam's real `initialize` request and the agent's pinned
launch from core (an ignored test writes them), plays the agent's scenario
below in a scratch workspace, and writes only the adapter's frames, one per
line. Each recording spends a few small turns on a signed-in account.

- Claude and Codex run on the user's own CLI home. Their recordings drop
  `available_commands_update`, which lists that home's skills and plugins.
- OpenCode and Oh-My-Pi run in a managed profile that core itself lays out
  in scratch (home, settings, plugin, and launch environment), holding only
  a copy of the CLI's credential database. A newer pin may migrate a
  database in place, so the user's own is only ever read. Binary agents run
  the pinned download, verified against the registry's digest.

Scrubbing replaces the workspace with the scenario's fixture path, the node
binary with /usr/local/bin/node, npx cache paths with /npx/<package>, $HOME
with /Users/user, the managed home with /ottam/home (other scratch agent
state with /ottam/agents), and account values with placeholders: the
fields named in SECRET_KEYS, and any copy of their values elsewhere as a
whole word. A leak check then refuses to write if any real value survives,
and email-shaped tokens left in the text are listed for review.
`--compare` prints what the old fixture covered that the new one does not;
name any gap in the pull request.

For Claude the script waits for the CLI's refresh lock to clear before
closing the adapter, and it never creates, removes, or force-kills anything
in a CLI's home.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import queue
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
FIXTURES = REPO / "backend/crates/ottam-core/src/agent/fixtures"

WORKSPACE = "/work/repo"
NODE = "/usr/local/bin/node"
HOME = "/Users/user"
SCRATCH_HOME = "/ottam/home"
SCRATCH_AGENTS = "/ottam/agents"
EMAIL = "user@example.com"
ORGANIZATION = "Example Organization"

REPO_FILES = {"README.md": "# Fixture\n", "notes.txt": "notes\n", "src/.keep": ""}
LISTING = "Run `ls -1 && echo hello-from-shell` in the shell."
APPEND = "to append a line `edited` to notes.txt"
STORY = "Write a 600-word short story about a lighthouse keeper."
# On the user's own CLI home these list its skills, plugins, and commands.
PRIVATE_UPDATES = ("available_commands_update",)

# Each scenario feeds what the Rust tests assert. Claude and Codex: the
# listing bytes and an edit that arrives as a diff (`payload_tests.rs`), and
# Claude's edit behind a permission prompt. OpenCode: two background
# subagents whose ids `child_sessions.rs` names, a follow-up that resumes one,
# a question form, and a turn cancelled while a subagent runs. Oh-My-Pi:
# permission prompts, an async shell job, and a cancelled turn.
SCENARIOS = {
    "claude": {
        "fixture": "claude-agent-acp-{version}.json",
        "files": {
            **REPO_FILES,
            # Ask beats a user's allow rules, so the edit always prompts.
            ".claude/settings.json": '{"permissions": {"ask": ["Edit"]}}\n',
        },
        "drop_updates": PRIVATE_UPDATES,
        "sessions": [{
            # Auto mode would skip the prompt; Default keeps its shape.
            "mode": "default",
            "prompts": [f"{LISTING} Then use the Edit tool (not the shell) {APPEND}. "
                        "Keep your reply to one sentence."],
        }],
    },
    "codex": {
        "fixture": "codex-acp-{version}.json",
        "files": REPO_FILES,
        "drop_updates": PRIVATE_UPDATES,
        "sessions": [{
            "prompts": [f"{LISTING} Then use apply_patch (not the shell) {APPEND}. "
                        "Keep your reply to one sentence."],
        }],
    },
    "omp": {
        "fixture": "omp-{version}.json",
        "files": REPO_FILES,
        "home": {
            "root": "~/.omp/agent",
            # config.yml picks the default model; it holds no credentials.
            "copy": ["agent.db", "config.yml"],
            # A refreshed OAuth copy would rotate the user's real token.
            "prune": {"agent.db": [
                "DELETE FROM auth_credentials WHERE credential_type != 'api_key'",
                "DELETE FROM auth_credential_refresh_leases",
                "DELETE FROM auth_credential_blocks",
            ]},
        },
        "sessions": [{
            "prompts": [
                f"{LISTING} Then use your edit tool (not the shell) {APPEND}, and check "
                "the bytes with `od -c notes.txt`. Keep your reply short.",
                "Run `sleep 4 && echo done-in-background` as a background job and do "
                "not wait for it. Reply in one sentence.",
                {"text": STORY, "cancel_after": 8},
            ],
        }],
    },
    "opencode": {
        "fixture": "opencode-{version}.json",
        "workspace": "/fixture/work",
        "files": {"README.md": "# Fixture\n", "a.txt": "alpha\n", "b.txt": "beta\n"},
        "git": True,
        "home": {
            "root": "~/.local/share/opencode",
            "copy": ["opencode.db"],
            # Console sign-ins carry refresh tokens; provider keys stay.
            "prune": {"opencode.db": ["DELETE FROM account", "DELETE FROM control_account",
                                      "DELETE FROM account_state"]},
        },
        "sessions": [
            # Subagents run in the background: each child outlives the prompt
            # that started it and reports later, so the recording waits.
            {"prompts": [
                {"text": "List the files here. Then, in one message, start two subagents "
                         "in parallel: one described exactly `Read a.txt` that reports the "
                         "exact contents of a.txt, and one described exactly `Read b.txt` "
                         "for b.txt. Do not modify any files. Once both have reported, "
                         "summarize in two sentences.",
                 "drain": 90.0},
                {"text": "Send the `Read b.txt` subagent a follow-up (call subagent again "
                         "with its sessionID) asking it to count the characters in b.txt. "
                         "Once it reports, tell me the count.",
                 "drain": 60.0},
            ]},
            {"prompts": ["Use your question tool to ask me whether to read a.txt or "
                         "b.txt next. If I don't answer, stop without reading either."]},
            {"prompts": [{"text": "Start a subagent described exactly `Story` that writes a "
                                  "600-word short story about a lighthouse keeper. While it "
                                  "runs, write your own 600-word short story about a "
                                  "lighthouse keeper.",
                          "cancel_after": 15, "drain": 10.0}]},
        ],
    },
}

CLAUDE_REFRESH_LOCK = ".oauth_refresh.lock"
AUX_ID = 1000
# Seconds a stalled socket read may block the pinned binary's download.
DOWNLOAD_TIMEOUT = 60
# Account fields and the placeholder each one's value becomes.
SECRET_KEYS = {"email": EMAIL, "organization": ORGANIZATION, "organizationName": ORGANIZATION,
               "orgName": ORGANIZATION, "userName": "User", "username": "user"}
EMAIL_PATTERN = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
NPX_PATTERN = re.compile(
    r"[^\s\"']*/_npx/[0-9a-f]+/node_modules/(?:\.bin/|@[^/\s\"']+/)?([^/\s\"']+)[^\s\"']*")


def fail(message: str) -> "NoReturn":  # noqa: F821
    print(f"record-acp-fixture.py: {message}", file=sys.stderr)
    sys.exit(2)


def recording_inputs(home_root: Path) -> dict:
    """Ottam's `initialize`, pinned launches, and each managed profile, which
    core's ignored test lays out under `home_root`."""
    with tempfile.TemporaryDirectory(prefix="ottam-fixture-inputs-") as directory:
        out = Path(directory) / "inputs.json"
        command = ["cargo", "test", "--quiet", "--manifest-path", str(REPO / "backend/Cargo.toml"),
                   "-p", "ottam-core", "--lib", "write_fixture_recording_inputs", "--",
                   "--ignored"]
        env = {**os.environ, "OTTAM_FIXTURE_INPUTS_OUT": str(out),
               "OTTAM_FIXTURE_HOME_ROOT": str(home_root)}
        result = subprocess.run(command, env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        if result.returncode != 0 or not out.exists():
            fail(f"core did not write the recording inputs:\n{result.stdout[-2000:]}")
        return json.loads(out.read_text())


def node_binary() -> str | None:
    """Node's real executable; version-manager shims hide it from `which`."""
    if not shutil.which("node"):
        return None
    result = subprocess.run(["node", "-p", "process.execPath"], stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)
    return result.stdout.strip() or None


def fetch_binary(binary: dict, into: Path) -> Path:
    """The pinned executable, refused unless its digest matches the registry."""
    download = into / "download"
    digest = hashlib.sha256()
    with urllib.request.urlopen(binary["url"], timeout=DOWNLOAD_TIMEOUT) as response, \
            open(download, "wb") as out:
        for chunk in iter(lambda: response.read(1 << 20), b""):
            digest.update(chunk)
            out.write(chunk)
    if digest.hexdigest() != binary["sha256"]:
        fail(f"{binary['url']} does not match the pinned sha256")
    executable = into / "bin" / binary["command"]
    executable.parent.mkdir()
    if binary["member"]:
        with tarfile.open(download) as archive:
            member = archive.extractfile(binary["member"])
            if member is None:
                fail(f"{binary['member']} is missing from {binary['url']}")
            executable.write_bytes(member.read())
    else:
        shutil.copyfile(download, executable)
    executable.chmod(0o755)
    return executable.parent


def copy_database(source: Path, target: Path):
    """A consistent copy through SQLite's backup, opening the source read-only."""
    if not source.exists():
        fail(f"{source} is missing; sign the CLI in first")
    reader = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
    writer = sqlite3.connect(target)
    try:
        reader.backup(writer)
    finally:
        writer.close()
        reader.close()


def seed_home(managed: dict, home: dict) -> dict:
    """Copy the CLI's credentials into the managed home core laid out, and
    return core's launch environment for it."""
    root = Path(managed["home"])
    source = Path(os.path.expanduser(home["root"]))
    for name in home["copy"]:
        if name.endswith(".db"):
            copy_database(source / name, root / name)
        else:
            shutil.copyfile(source / name, root / name)
    for name, statements in home.get("prune", {}).items():
        database = sqlite3.connect(root / name)
        try:
            for statement in statements:
                database.execute(statement)
            database.commit()
        finally:
            database.close()
    return dict(managed["environment"])


def secrets(frames: list) -> dict[str, str]:
    """Account values worth hiding wherever an adapter copies them, each with
    its placeholder: the SECRET_KEYS fields, and emails inside an object that
    carries one (an account), never emails anywhere else."""
    found: dict[str, str] = {}

    def emails(value):
        if isinstance(value, dict):
            for inner in value.values():
                emails(inner)
        elif isinstance(value, list):
            for inner in value:
                emails(inner)
        elif isinstance(value, str):
            for email in EMAIL_PATTERN.findall(value):
                found.setdefault(email, EMAIL)

    def walk(value):
        if isinstance(value, dict):
            if any(key in SECRET_KEYS for key in value):
                emails(value)
            for key, inner in value.items():
                if key in SECRET_KEYS and isinstance(inner, str) and inner.strip():
                    found.setdefault(inner, SECRET_KEYS[key])
                walk(inner)
        elif isinstance(value, list):
            for inner in value:
                walk(inner)

    walk(frames)
    for placeholder in set(SECRET_KEYS.values()):
        found.pop(placeholder, None)
    return found


def whole_word(value: str) -> re.Pattern:
    """`value` only where it stands alone, so a short account value such as a
    username never rewrites the words it happens to sit inside."""
    return re.compile(rf"(?<!\w){re.escape(value)}(?!\w)")


def replacements(workspace: Path, node: str | None, home: str, hidden: dict[str, str],
                 workspace_as: str = WORKSPACE, scratch: Path | None = None,
                 agents: Path | None = None) -> list:
    """(pattern, replacement) pairs, longest source first. Paths match
    anywhere; account values match as whole words."""
    pairs = []
    for path in {str(workspace), os.path.realpath(workspace)}:
        pairs.append((path, workspace_as, False))
    for directory, placeholder in ((scratch, SCRATCH_HOME), (agents, SCRATCH_AGENTS)):
        if directory is not None:
            for path in {str(directory), os.path.realpath(directory)}:
                pairs.append((path, placeholder, False))
    if node:
        for path in {node, os.path.realpath(node)}:
            pairs.append((path, NODE, False))
    pairs.extend((value, placeholder, True) for value, placeholder in hidden.items())
    pairs.append((home, HOME, False))
    # Longest first, so a workspace inside $HOME scrubs as the workspace.
    pairs.sort(key=lambda pair: len(pair[0]), reverse=True)
    return [(whole_word(old) if bounded else re.compile(re.escape(old)), new)
            for old, new, bounded in pairs]


def scrub(frames: list, pairs: list) -> list:
    def text(value: str) -> str:
        value = NPX_PATTERN.sub(r"/npx/\1", value)
        for pattern, new in pairs:
            value = pattern.sub(lambda _: new, value)
        return value

    def walk(value):
        if isinstance(value, dict):
            return {key: walk(inner) for key, inner in value.items()}
        if isinstance(value, list):
            return [walk(inner) for inner in value]
        if isinstance(value, str):
            return text(value)
        return value

    return walk(frames)


def leaks(rendered: str, needles: dict) -> list[str]:
    """The kinds of real value still present; names only, never the values.
    A needle is a substring, or a pattern such as `whole_word`'s."""
    def present(needle) -> bool:
        if isinstance(needle, re.Pattern):
            return needle.search(rendered) is not None
        return bool(needle) and needle in rendered

    return sorted(kind for kind, needle in needles.items() if present(needle))


def render(frames: list) -> str:
    lines = [json.dumps(frame, separators=(",", ":"), ensure_ascii=False) for frame in frames]
    return "[\n" + ",\n".join(lines) + "\n]\n"


def coverage(frames: list) -> dict[str, set[str]]:
    """What a fixture exercises, to tell whether a re-recording lost any."""
    seen: dict[str, set[str]] = {key: set() for key in
                                 ("methods", "updates", "requests", "stops", "results")}
    for frame in frames:
        method = frame.get("method")
        if method:
            seen["methods"].add(method)
            if "id" in frame:
                seen["requests"].add(method)
            update = (frame.get("params") or {}).get("update")
            if isinstance(update, dict) and update.get("sessionUpdate"):
                seen["updates"].add(update["sessionUpdate"])
        result = frame.get("result")
        if isinstance(result, dict):
            if result.get("stopReason"):
                seen["stops"].add(result["stopReason"])
            if "sessionId" in result:
                seen["results"].add("session/new")
    return seen


def compare(old: list, new: list) -> list[str]:
    before, after = coverage(old), coverage(new)
    return [f"{kind}: {item}" for kind in before for item in sorted(before[kind] - after[kind])]


def kept(frames: list, drop_updates) -> list:
    """The frames a fixture keeps: not the answer to Ottam's own mode switch
    (an agent's own request may reuse its id), and not the updates that
    describe the user's CLI home rather than the adapter."""
    def private(frame):
        update = (frame.get("params") or {}).get("update")
        return isinstance(update, dict) and update.get("sessionUpdate") in drop_updates

    def switch_answer(frame):
        return frame.get("id") == AUX_ID and "method" not in frame

    return [frame for frame in frames if not switch_answer(frame) and not private(frame)]


def mode_switch(session: dict, mode: str | None) -> dict | None:
    for option in session.get("configOptions") or []:
        if option.get("category") == "mode" and option.get("currentValue") != mode:
            values = [choice.get("value") for choice in option.get("options") or []]
            if mode in values:
                return {"sessionId": session["sessionId"], "configId": option["id"], "value": mode}
    return None


class Adapter:
    def __init__(self, command: list[str], env: dict, cwd: Path, stderr):
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=stderr, text=True,
                                        bufsize=1, start_new_session=True)
        self.inbox: queue.Queue = queue.Queue()
        self.frames: list = []
        self.next_id = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.process.stdout:
            line = line.strip()
            if line:
                self.inbox.put(line)
        self.inbox.put(None)

    def send(self, message: dict):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, method: str, params: dict, timeout: float, request_id: int | None = None,
                cancel: tuple[float, str] | None = None) -> dict:
        """Send a request and wait for its answer. `cancel` is (seconds,
        session id): `session/cancel` goes out once that long has passed."""
        if request_id is None:
            request_id = self.next_id
            self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        started = time.monotonic()
        deadline = started + timeout
        cancel_at = started + cancel[0] if cancel else None
        while True:
            wait_until = min(deadline, cancel_at) if cancel_at else deadline
            frame = self.next(wait_until)
            if frame is None:
                if cancel_at and time.monotonic() >= cancel_at:
                    self.send({"jsonrpc": "2.0", "method": "session/cancel",
                               "params": {"sessionId": cancel[1]}})
                    cancel_at = None
                    continue
                fail(f"no answer to {method} before the timeout or adapter exit")
            if frame.get("id") == request_id and "method" not in frame:
                if "error" in frame:
                    detail = EMAIL_PATTERN.sub(EMAIL, json.dumps(frame["error"])[:1000])
                    fail(f"{method} failed: {detail}")
                return frame["result"]

    def next(self, deadline: float):
        """The next frame, or None once the adapter exits or `deadline`
        passes, even while output is still queued."""
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            try:
                line = self.inbox.get(timeout=remaining)
            except queue.Empty:
                return None
            if line is None:
                return None
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                continue
            self.frames.append(frame)
            if "method" in frame and "id" in frame:
                self.answer(frame)
            return frame

    def answer(self, frame: dict):
        method, params = frame["method"], frame.get("params") or {}
        if method == "session/request_permission":
            options = params.get("options") or []
            chosen = next((o for o in options if o.get("kind") == "allow_once"),
                          options[0] if options else None)
            outcome = ({"outcome": "selected", "optionId": chosen["optionId"]} if chosen
                       else {"outcome": "cancelled"})
            reply = {"result": {"outcome": outcome}}
        elif method == "elicitation/create":
            reply = {"result": {"action": "decline"}}
        else:
            reply = {"error": {"code": -32601, "message": f"Method not found: {method}"}}
        self.send({"jsonrpc": "2.0", "id": frame["id"], **reply})

    def drain(self, seconds: float):
        deadline = time.monotonic() + seconds
        while self.next(deadline) is not None:
            pass

    def close(self, refresh_lock: Path | None):
        if refresh_lock is not None:
            waited = time.monotonic() + 120
            while refresh_lock.exists() and time.monotonic() < waited:
                time.sleep(1)
        self.process.stdin.close()
        try:
            self.process.wait(timeout=15)
            return
        except subprocess.TimeoutExpired:
            pass
        os.killpg(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            print("record-acp-fixture.py: adapter still running after SIGTERM; "
                  f"left as pid {self.process.pid}", file=sys.stderr)


def play(adapter: Adapter, scenario: dict, initialize: dict, workspace: Path, timeout: float):
    adapter.request("initialize", initialize, timeout)
    for plan in scenario["sessions"]:
        session = adapter.request("session/new", {"cwd": str(workspace), "mcpServers": []},
                                  timeout)
        switch = mode_switch(session, plan.get("mode"))
        if switch:
            adapter.request("session/set_config_option", switch, timeout, request_id=AUX_ID)
        for prompt in plan["prompts"]:
            if isinstance(prompt, str):
                prompt = {"text": prompt}
            cancel = (prompt["cancel_after"], session["sessionId"]) \
                if "cancel_after" in prompt else None
            adapter.request("session/prompt", {
                "sessionId": session["sessionId"],
                "prompt": [{"type": "text", "text": prompt["text"]}],
            }, timeout, cancel=cancel)
            # Background work reports after the prompt answers.
            adapter.drain(prompt.get("drain", 0.0))
    adapter.drain(1.0)


def record(agent: str, scenario: dict, spec: dict, initialize: dict, timeout: float,
           scratch: Path, stderr_path: Path | None = None) -> tuple:
    workspace = scratch / "workspace"
    for name, content in scenario["files"].items():
        path = workspace / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    if scenario.get("git"):
        subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    env = {**os.environ, **spec["environment"]}
    if spec.get("binary"):
        env["PATH"] = f"{fetch_binary(spec['binary'], scratch)}{os.pathsep}{env['PATH']}"
    home = None
    if scenario.get("home"):
        if not spec.get("managed"):
            fail(f"core laid out no managed profile for {agent}")
        home = Path(spec["managed"]["home"])
        env.update(seed_home(spec["managed"], scenario["home"]))
    refresh_lock = None
    if agent == "claude":
        claude_home = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
        refresh_lock = claude_home / CLAUDE_REFRESH_LOCK
    stderr = open(stderr_path, "wb") if stderr_path else tempfile.TemporaryFile()
    adapter = Adapter([spec["command"], *spec["arguments"]], env, workspace, stderr)
    try:
        play(adapter, scenario, initialize, workspace, timeout)
    finally:
        adapter.close(refresh_lock)
        stderr.close()
    return kept(adapter.frames, scenario.get("drop_updates", ())), workspace, home


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("agent", choices=sorted(SCENARIOS))
    parser.add_argument("--compare", type=Path, help="old fixture whose coverage must survive")
    parser.add_argument("--out", type=Path, help="defaults to the fixture named for the pin")
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--stderr", type=Path,
                        help="keep the adapter's unscrubbed stderr here, for diagnosis only")
    options = parser.parse_args()

    scenario = SCENARIOS[options.agent]
    scratch = Path(tempfile.mkdtemp(prefix="ottam-fixture-"))
    try:
        agents = scratch / "agents"
        inputs = recording_inputs(agents)
        spec = inputs["agents"][options.agent]
        frames, workspace, home = record(options.agent, scenario, spec, inputs["initialize"],
                                         options.timeout, scratch, options.stderr)
        hidden = secrets(frames)
        node = node_binary()
        user_home = str(Path.home())
        pairs = replacements(workspace, node, user_home, hidden,
                             scenario.get("workspace", WORKSPACE), home, agents)
        scrubbed = scrub(frames, pairs)
        rendered = render(scrubbed)
        needles = {"home": user_home, "scratch": str(scratch),
                   "scratch realpath": os.path.realpath(scratch),
                   "node": node and os.path.realpath(node), "temp": "/var/folders/"}
        needles.update({f"account value {i}": whole_word(value)
                        for i, value in enumerate(sorted(hidden))})
        found = leaks(rendered, needles)
        if found:
            fail(f"refusing to write: still contains {', '.join(found)}")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    # Scrubbed home paths still describe the user's setup, and an email
    # outside an account object is left as sent; review each one.
    for number, line in enumerate(rendered.splitlines(), start=1):
        if f"{HOME}/" in line:
            print(f"  review line {number}: mentions a path under the CLI home")
        if any(email != EMAIL for email in EMAIL_PATTERN.findall(line)):
            print(f"  review line {number}: an email-shaped token outside an account")

    # Read the baseline first: --compare may name the file about to be replaced.
    baseline = json.loads(options.compare.read_text()) if options.compare else None
    out = options.out or FIXTURES / scenario["fixture"].format(version=spec["version"])
    out.write_text(rendered)
    print(f"wrote {out.relative_to(REPO) if out.is_relative_to(REPO) else out} "
          f"({len(scrubbed)} frames)")
    if baseline is not None:
        lost = compare(baseline, scrubbed)
        for item in lost:
            print(f"  lost {item}")
        if not lost:
            print("  covers everything the old fixture did")
    return 0


if __name__ == "__main__":
    sys.exit(main())
