#!/usr/bin/env python3
"""Attribute Time Profiler samples inside each run's measured phase."""
from __future__ import annotations

import os
import re
import sys
from collections import Counter

import trace_support as support

CATEGORIES = {
    "renderer_tick": [r"StreamingCommitCoordinator\.frameTick"],
    "markdown": [r"MarkdownBlockTreeParser", r"TranscriptProjectionStore\.append"],
    "artifact_measure": [r"makeTextArtifact", r"TranscriptTextArtifact", r"CTFramesetter", r"CTLine", r"CoreText"],
    "layout_coordinator": [r"TranscriptLayoutCoordinator\.commit"],
    "frame_size": [r"setFrameSize:", r"setFrameSize\("],
    "clip_scroll": [r"NSClipView.*scrollTo", r"TranscriptLayoutCoordinator\.scroll"],
    "scroll_reflection": [r"reflectScrolledClipView", r"_reflectDocumentViewFrameChange"],
    "forced_layout": [r"layoutSubtreeIfNeeded", r"_layoutSubtreeIfNeeded", r"___NSViewLayout"],
    "collection": [r"NSCollectionView", r"TranscriptCollectionLayout"],
    "accessibility": [r"Accessibility", r"accessibility"],
    "tracking_areas": [r"TrackingArea", r"trackingArea", r"CursorRect", r"cursorRect"],
    "fade": [r"Fade", r"fade", r"NSAnimationContext", r"CAAnimation", r"displayIfNeeded"],
    "drawing": [r"drawRect", r"drawInContext", r"CGContext", r"CoreGraphics", r"vImage"],
    "quartz_core": [r"QuartzCore", r"CA::", r"CALayer"],
    "swiftui": [r"SwiftUI", r"NSHostingView"],
    "objc_runtime": [r"objc_", r"swift_retain", r"swift_release", r"malloc", r"free"],
}
PATTERNS = {category: [re.compile(pattern) for pattern in patterns] for category, patterns in CATEGORIES.items()}
FRAME_TICK = "StreamingCommitCoordinator.frameTick"


def dereferenced(node, elements):
    """xctrace writes a repeated element once and refers back to it by id."""
    if node is None or node.get("ref") is None:
        return node
    return elements.get(node.get("ref"))


def sample_rows(root, definitions, elements):
    rows = []
    for row in root.iter("row"):
        time_node = row.find("sample-time")
        thread_node = row.find("thread")
        weight_node = row.find("weight")
        core_node = row.find("core")
        tagged = dereferenced(row.find("tagged-backtrace"), elements)
        backtrace = dereferenced(tagged.find("backtrace"), elements) if tagged is not None else None
        if time_node is None or thread_node is None or weight_node is None or backtrace is None:
            continue
        frames = [support.referenced_value(frame, definitions, "name") for frame in backtrace.findall("frame")]
        rows.append({
            "time_ns": support.leading_int(support.referenced_value(time_node, definitions, "value")),
            "thread": support.text_of(support.referenced_value(thread_node, definitions, "fmt")),
            "core": (support.text_of(support.referenced_value(core_node, definitions, "fmt"))
                     if core_node is not None else "<unknown>"),
            "weight_ns": support.leading_int(support.referenced_value(weight_node, definitions, "value")),
            "frames": [frame for frame in frames if frame is not None],
        })
    return rows


def thread_group(thread: str) -> str:
    if thread.startswith("Main Thread"):
        return "main"
    if "SwiftUI.AsyncRenderer" in thread:
        return "swiftui_async"
    if re.search(r"CA::|com\.apple\.coremedia|CVDisplayLink", thread):
        return "composition"
    return "other"


def first_group(pattern: str, text: str):
    match = re.search(pattern, text)
    return match.group(1) if match else None


