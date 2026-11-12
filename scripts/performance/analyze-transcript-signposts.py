#!/usr/bin/env python3
"""Summarize transcript signpost events and intervals inside each run's measured phase."""
from __future__ import annotations

import os
import re
import sys
from collections import Counter, defaultdict

import trace_support as support

CUSTOM_SUBSYSTEM = "dev.ottam.Ottam"
SYNCHRONOUS_PARTS = ("render.prepare", "collection.apply", "layout.commit")


def id_map(root) -> dict:
    return {node.get("id"): node for node in root.iter() if node.get("id") is not None}


def column(row, index):
    return row[index] if index < len(row) else None


def record(fields, key, source):
    """Append the message field named by the key's last component."""
    fields[key].append(field(source, key.rsplit(".", 1)[-1]))


def target_of(node, ids):
    if node is None:
        return None
    return ids.get(node.get("ref")) if node.get("ref") is not None else node


def resolved(node, ids):
    target = target_of(node, ids)
    if target is None:
        return None
    return target.get("fmt") if target.get("fmt") is not None else support.element_text(target)


def raw_number(node, ids):
    target = target_of(node, ids)
    return None if target is None else support.leading_float(support.element_text(target))


def field(message, name):
    if message is None:
        return None
    match = re.search(re.escape(name) + r"=\s*([+-]?[0-9,.]+)", message, re.ASCII)
    return support.leading_float(match.group(1).replace(",", "")) if match else None


def string_field(message, name):
    if message is None:
        return None
    match = re.search(re.escape(name) + r"=\s*([A-Za-z0-9_.-]+)", message, re.ASCII)
    return match.group(1) if match else None


def successful_publication(message) -> bool:
    # log stream prints Bool as true/false; xctrace exports its integer value.
    return string_field(message, "succeeded") in ("true", "1")


def transaction_id(message):
    value = field(message, "transaction")
    return None if value is None else int(value)


def stats(values, scale=1.0) -> dict:
    ordered = sorted(value / scale for value in values if value is not None)
    if not ordered:
        return {"count": 0}

    def percentile(fraction):
        return ordered[support.percentile_index(fraction, len(ordered))]

    return {
        "count": len(ordered),
        "p50": support.round_half_up(percentile(0.50), 4),
        "p95": support.round_half_up(percentile(0.95), 4),
        "p99": support.round_half_up(percentile(0.99), 4),
        "max": support.round_half_up(ordered[-1], 4),
        "mean": support.round_half_up(support.compensated_sum(ordered) / len(ordered), 4),
    }


def signpost_phase_range(path, summary, directory):
    if (support.present(summary.get("phaseStartTimestamp")) and support.present(summary.get("phaseEndTimestamp"))
            and os.path.isfile(os.path.join(directory, "trace-toc.xml"))):
        return support.phase_range_ns(directory, summary)

    root = support.parse_xml(path)
    ids = id_map(root)
    times = defaultdict(list)
    for row in root.iter("row"):
        if resolved(column(row, 9), ids) != CUSTOM_SUBSYSTEM:
            continue
        name = resolved(column(row, 6), ids)
        message = resolved(column(row, 11), ids)
        if not ((name == "normalized.publication" and successful_publication(message))
                or (name == "stream.admission" and string_field(message, "disposition") == "accepted")):
            continue
        moment = raw_number(column(row, 0), ids)
        if moment is not None:
            times[name].append(moment)
    publications = times["normalized.publication"]
    normalized = bool(publications)
    if not normalized:
        publications = times["stream.admission"]
    if not publications:
        support.fail(f"error: no successful publications or legacy admissions in {path}")

    def ordinal(key, fallback):
        value = summary.get(key)
        return fallback if value is None else support.leading_int(value)

    start_ordinal = 1 if normalized else ordinal("phaseAdmissionStartOrdinal", 1)
    end_ordinal = len(publications) if normalized else ordinal("phaseAdmissionEndOrdinal", len(publications))
    start_index = min(max(start_ordinal - 1, 0), len(publications) - 1)
    end_index = min(max(end_ordinal - 1, start_index), len(publications) - 1)
    return publications[start_index], publications[end_index]


def within(phase, moment) -> bool:
    return phase[0] <= moment <= phase[1]


