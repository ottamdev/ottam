#!/usr/bin/env python3
"""Split main-thread running time inside each run's measured phase by priority and core type."""
from __future__ import annotations

import os
import re
import sys
import xml.etree.ElementTree as ElementTree
from collections import Counter

import trace_support as support


def row_value(row, definitions, tag, attribute):
    return support.referenced_value(row.find(tag), definitions, attribute)


def rows(path):
    """Yield each <row> once it is complete, then drop it, so large traces stream."""
    parents = []
    for event, node in ElementTree.iterparse(path, events=("start", "end")):
        if event == "start":
            parents.append(node)
            continue
        parents.pop()
        if node.tag == "row":
            yield node
            node.clear()
            if parents:
                parents[-1].remove(node)


def analyze(directory):
    summary = support.load_summary(directory)
    path = os.path.join(directory, "thread-state.xml")
    if not os.path.isfile(path):
        support.fail(f"error: missing {path}")
    phase_start, phase_end = support.phase_range_ns(directory, summary)
    definitions = {}
    priorities = Counter()
    cores = Counter()
    running_ns = 0

    for row in rows(path):
        # Definitions accumulate in trace order; a row can only refer back.
        for node in row.iter():
            if node is not row and node.get("id") is not None:
                definitions[node.get("id")] = {
                    "fmt": node.get("fmt"),
                    "value": support.strip_text(support.element_text(node)),
                }

        if not support.text_of(row_value(row, definitions, "thread", "fmt")).startswith("Main Thread"):
            continue
        if support.text_of(row_value(row, definitions, "thread-state", "fmt")) != "Running":
            continue
        start_ns = support.leading_int(row_value(row, definitions, "start-time", "value"))
        duration_ns = support.leading_int(row_value(row, definitions, "duration", "value"))
        overlap_start = max(start_ns, phase_start)
        overlap_end = min(start_ns + duration_ns, phase_end)
        if not overlap_end > overlap_start:
            continue
        overlap = overlap_end - overlap_start
        running_ns += overlap
        priority = support.text_of(row_value(row, definitions, "sched-priority", "fmt"))
        core = support.text_of(row_value(row, definitions, "core", "fmt"))
        priorities[priority or "unknown"] += overlap
        match = re.search(r"\(([^)]+)\)", core)
        cores[match.group(1) if match else "unknown"] += overlap

    denominator = max(running_ns, 1)

    def share(values):
        return {key: support.round_half_up(float(amount) / denominator * 100, 3) for key, amount in values.items()}

    return {
        "run": support.first_present(summary.get("run"), os.path.basename(directory)),
        "variant": summary.get("variant"),
        "focusMode": summary.get("focusMode"),
        "valid": support.valid_summary(summary),
        "exclusionReasons": support.exclusion_reasons(summary),
        "range_seconds": support.round_half_up((phase_end - phase_start) / 1_000_000_000.0, 3),
        "main_running_seconds": support.round_half_up(running_ns / 1_000_000_000.0, 3),
        "priority_share_percent": share(priorities),
        "core_share_percent": share(cores),
    }


def main(arguments):
    support.usage_or_exit(arguments, "analyze-thread-state.py")
    support.print_report([analyze(support.run_directory(argument)) for argument in arguments])


if __name__ == "__main__":
    main(sys.argv[1:])