def analyze(directory):
    summary = support.load_summary(directory)
    path = os.path.join(directory, "time-profile.xml")
    if not os.path.isfile(path):
        support.fail(f"error: missing {path}")
    phase_start, phase_end = support.phase_range_ns(directory, summary)
    root = support.parse_xml(path)
    elements = {node.get("id"): node for node in root.iter() if node.get("id") is not None}
    definitions = {
        identifier: {
            "name": node.get("name"),
            "fmt": node.get("fmt"),
            "value": support.strip_text(support.element_text(node)),
        }
        for identifier, node in elements.items()
    }

    scoped = [row for row in sample_rows(root, definitions, elements) if phase_start <= row["time_ns"] <= phase_end]
    duration_ns = max(phase_end - phase_start, 1)
    tick_rows = [row for row in scoped if any(FRAME_TICK in name for name in row["frames"])]

    totals = Counter()
    self_symbols = Counter()
    categories = Counter()
    tick_categories = Counter()
    thread_groups = Counter()
    core_groups = Counter()
    core_indices = Counter()

    for row in scoped:
        weight = row["weight_ns"]
        totals["cpu_ns"] += weight
        self_symbols[row["frames"][0] if row["frames"] else "<unknown>"] += weight
        thread_groups[thread_group(row["thread"])] += weight
        core_groups[support.first_present(first_group(r"\(([^)]+)\)", row["core"]), row["core"])] += weight
        core_indices[first_group(r"CPU (\d+)", row["core"]) or "unknown"] += weight

        text = "\n".join(row["frames"])
        for category, patterns in PATTERNS.items():
            if not any(pattern.search(text) for pattern in patterns):
                continue
            categories[category] += weight
            if FRAME_TICK in text:
                tick_categories[category] += weight

    cpu_ns = totals.get("cpu_ns", 0)
    cpu_total = max(cpu_ns, 1)

    def percent(value):
        return support.round_half_up(float(value) / duration_ns * 100, 3)

    def share(value):
        return support.round_half_up(float(value) / cpu_total * 100, 3)

    admissions = summary.get("phaseAdmissions")
    if not support.present(admissions):
        # Older summaries carry no count; recount the log's accepted work.
        admissions = len(support.publication_times(directory))
    admissions = support.leading_int(admissions)

    def per_admission(nanoseconds):
        return support.round_half_up(float(nanoseconds) / admissions / 1_000_000, 3) if admissions > 0 else None

    # Ties keep first-seen order, so equal weights rank deterministically.
    ranked_symbols = sorted(self_symbols.items(), key=lambda item: -item[1])[:50]
    ranked_cores = sorted(core_indices.items(), key=lambda item: support.leading_int(item[0]))

    return {
        "run": support.first_present(summary.get("run"), os.path.basename(directory)),
        "variant": summary.get("variant"),
        "focusMode": summary.get("focusMode"),
        "valid": support.valid_summary(summary),
        "exclusionReasons": support.exclusion_reasons(summary),
        "path": path,
        "range_seconds": support.round_half_up(duration_ns / 1_000_000_000.0, 3),
        "row_count": len(scoped),
        "tick_sample_count": len(tick_rows),
        "sampled_cpu_percent": percent(cpu_ns),
        "sampled_cpu_ms_per_admission": per_admission(cpu_ns),
        "renderer_tick_ms_per_admission": per_admission(sum(row["weight_ns"] for row in tick_rows)),
        "thread_cpu_percent": {key: percent(value) for key, value in thread_groups.items()},
        "core_cpu_percent": {key: percent(value) for key, value in core_groups.items()},
        "core_cpu_share_percent": {key: share(value) for key, value in core_groups.items()},
        "core_index_share_percent": {key: share(value) for key, value in ranked_cores},
        "inclusive_cpu_percent": {key: percent(value) for key, value in categories.items()},
        "inclusive_cpu_share_percent": {key: share(value) for key, value in categories.items()},
        "tick_inclusive_cpu_percent": {key: percent(value) for key, value in tick_categories.items()},
        "top_self_cpu_percent": {key: percent(value) for key, value in ranked_symbols},
    }


def main(arguments):
    support.usage_or_exit(arguments, "analyze-transcript-time-profile.py")
    support.print_report([analyze(support.run_directory(argument)) for argument in arguments])


if __name__ == "__main__":
    main(sys.argv[1:])
