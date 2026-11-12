#!/usr/bin/env python3
"""Measure an isolated, background chooser in an already-built macOS app bundle.

Uses cumulative proc_pid_rusage counters for magnitude and sample for attribution.
Each invocation creates a fresh home; never attach this harness to user processes.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import tempfile
import time


class Usage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_ubyte * 16)] + [
        (name, ctypes.c_uint64) for name in (
            "user_ticks", "system_ticks", "package_idle_wakeups", "interrupt_wakeups",
            "pageins", "wired", "resident", "footprint", "start", "exit",
            "child_user", "child_system", "child_idle", "child_interrupt",
            "child_pageins", "child_elapsed", "read_bytes", "written_bytes",
        )
    ]


def usage(pid):
    result = Usage()
    if ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True).proc_pid_rusage(
        pid, 2, ctypes.byref(result)
    ):
        raise OSError(ctypes.get_errno(), "proc_pid_rusage")
    return {name: getattr(result, name) for name, _ in result._fields_ if name != "uuid"}


def processes():
    rows = subprocess.check_output(["ps", "-axo", "pid=,ppid=,comm="], text=True)
    return [line.strip().split(None, 2) for line in rows.splitlines() if line.strip()]


def front():
    return subprocess.check_output(["lsappinfo", "front"], text=True).strip()


class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


def timebase():
    value = Timebase()
    if ctypes.CDLL("/usr/lib/libSystem.B.dylib").mach_timebase_info(ctypes.byref(value)):
        raise RuntimeError("mach_timebase_info failed")
    return {"numer": value.numer, "denom": value.denom}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=60)
    parser.add_argument("--sample-seconds", type=int, default=8)
    args = parser.parse_args()
    app, out = args.app.resolve(), args.output.resolve()
    if args.seconds <= 0 or args.sample_seconds <= 0 or args.sample_seconds > args.seconds:
        parser.error("measurement durations must be positive and sampling must fit the interval")
    executable = str(app / "Contents/MacOS/Ottam")
    if any(command == executable for _, _, command in processes()):
        parser.error("this app bundle is already running; use a separate staged bundle")
    out.mkdir(parents=True, exist_ok=False)
    # Unix-domain sockets have a short path limit on macOS. Evidence paths can
    # exceed it, silently changing the core workload if the attach socket fails.
    home = Path(tempfile.mkdtemp(prefix="cdidle-", dir="/tmp"))
    pids, samples, handles = {}, [], []
    result = {"app": str(app), "scenario": "fresh background workspace chooser", "phase": "after 15s settling"}
    result["mach_timebase"] = timebase()
    try:
        subprocess.run(["open", "-n", "-g", "-j", str(app), "--env", f"OTTAM_HOME={home}"], check=True)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            rows = processes()
            for pid, _, command in rows:
                if command == str(app / "Contents/MacOS/Ottam"):
                    pids["app"] = int(pid)
            for pid, parent, command in rows:
                if int(parent) == pids.get("app") and command.endswith("/ottam-core"):
                    pids["core"] = int(pid)
            if len(pids) == 2:
                break
            time.sleep(0.2)
        if len(pids) != 2:
            raise RuntimeError(f"Could not locate isolated app/core: {pids}")
        time.sleep(15)
        if not (home / "core.sock").is_socket():
            raise RuntimeError("The isolated core did not create its attach socket")
        result["measured_app_asn"] = subprocess.check_output(
            ["lsappinfo", "find", f"pid={pids['app']}"], text=True
        ).strip()
        if not result["measured_app_asn"]:
            raise RuntimeError("Could not establish app focus identity")
        result["thermal_before"] = subprocess.check_output(["pmset", "-g", "therm"], text=True, stderr=subprocess.STDOUT)
        result["pids"] = pids
        result["before"] = {name: usage(pid) for name, pid in pids.items()}
        started = time.monotonic()
        for name, pid in pids.items():
            log = (out / f"{name}.capture.log").open("w")
            handles.append(log)
            samples.append(subprocess.Popen(["sample", str(pid), str(args.sample_seconds), "1", "-file", str(out / f"{name}.sample.txt")], stdout=log, stderr=subprocess.STDOUT))
        focus = [front()]
        while time.monotonic() - started < args.seconds:
            time.sleep(min(5, max(0, args.seconds - (time.monotonic() - started))))
            focus.append(front())
        result["after"] = {name: usage(pid) for name, pid in pids.items()}
        result["wall_seconds"] = time.monotonic() - started
        result["frontmost_snapshots"] = focus
        result["sample_exit_codes"] = [sample.wait(timeout=60) for sample in samples]
        result["thermal_after"] = subprocess.check_output(["pmset", "-g", "therm"], text=True, stderr=subprocess.STDOUT)
        result["metrics"] = {}
        for name in pids:
            before, after = result["before"][name], result["after"][name]
            ticks = (after["user_ticks"] + after["system_ticks"]) - (before["user_ticks"] + before["system_ticks"])
            cpu = ticks * result["mach_timebase"]["numer"] / result["mach_timebase"]["denom"] / 1e9
            result["metrics"][name] = {"cpu_seconds": cpu, "cpu_percent": cpu / result["wall_seconds"] * 100,
                "package_idle_wakeups": after["package_idle_wakeups"] - before["package_idle_wakeups"],
                "interrupt_wakeups": after["interrupt_wakeups"] - before["interrupt_wakeups"]}
        # The controlled regime is the measured app remaining backgrounded;
        # switching between other apps does not change that regime.
        result["sample_seconds"] = args.sample_seconds
        result["valid"] = (
            all(code == 0 for code in result["sample_exit_codes"])
            and all(asn and asn != result["measured_app_asn"] for asn in focus)
            and result["thermal_before"] == result["thermal_after"]
        )
    finally:
        for sample in samples:
            if sample.poll() is None:
                sample.terminate()
        for handle in handles:
            handle.close()
        for pid in pids.values():
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            alive = {int(row[0]) for row in processes()} & set(pids.values())
            if not alive:
                break
            time.sleep(0.1)
        result["cleanup_complete"] = not ({int(row[0]) for row in processes()} & set(pids.values()))
        if (home / "logs").is_dir():
            shutil.copytree(home / "logs", out / "logs", dirs_exist_ok=True)
        if result["cleanup_complete"]:
            shutil.rmtree(home)
        else:
            result["retained_home"] = str(home)
        result["valid"] = result.get("valid", False) and result["cleanup_complete"]
        (out / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result.get("metrics", {})), flush=True)


if __name__ == "__main__":
    main()