def parse_events(path, phase):
    if not os.path.isfile(path):
        support.fail(f"error: missing {path}")
    root = support.parse_xml(path)
    ids = id_map(root)
    fields = defaultdict(list)
    counts = Counter()

    for row in root.iter("row"):
        if resolved(column(row, 9), ids) != CUSTOM_SUBSYSTEM:
            continue
        moment = raw_number(column(row, 0), ids)
        if moment is None or not within(phase, moment):
            continue
        name = resolved(column(row, 6), ids)
        message = resolved(column(row, 11), ids)

        if name == "normalized.publication":
            outcome = "succeeded" if successful_publication(message) else "failed"
            counts[f"normalized.publication.{outcome}"] += 1
        elif name == "stream.admission":
            if string_field(message, "disposition") != "accepted":
                continue
            for key in ("admission.oldest_pending_ms", "admission.bytes", "admission.deltas"):
                record(fields, key, message)
        elif name == "display.tick":
            for key in ("tick.spacing_ms", "tick.missed_slots", "tick.pending_bytes", "tick.pending_deltas"):
                record(fields, key, message)
        elif name == "collection.invalidation":
            scope = string_field(message, "scope") or "unknown"
            origin = string_field(message, "origin") or "unknown"
            counts[f"collection.invalidation.scope.{scope}"] += 1
            counts[f"collection.invalidation.origin.{origin}"] += 1
            for suffix in ("invalidated_items", "dirty_start", "invalidate_everything",
                           "invalidate_data_source_counts", "update_bracket",
                           "content_size_delta", "content_offset_delta"):
                record(fields, f"collection.invalidation.{suffix}", message)
        elif name == "collection.update-lifecycle":
            stage = string_field(message, "phase") or "unknown"
            counts[f"collection.update-lifecycle.phase.{stage}"] += 1
            for suffix in ("bracket", "update_items", "earliest_affected"):
                record(fields, f"collection.update-lifecycle.{suffix}", message)
        elif name == "collection.environment":
            for suffix in ("app_active", "key_window", "window_visible"):
                record(fields, f"collection.environment.{suffix}", message)
            counts[f"collection.environment.thermal.{string_field(message, 'thermal_state') or 'unknown'}"] += 1
            counts[f"collection.environment.thread_qos.{string_field(message, 'thread_qos') or 'unknown'}"] += 1
        elif name == "collection.clip-bounds":
            counts["collection.clip-bounds"] += 1
            record(fields, "collection.clip-bounds.delta_y", message)
    return fields, counts


def parse_intervals(path, phase):
    if not os.path.isfile(path):
        support.fail(f"error: missing {path}")
    root = support.parse_xml(path)
    ids = id_map(root)
    durations = defaultdict(list)
    fields = defaultdict(list)
    counts = Counter()
    transactions = defaultdict(dict)

    for row in root.iter("row"):
        if resolved(column(row, 5), ids) != CUSTOM_SUBSYSTEM:
            continue
        start = raw_number(column(row, 0), ids)
        if start is None or not within(phase, start):
            continue
        duration = raw_number(column(row, 1), ids)
        name = resolved(column(row, 3), ids)
        message = resolved(column(row, 11), ids)
        end_message = resolved(column(row, 12), ids)

        if name == "commit.present":
            proxy = string_field(end_message, "proxy") or "unknown"
            counts[f"commit.present.proxy.{proxy}"] += 1
            # Failure/teardown intervals explain discarded work, never presentation latency.
            if proxy != "ca_transaction":
                durations[f"commit.present.{proxy}"].append(duration)
                continue
        durations[name].append(duration)
        counts[name] += 1
        if name == "collection.prepare":
            record(fields, f"{name}.rebuilt_items", end_message)
        elif name == "collection.attributes":
            record(fields, f"{name}.requested_height", message)
            record(fields, f"{name}.expanded_height", message)
            record(fields, f"{name}.intersecting_items", end_message)
            record(fields, f"{name}.returned_items", end_message)
        elif name == "collection.frame":
            for suffix in ("requested_width_delta", "requested_height_delta",
                           "actual_width_delta", "actual_height_delta"):
                record(fields, f"{name}.{suffix}", end_message)
            changed = support.leading_int(field(end_message, "changed"))
            counts[f"collection.frame.changed.{changed}"] += 1
            durations[f"collection.frame.changed.{changed}"].append(duration)
        elif name == "collection.scroll":
            target = string_field(message, "target") or "unknown"
            for suffix in ("requested_delta_y", "constrained_delta_y", "actual_delta_y"):
                record(fields, f"{name}.{suffix}", end_message)
            moved = support.leading_int(field(end_message, "moved"))
            reflected = support.leading_int(field(end_message, "reflected"))
            counts[f"collection.scroll.target.{target}"] += 1
            counts[f"collection.scroll.moved.{moved}"] += 1
            counts[f"collection.scroll.reflected.{reflected}"] += 1
            durations[f"collection.scroll.moved.{moved}"].append(duration)
        elif name == "collection.materialize":
            record(fields, f"{name}.needed_layout_before", message)
            record(fields, f"{name}.visible_before", message)
            for suffix in ("visible_after", "appeared", "disappeared"):
                record(fields, f"{name}.{suffix}", end_message)
            changed = support.leading_int(field(end_message, "visible_set_changed"))
            counts[f"collection.materialize.visible_set_changed.{changed}"] += 1
            durations[f"collection.materialize.visible_set_changed.{changed}"].append(duration)
        elif name == "collection.refresh":
            mode = string_field(message, "mode") or "unknown"
            record(fields, f"{name}.requested_items", message)
            record(fields, f"{name}.refreshed_items", end_message)
            record(fields, f"{name}.selection_refreshed", end_message)
            counts[f"collection.refresh.mode.{mode}"] += 1
            durations[f"collection.refresh.mode.{mode}"].append(duration)
        elif name == "layout.commit":
            record(fields, f"{name}.synchronous_count", end_message)
            materialization = string_field(end_message, "materialization") or "unknown"
            counts[f"layout.commit.materialization.{materialization}"] += 1
        transaction = transaction_id(message)
        if transaction is not None and name in SYNCHRONOUS_PARTS:
            transactions[transaction][name] = duration

    durations["transaction.synchronous"] = [
        support.compensated_sum(parts[name] for name in SYNCHRONOUS_PARTS)
        for parts in transactions.values()
        if all(name in parts for name in SYNCHRONOUS_PARTS)
    ]
    return durations, fields, counts


