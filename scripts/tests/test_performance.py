"""Performance analyzer contracts on small synthetic runs."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PERFORMANCE = Path(__file__).resolve().parents[1] / "performance"
sys.path.insert(0, str(PERFORMANCE))

import trace_support  # noqa: E402


def analyze(script: str, *directories: Path) -> dict:
    result = subprocess.run([sys.executable, str(PERFORMANCE / script), *map(str, directories)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return json.loads(result.stdout)


def row(fields: dict) -> str:
    return "<row>" + "".join(f"<cell>{fields.get(index, '')}</cell>" for index in range(13)) + "</row>"


def event(time: int, name: str, message: str) -> str:
    return row({0: time, 6: name, 9: "dev.ottam.Ottam", 11: message})


def summary(run: str, variant: str, chunk_bytes: str = "64") -> dict:
    return {
        "run": run, "variant": variant, "pair": "pair-1", "focusMode": "active",
        "scenario": "transcript.measure-multi-turn", "traceTemplate": "Logging",
        "scenarioResult": "passed", "focusStable": True, "phaseValid": True, "archiveSucceeded": True,
        "cpuPercentOfPhase": 10 if variant == "baseline" else 8, "buildConfiguration": "profile",
        "fixtureKnobs": {"chunkBytes": chunk_bytes}, "fixtureSourceSHA256": "fixture-digest",
        "machineIdentitySHA256": "machine-digest", "phaseDefinition": "accepted-after-focus-stable",
        "phaseWorkload": "complete-frozen-corpus",
    }


class Performance(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ottam-performance-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def write_summary(self, directory: Path, value: dict) -> None:
        directory.mkdir(exist_ok=True)
        (directory / "summary.json").write_text(json.dumps(value))

    def test_summarize_runs_pairs_only_comparable_runs(self):
        baseline, feature = self.root / "baseline", self.root / "feature"
        self.write_summary(baseline, summary("baseline", "baseline"))
        self.write_summary(feature, summary("feature", "feature"))
        pair = next(iter(analyze("summarize-runs.py", baseline, feature)["pairs"].values()))
        self.assertIs(pair["complete"], True)
        self.assertEqual(pair["featureDeltaPercent"], -20.0)

        self.write_summary(feature, summary("feature", "feature", chunk_bytes="256"))
        pair = next(iter(analyze("summarize-runs.py", baseline, feature)["pairs"].values()))
        self.assertIs(pair["complete"], False)
        self.assertIn("mismatched_fixtureKnobs", pair["mismatchReasons"])
        self.assertNotIn("featureDeltaPercent", pair)

        mismatched = summary("feature", "feature")
        mismatched["phaseWorkload"] = "fixed-duration-detached-stream"
        self.write_summary(feature, mismatched)
        pair = next(iter(analyze("summarize-runs.py", baseline, feature)["pairs"].values()))
        self.assertIn("mismatched_phaseWorkload", pair["mismatchReasons"])

    def test_signposts_count_presented_work_and_publications(self):
        self.write_summary(self.root, {"run": "test", "scenarioResult": "passed", "focusStable": True})
        intervals = "".join(
            row({0: 150, 1: 1_000_000, 3: "commit.present", 5: "dev.ottam.Ottam", 12: f"proxy= {proxy}"})
            for proxy in ("ca_transaction", "renderer_failed", "teardown")
        )
        (self.root / "signpost-intervals.xml").write_text(f"<data>{intervals}</data>")
        for name, value in (("normalized.publication", "succeeded= true"),
                            ("normalized.publication", "succeeded= 1"),
                            ("stream.admission", "disposition= accepted")):
            with self.subTest(name=name, value=value):
                events = event(100, name, value) + event(200, name, value)
                events += event(999, "normalized.publication", "succeeded= false")
                (self.root / "signpost-events.xml").write_text(f"<data>{events}</data>")
                result = analyze("analyze-transcript-signposts.py", self.root)["test"]
                self.assertEqual(result["intervals_ms"]["commit.present"]["count"], 1)
                self.assertEqual(result["counts"]["commit.present.proxy.renderer_failed"], 1)
                if name == "normalized.publication":
                    self.assertEqual(result["counts"]["normalized.publication.succeeded"], 2)

    def test_explicit_phase_takes_precedence_over_log_fallback(self):
        (self.root / "trace-toc.xml").write_text(
            "<trace><start-date>2026-09-07T12:00:00Z</start-date></trace>")
        (self.root / "signposts.log").write_text(
            "2026-09-07 12:00:01.000 normalized.publication succeeded=true\n"
            "2026-09-07 12:00:02.000 normalized.publication succeeded=true\n")
        self.assertEqual(len(trace_support.publication_times(str(self.root))), 2)
        phase = trace_support.phase_range_ns(str(self.root), {
            "phaseStartTimestamp": "2026-09-07T12:00:03Z", "phaseEndTimestamp": "2026-09-07T12:00:04Z"})
        self.assertEqual(phase, (3_000_000_000, 4_000_000_000))

    def write_phase(self, summary: dict) -> None:
        """A measured phase from 1 s to 3 s after the trace start."""
        self.write_summary(self.root, dict(summary, run="test", scenarioResult="passed", focusStable=True,
                                           phaseStartTimestamp="2026-09-07T12:00:01Z",
                                           phaseEndTimestamp="2026-09-07T12:00:03Z"))
        (self.root / "trace-toc.xml").write_text(
            "<trace><start-date>2026-09-07T12:00:00Z</start-date></trace>")

    def test_time_profile_follows_referenced_backtraces(self):
        # Without phaseAdmissions, the log's two publications are the admissions.
        self.write_phase({})
        (self.root / "signposts.log").write_text(
            "2026-09-07 12:00:01.500 normalized.publication succeeded=true\n"
            "2026-09-07 12:00:02.500 normalized.publication succeeded=true\n")
        samples = (
            '<row><sample-time id="1">1500000000</sample-time><thread id="2" fmt="Main Thread 0x1"/>'
            '<core id="3" fmt="CPU 2 (P Core)"/><weight id="4">1000000</weight>'
            '<tagged-backtrace id="5"><backtrace id="6">'
            '<frame id="7" name="StreamingCommitCoordinator.frameTick"/><frame id="8" name="main"/>'
            '</backtrace></tagged-backtrace></row>'
            # A repeated stack refers back to the first one.
            '<row><sample-time id="9">2500000000</sample-time><thread ref="2"/><core ref="3"/>'
            '<weight ref="4"/><tagged-backtrace ref="5"/></row>'
            '<row><sample-time id="10">9000000000</sample-time><thread ref="2"/><core ref="3"/>'
            '<weight ref="4"/><tagged-backtrace ref="5"/></row>'
        )
        (self.root / "time-profile.xml").write_text(f"<trace-query-result><node>{samples}</node></trace-query-result>")
        [result] = analyze("analyze-transcript-time-profile.py", self.root)
        self.assertEqual(result["row_count"], 2)
        self.assertEqual(result["tick_sample_count"], 2)
        self.assertEqual(result["sampled_cpu_percent"], 0.1)
        self.assertEqual(result["sampled_cpu_ms_per_admission"], 1.0)
        self.assertEqual(result["thread_cpu_percent"], {"main": 0.1})
        self.assertEqual(result["core_index_share_percent"], {"2": 100.0})
        self.assertEqual(result["top_self_cpu_percent"], {"StreamingCommitCoordinator.frameTick": 0.1})

    def test_thread_state_clips_main_thread_running_time_to_the_phase(self):
        self.write_phase({})
        states = (
            '<row><start-time id="1">500000000</start-time><duration id="2">1000000000</duration>'
            '<thread id="3" fmt="Main Thread 0x1"/><thread-state id="4" fmt="Running"/>'
            '<sched-priority id="5" fmt="47"/><core id="6" fmt="CPU 2 (P Core)"/></row>'
            '<row><start-time id="7">2000000000</start-time><duration ref="2"/><thread ref="3"/>'
            '<thread-state ref="4"/><sched-priority id="8" fmt="31"/><core id="9" fmt="CPU 4 (E Core)"/></row>'
            '<row><start-time id="10">1500000000</start-time><duration ref="2"/>'
            '<thread id="11" fmt="Worker 0x2"/><thread-state ref="4"/></row>'
            '<row><start-time id="12">1500000000</start-time><duration ref="2"/><thread ref="3"/>'
            '<thread-state id="13" fmt="Blocked"/></row>'
        )
        (self.root / "thread-state.xml").write_text(f"<trace-query-result><node>{states}</node></trace-query-result>")
        [result] = analyze("analyze-thread-state.py", self.root)
        self.assertEqual(result["range_seconds"], 2.0)
        self.assertEqual(result["main_running_seconds"], 1.5)
        self.assertEqual(result["priority_share_percent"], {"47": 33.333, "31": 66.667})
        self.assertEqual(result["core_share_percent"], {"P Core": 33.333, "E Core": 66.667})

    def test_timestamps_reject_out_of_range_fields(self):
        self.assertEqual(trace_support.epoch_iso8601("2026-09-07t12:00:00z"),
                         trace_support.epoch_iso8601("2026-09-07T12:00:00Z"))
        for text in ("2026-13-07T12:00:00Z", "2026-02-29T12:00:00Z", "2026-09-07T25:00:00Z"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                trace_support.epoch_iso8601(text)

    def test_report_numbers(self):
        self.assertEqual(trace_support.round_half_up(2.5), 3)
        self.assertEqual(trace_support.round_half_up(-2.5), -3)
        self.assertEqual(trace_support.round_half_up(0.00015, 4), 0.0002)
        self.assertEqual(trace_support.float_text(1e15), "1.0e+15")
        self.assertEqual(trace_support.float_text(1e-05), "1.0e-05")
        self.assertEqual(trace_support.float_text(1234567890123456.5), "1234567890123456.5")
        self.assertEqual(trace_support.float_text(-0.0), "-0.0")
        self.assertEqual(trace_support.leading_float(" 1_000.5ms"), 1000.5)
        self.assertEqual(trace_support.leading_int("12.7"), 12)
        self.assertEqual(trace_support.json_text({"a": [], "b": {}, "c": [1.0]}),
                         '{\n  "a": [],\n  "b": {},\n  "c": [\n    1.0\n  ]\n}')


if __name__ == "__main__":
    unittest.main()
