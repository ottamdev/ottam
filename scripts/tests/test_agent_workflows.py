"""Contract tests; fake toolchains keep routing and failure tests inexpensive."""
import os
import importlib.util
from pathlib import Path
import shutil
import sys
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
# Filtered runs build the fixture servers before testing; a fake cargo whose
# output is the test result lets those builds pass quietly.
BUILDS_PASS = '[[ "$1" == build ]] && exit 0\n'


class Workflows(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ottam-workflows-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for directory in ("scripts/lib", ".githooks", "backend", "bin"):
            (self.root / directory).mkdir(parents=True)
        for name in ("test-app.sh", "test-rust-core.sh", "prune-rust-target.sh",
                     "check-migrations.sh", "install-git-hooks.sh", "lib/xcode-developer-dir.sh"):
            shutil.copy2(ROOT / "scripts" / name, self.root / "scripts" / name)
        shutil.copy2(ROOT / ".githooks/pre-commit", self.root / ".githooks/pre-commit")
        self.env = dict(os.environ, PATH=str(self.root / "bin") + ":" + os.environ["PATH"])
        self.env.pop("OTTAM_TEST_VERBOSE", None)
        self.env["GIT_CONFIG_GLOBAL"] = os.devnull
        self.env["GIT_CONFIG_NOSYSTEM"] = "1"
        self.write("bin/cargo", '#!/bin/bash\nprintf "%s\\n" "$*" >> calls\nexit "${FAKE_STATUS:-0}"\n')
        self.write("bin/swift", '#!/bin/bash\nprintf "%s\\n" "$*" > swift-args\ncat "$FAKE_OUTPUT"\nexit "${FAKE_STATUS:-0}"\n')
        developer = self.root / "Xcode.app/Contents/Developer"
        (developer / "Platforms/MacOSX.platform/Developer/Library/Frameworks/Testing.framework").mkdir(parents=True)
        self.env["DEVELOPER_DIR"] = str(developer)
        self.env["FAKE_OUTPUT"] = str(self.root / "output")
        self.write("output", "")

    def write(self, name, text):
        path = self.root / name
        path.write_text(text)
        path.chmod(0o755)

    def run_cmd(self, *args, status=0, cwd=None):
        result = subprocess.run(args, cwd=cwd or self.root, env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.assertEqual(result.returncode, status, result.stdout)
        return result.stdout

    def test_rust_routing(self):
        self.write("bin/cargo", '#!/bin/bash\nprintf "%s\\n" "$*" >> calls\n' + BUILDS_PASS
                   + 'cat "$FAKE_OUTPUT"\nexit "${FAKE_STATUS:-0}"\n')
        self.write("output", "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n")
        self.run_cmd("scripts/test-rust-core.sh", "--filter", "two words")
        calls = self.root / "backend/calls"
        self.assertEqual(calls.read_text().splitlines(), [
            "build --quiet -p ottam-test-mcp",
            "test --quiet -p ottam-core --lib two words --"])
        calls.unlink()
        self.run_cmd("scripts/test-rust-core.sh", "--test", "jsonl_protocol", "--filter", "agent")
        self.assertEqual(calls.read_text().splitlines(), [
            "build --quiet -p ottam-test-mcp",
            "build --quiet -p ottam-test-acp",
            "test --quiet -p ottam-core --test jsonl_protocol agent -- --test-threads=4"])
        calls.unlink()
        self.env["OTTAM_TEST_VERBOSE"] = "1"
        self.env["FAKE_STATUS"] = "17"
        self.run_cmd("scripts/test-rust-core.sh", "--lib", status=17)
        self.assertNotIn("--quiet", calls.read_text())

    def test_ci_surface_selection(self):
        spec = importlib.util.spec_from_file_location("ci_changes", ROOT / "scripts/ci/changes.py")
        changes = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(changes)
        cases = [
            (["README.md"], dict(rust=False, swift=False, docs=True)),
            (["backend/README.md"], dict(rust=False, swift=False, docs=True)),
            (["scripts/check-docs.sh"], dict(rust=False, swift=False, docs=True)),
            (["scripts/lib/check_docs.py"], dict(rust=False, swift=False, docs=True)),
            (["backend/crates/ottam-core/src/lib.rs"], dict(rust=True, swift=True, docs=False)),
            (["app/Sources/OttamApp/Main.swift"], dict(rust=True, swift=True, docs=False)),
            (["scripts/check-protocol-alignment.sh"], dict(rust=True, swift=True, docs=False)),
            ([".github/workflows/full-tests.yml"], dict(rust=True, swift=True, docs=False)),
            (["rust-toolchain.toml"], dict(rust=True, swift=True, docs=False)),
            (["rust-toolchain"], dict(rust=True, swift=True, docs=False)),
            (["ui-testing/project.yml"], dict(rust=False, swift=False, docs=False)),
            (["ui-testing/README.md"], dict(rust=False, swift=False, docs=True)),
            ([], dict(rust=False, swift=False, docs=False)),
        ]
        for paths, expected in cases:
            self.assertEqual(changes.surfaces(paths), expected)

    def test_ci_branch_diff_includes_earlier_commits_and_deletions(self):
        self.env.pop("DEVELOPER_DIR", None)
        self.run_cmd("git", "init", "-q")
        self.run_cmd("git", "config", "user.name", "Contract Test")
        self.run_cmd("git", "config", "user.email", "test@example.invalid")
        self.run_cmd("git", "config", "commit.gpgsign", "false")
        self.write("README.md", "base\n")
        self.run_cmd("git", "add", "README.md")
        self.run_cmd("git", "commit", "-qm", "base")
        base = self.run_cmd("git", "rev-parse", "HEAD").strip()
        (self.root / "app").mkdir()
        self.write("app/Example.swift", "// changed\n")
        self.run_cmd("git", "add", "app/Example.swift")
        self.run_cmd("git", "commit", "-qm", "first change")
        self.run_cmd("git", "rm", "README.md")
        self.run_cmd("git", "commit", "-qm", "second change")
        result = self.run_cmd(sys.executable, str(ROOT / "scripts/ci/changes.py"), "--base", base)
        self.assertEqual(result, "rust=true\nswift=true\ndocs=true\n")
        self.run_cmd(sys.executable, str(ROOT / "scripts/ci/changes.py"), "--base", "missing-ref", status=1)

    def test_rust_readable_output(self):
        self.write("bin/cargo", '#!/bin/bash\n' + BUILDS_PASS
                   + 'cat "$FAKE_OUTPUT"\necho "warning: keep diagnostics" >&2\nexit "${FAKE_STATUS:-0}"\n')
        output = ("running 2 tests\n..\n"
                  "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.25s\n"
                  "running 0 tests\n"
                  "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n")
        self.write("output", output)
        result = self.run_cmd("scripts/test-rust-core.sh", "--lib")
        self.assertIn("PASS  ottam-core / library: 2 passed, 1 ignored, 3 filtered out (0.25s)", result)
        self.assertIn("warning: keep diagnostics", result)
        self.assertNotIn("running", result)
        self.assertEqual(result.count("PASS"), 1)
        self.env["FAKE_STATUS"] = "101"
        self.write("output", "injected_failure --- FAILED\nassertion failed: expected 2\n")
        result = self.run_cmd("scripts/test-rust-core.sh", "--lib", status=101)
        self.assertIn("FAIL  ottam-core / library", result)
        self.assertIn("assertion failed: expected 2", result)
        self.assertNotIn("PASS", result)
        last_log = (self.root / ".build/test-rust-core-last.log").read_text()
        self.assertIn("== ottam-core / library (failed)", last_log)
        self.assertIn("assertion failed: expected 2", last_log)
        self.env["OTTAM_TEST_VERBOSE"] = "1"
        self.env["FAKE_STATUS"] = "0"
        self.write("output", output)
        self.assertIn(output, self.run_cmd("scripts/test-rust-core.sh", "--lib"))

    def test_rust_filter_zero_match(self):
        self.write("bin/cargo", '#!/bin/bash\ncat "$FAKE_OUTPUT"\nexit 0\n')
        self.write("output", "running 0 tests\n"
                   "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s\n")
        result = self.run_cmd("scripts/test-rust-core.sh", "--filter", "typo", status=1)
        self.assertIn("FAIL  ottam-core / library [filter: typo]: 0 tests matched filter 'typo'", result)
        self.assertNotIn("PASS", result)
        self.assertIn("test-rust-core.sh: full log: .build/test-rust-core-last.log", result)
        self.assertIn("5 filtered out", (self.root / ".build/test-rust-core-last.log").read_text())
        self.assertIn("PASS  ottam-core / library: 0 passed", self.run_cmd("scripts/test-rust-core.sh", "--lib"))
        self.env["OTTAM_TEST_VERBOSE"] = "1"
        result = self.run_cmd("scripts/test-rust-core.sh", "--filter", "typo", status=1)
        self.assertIn("0 tests matched filter 'typo'", result)
        self.assertIn("5 filtered out", (self.root / ".build/test-rust-core-last.log").read_text())

    def test_rust_log_keeps_stderr(self):
        self.write("bin/cargo", '#!/bin/bash\n' + BUILDS_PASS + 'echo "error[E0425]: cannot find value" >&2\nexit 101\n')
        result = self.run_cmd("scripts/test-rust-core.sh", "--lib", status=101)
        self.assertIn("error[E0425]: cannot find value", result)
        last_log = (self.root / ".build/test-rust-core-last.log").read_text()
        self.assertIn("== ottam-core / library (failed)", last_log)
        self.assertIn("error[E0425]: cannot find value", last_log)

    def test_bad_rust_arguments(self):
        for args in (("--test",), ("--lib", "--test", "storage"), ("--unknown",),
                     ("--filter", "--lib"), ("--test", "-q"), ("--filter", "x", "--filter", "y")):
            self.run_cmd("scripts/test-rust-core.sh", *args, status=64)
        self.assertFalse((self.root / "backend/calls").exists())
        self.run_cmd("scripts/test-rust-core.sh", "--help")

    def test_full_rust_stages(self):
        for path in ROOT.glob("scripts/check-*.sh"):
            self.write("scripts/" + path.name, "#!/bin/bash\nexit 0\n")
        for package, target in (("ottam-core", "jsonl_protocol"), ("ottam-protocol", "artifacts")):
            directory = self.root / "backend/crates" / package / "tests"
            directory.mkdir(parents=True)
            (directory / (target + ".rs")).touch()
        self.run_cmd("scripts/test-rust-core.sh")
        calls = (self.root / "backend/calls").read_text()
        for expected in ("test --quiet --workspace --lib --bins", "--test artifacts",
                         "--test-threads=4", "test --quiet --workspace --doc",
                         "build --quiet -p ottam-core", "clippy --quiet --all-targets -- -D warnings"):
            self.assertIn(expected, calls)
        self.assertEqual(calls.count("--test jsonl_protocol"), 1)
        self.assertIn("OttamBackendClientTests", (self.root / "backend/swift-args").read_text())
        (self.root / "backend/swift-args").unlink()
        self.run_cmd("scripts/test-rust-core.sh", "--rust-only")
        self.assertFalse((self.root / "backend/swift-args").exists())
        self.run_cmd("scripts/test-rust-core.sh", "--rust-only", "--lib", status=64)


    def test_swift_success_and_verbose(self):
        output = ('◇ Test run started.\n◇ Suite "Example" started.\n'
                  '◇ Test case passing 1 argument value → \"line\nline\" started.\n'
                  '◇ Test example() started.\n✔ Test example() passed after 0.001 seconds.\n'
                  '✔ Suite "Example" passed after 0.001 seconds.\n'
                  '✔ Test run with 1 test passed after 0.001 seconds.\n')
        self.write("output", output)
        quiet = self.run_cmd("scripts/test-app.sh", "--filter", "Example")
        self.assertNotIn("example()", quiet)
        self.assertNotIn('Suite "Example"', quiet)
        self.assertIn("Test run with 1 test passed", quiet)
        log_notice = "test-app.sh: full log: .build/test-app-last.log\n"
        self.assertIn(log_notice, quiet)
        self.assertEqual((self.root / ".build/test-app-last.log").read_text(), output)
        verbose = self.run_cmd("scripts/test-app.sh", "--verbose", "--filter", "Example")
        self.assertEqual(verbose, output + log_notice)
        self.assertEqual((self.root / ".build/test-app-last.log").read_text(), output)
        self.assertNotIn("--verbose", (self.root / "swift-args").read_text())
        self.assertIn("--filter Example", (self.root / "swift-args").read_text())
        self.assertIn("-Xswiftc -warnings-as-errors", (self.root / "swift-args").read_text())

    def test_swift_failures(self):
        for output, code, expected in (
            ("◇ Test run started.\n✘ example: assertion failed\n✘ Test run with 1 test failed.\n", 1, 1),
            ("error: compile failed\n", 2, 2),
            ("◇ Test run started.\n", 0, 70),
        ):
            self.write("output", output)
            self.env["FAKE_STATUS"] = str(code)
            result = self.run_cmd("scripts/test-app.sh", status=expected)
            self.assertIn(output.strip(), result)
            self.assertEqual((self.root / ".build/test-app-last.log").read_text(), output)

    def test_swift_filter_zero_match(self):
        output = "Build complete! (0.01 secs.)\nwarning: No matching test cases were run\n"
        self.write("output", output)
        result = self.run_cmd("scripts/test-app.sh", "--filter", "typo", status=65)
        self.assertIn("test-app.sh: 0 tests matched the filter", result)
        self.assertEqual((self.root / ".build/test-app-last.log").read_text(), output)
        self.run_cmd("scripts/test-app.sh")

    def test_hooks(self):
        self.env.pop("DEVELOPER_DIR", None)
        self.run_cmd("git", "init", "-q")
        self.run_cmd("git", "config", "user.name", "Contract Test")
        self.run_cmd("git", "config", "user.email", "test@example.invalid")
        self.run_cmd("git", "config", "commit.gpgsign", "false")
        self.run_cmd("scripts/install-git-hooks.sh")
        self.run_cmd("scripts/install-git-hooks.sh")
        self.write("scripts/check-docs.sh", '#!/bin/bash\necho docs-checked\nexit "${DOCS_STATUS:-0}"\n')
        self.write("plain.txt", "clean\n")
        self.run_cmd("git", "add", "plain.txt")
        self.assertNotIn("docs-checked", self.run_cmd("git", "commit", "-qm", "plain"))
        self.write("space name.md", "clean\n")
        self.run_cmd("git", "add", "space name.md")
        self.env["DOCS_STATUS"] = "1"
        self.assertIn("docs-checked", self.run_cmd("git", "commit", "-qm", "docs", status=1))
        self.env["DOCS_STATUS"] = "0"
        self.run_cmd("git", "add", "scripts", ".githooks")
        self.run_cmd("git", "commit", "-qm", "hooks")
        linked = self.root / "linked"
        self.run_cmd("git", "worktree", "add", "-qb", "linked", str(linked))
        self.run_cmd("scripts/install-git-hooks.sh", cwd=linked)
        (linked / "bad.txt").write_text("trailing  \n")
        self.run_cmd("git", "add", "bad.txt", cwd=linked)
        self.run_cmd("git", "commit", "-qm", "bad", cwd=linked, status=1)
        absolute = str(self.root / ".githooks")
        self.run_cmd("git", "config", "core.hooksPath", absolute)
        self.run_cmd("scripts/install-git-hooks.sh")
        self.run_cmd("scripts/install-git-hooks.sh", cwd=linked)
        self.assertEqual(self.run_cmd("git", "config", "core.hooksPath").strip(), absolute)
        self.run_cmd("git", "config", "core.hooksPath", str(self.root / "backend"))
        self.run_cmd("scripts/install-git-hooks.sh", status=1)
        self.run_cmd("git", "config", "core.hooksPath", "custom")
        self.run_cmd("scripts/install-git-hooks.sh", status=1)
        self.assertEqual(self.run_cmd("git", "config", "core.hooksPath").strip(), "custom")

    def test_migrations_are_append_only(self):
        self.env.pop("DEVELOPER_DIR", None)
        self.run_cmd("git", "init", "-q")
        self.run_cmd("git", "config", "user.name", "Contract Test")
        self.run_cmd("git", "config", "user.email", "test@example.invalid")
        self.run_cmd("git", "config", "commit.gpgsign", "false")
        self.run_cmd("git", "commit", "-q", "--allow-empty", "-m", "base")
        self.run_cmd("scripts/install-git-hooks.sh")
        migrations = self.root / "backend/crates/ottam-core/migrations"
        first = migrations / "00000000000001_first"
        first.mkdir(parents=True)
        (first / "up.sql").write_text("CREATE TABLE a (id TEXT);\n")
        self.run_cmd("git", "add", "-A", str(migrations))
        self.run_cmd("git", "commit", "-qm", "add first")
        # Pre-commit refuses an edit to a committed migration.
        (first / "down.sql").write_text("DROP TABLE a;\n")
        self.run_cmd("git", "add", "-A", str(migrations))
        self.assertIn("append-only", self.run_cmd("git", "commit", "-qm", "edit", status=1))
        (first / "down.sql").unlink()
        (first / "up.sql").write_text("CREATE TABLE a (id TEXT, b TEXT);\n")
        self.run_cmd("git", "add", "-A", str(migrations))
        self.assertIn("M backend/crates/ottam-core/migrations/00000000000001_first/up.sql",
                      self.run_cmd("git", "commit", "-qm", "edit", status=1))
        self.env["OTTAM_ALLOW_MIGRATION_EDIT"] = "1"
        self.run_cmd("git", "commit", "-qm", "approved reset")
        self.env.pop("OTTAM_ALLOW_MIGRATION_EDIT")
        second = migrations / "00000000000002_second"
        second.mkdir()
        (second / "up.sql").write_text("CREATE TABLE b (id TEXT);\n")
        self.run_cmd("git", "add", "-A", str(migrations))
        self.run_cmd("git", "commit", "-qm", "add second")
        # CI compares the range with its base: a hook bypass that edits a
        # migration the base has is caught, while a migration the range adds
        # may change within it.
        self.assertIn("M backend/crates/ottam-core/migrations/00000000000001_first/up.sql",
                      self.run_cmd("scripts/check-migrations.sh", "HEAD~2", "HEAD", status=1))
        self.run_cmd("scripts/check-migrations.sh", "HEAD~3", "HEAD")
        self.run_cmd("scripts/check-migrations.sh", "HEAD~1", "HEAD")


if __name__ == "__main__":
    unittest.main()