def stats_by_key(values_by_key, scale=1.0):
    return {key: stats(values, scale) for key, values in values_by_key.items()}


def main(arguments):
    support.usage_or_exit(arguments, "analyze-transcript-signposts.py")
    result = {}
    excluded = []
    aggregates = {}

    def aggregate(group):
        if group not in aggregates:
            aggregates[group] = {"events": defaultdict(list), "intervals": defaultdict(list),
                                 "interval_fields": defaultdict(list), "counts": Counter()}
        return aggregates[group]

    for argument in arguments:
        directory = support.run_directory(argument)
        summary = support.load_summary(directory)
        events_path = os.path.join(directory, "signpost-events.xml")
        phase = signpost_phase_range(events_path, summary, directory)
        event_fields, event_counts = parse_events(events_path, phase)
        durations, interval_fields, interval_counts = parse_intervals(
            os.path.join(directory, "signpost-intervals.xml"), phase
        )
        run = support.first_present(summary.get("run"), os.path.basename(directory))
        counts = Counter(event_counts)
        counts.update(interval_counts)
        result[run] = {
            "summary": summary,
            "events": stats_by_key(event_fields),
            "intervals_ms": stats_by_key(durations, 1_000_000.0),
            "interval_fields": stats_by_key(interval_fields),
            "counts": counts,
        }

        if not support.valid_summary(summary):
            excluded.append({"run": run, "reasons": support.exclusion_reasons(summary)})
            continue

        variant = support.first_present(summary.get("variant"), "baseline" if "baseline" in run else "feature")
        group = ".".join(support.text_of(value) for value in (variant, summary.get("focusMode"), summary.get("scenario")))
        for key, values in event_fields.items():
            aggregate(group)["events"][key].extend(values)
        for key, values in durations.items():
            aggregate(group)["intervals"][key].extend(values)
        for key, values in interval_fields.items():
            aggregate(group)["interval_fields"][key].extend(values)
        aggregate(group)["counts"].update(event_counts)
        aggregate(group)["counts"].update(interval_counts)

    result["_aggregates"] = {
        group: {
            "events": stats_by_key(values["events"]),
            "intervals_ms": stats_by_key(values["intervals"], 1_000_000.0),
            "interval_fields": stats_by_key(values["interval_fields"]),
            "counts": values["counts"],
        }
        for group, values in aggregates.items()
    }
    result["_excluded"] = excluded
    support.print_report(result)


if __name__ == "__main__":
    main(sys.argv[1:])
