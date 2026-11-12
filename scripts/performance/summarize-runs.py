#!/usr/bin/env python3
"""Group measured runs, and compare baseline/feature pairs that are comparable."""
from __future__ import annotations

import os
import sys

import trace_support as support

COMPARABILITY_FIELDS = (
    "buildConfiguration",
    "fixtureKnobs",
    "fixtureSourceSHA256",
    "machineIdentitySHA256",
    "phaseDefinition",
    "phaseWorkload",
)


def stats(values: list) -> dict:
    ordered = sorted(support.leading_float(value) for value in values if value is not None)
    if not ordered:
        return {"count": 0}

    def percentile(fraction: float) -> float:
        return ordered[support.percentile_index(fraction, len(ordered))]

    return {
        "count": len(ordered),
        "min": support.round_half_up(ordered[0], 4),
        "p50": support.round_half_up(percentile(0.50), 4),
        "p95": support.round_half_up(percentile(0.95), 4),
        "max": support.round_half_up(ordered[-1], 4),
        "mean": support.round_half_up(support.compensated_sum(ordered) / len(ordered), 4),
    }


def mismatch_reasons(members: list) -> list:
    reasons = []
    for name in COMPARABILITY_FIELDS:
        values = [summary.get(name) for summary in members]
        if any(value is None for value in values):
            reasons.append(f"missing_{name}")
        elif len({support.comparison_key(value) for value in values}) != 1:
            reasons.append(f"mismatched_{name}")
    return reasons


def group_by(items: list, key) -> dict:
    groups: dict = {}
    for item in items:
        groups.setdefault(key(item), []).append(item)
    return groups


def label(*values) -> str:
    return ".".join(support.text_of(value) for value in values)


def summarize_group(members: list) -> dict:
    return {
        "runs": [summary.get("run") for summary in members],
        "cpuPercentOfPhase": stats([summary.get("cpuPercentOfPhase") for summary in members]),
        "cpuSeconds": stats([summary.get("cpuSeconds") for summary in members]),
        "phaseWallSeconds": stats([summary.get("phaseWallSeconds") for summary in members]),
        "phaseAdmissions": stats([summary.get("phaseAdmissions") for summary in members]),
    }


def summarize_pair(members: list) -> dict:
    variants = {
        variant: [{"run": summary.get("run"), "cpuPercentOfPhase": summary.get("cpuPercentOfPhase")}
                  for summary in runs]
        for variant, runs in group_by(
            members, lambda summary: support.first_present(summary.get("variant"), "unspecified")
        ).items()
    }
    reasons = mismatch_reasons(members)
    variants_complete = len(variants.get("baseline", [])) == 1 and len(variants.get("feature", [])) == 1
    if not variants_complete:
        reasons.append("requires_exactly_one_baseline_and_feature")
    complete = variants_complete and not reasons
    result = {"complete": complete, "mismatchReasons": reasons, "variants": variants}
    if complete:
        baseline = support.leading_float(variants["baseline"][0]["cpuPercentOfPhase"])
        feature = support.leading_float(variants["feature"][0]["cpuPercentOfPhase"])
        result["featureDeltaPercent"] = (
            None if baseline == 0 else support.round_half_up((feature - baseline) / baseline * 100, 3)
        )
    return result


def main(arguments: list) -> None:
    support.usage_or_exit(arguments, "summarize-runs.py")
    summaries = []
    for argument in arguments:
        directory = support.run_directory(argument)
        summary = support.load_summary(directory)
        summary["directory"] = directory
        summaries.append(summary)

    excluded = [
        {
            "run": support.first_present(summary.get("run"), os.path.basename(summary["directory"])),
            "reasons": support.exclusion_reasons(summary),
        }
        for summary in summaries
        if not support.valid_summary(summary)
    ]
    valid = [summary for summary in summaries if support.valid_summary(summary)]

    groups = group_by(valid, lambda summary: ".".join(
        support.text_of(support.first_present(summary.get(name), "unspecified"))
        for name in ("variant", "focusMode", "scenario", "traceTemplate")
    ))
    paired = [summary for summary in valid if support.text_of(summary.get("pair")) != ""]
    pairs = group_by(paired, lambda summary: label(
        summary.get("pair"), summary.get("focusMode"), summary.get("scenario"), summary.get("traceTemplate")
    ))

    support.print_report({
        "runs": summaries,
        "groups": {name: summarize_group(members) for name, members in groups.items()},
        "pairs": {name: summarize_pair(members) for name, members in pairs.items()},
        "excluded": excluded,
    })


if __name__ == "__main__":
    main(sys.argv[1:])
