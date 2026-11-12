"""Shared loading, phase, and reporting helpers for the performance analyzers.

The number rules here (half-away-from-zero rounding, compensated sums, lenient
numeric parsing, and float spelling) keep reports comparable with runs
recorded before the analyzers moved to Python.
"""
from __future__ import annotations

import json
import math
import os
import re
import sys
import time
import xml.etree.ElementTree as ElementTree
from fractions import Fraction
from typing import Any, Iterable, Optional

ASCII_WHITESPACE = " \t\n\v\f\r"
LEADING_FLOAT = re.compile(r"[+-]?(?:\d+(?:_\d+)*)?(?:\.\d+(?:_\d+)*)?(?:[eE][+-]?\d+(?:_\d+)*)?", re.ASCII)
LEADING_INT = re.compile(r"[+-]?\d+(?:_\d+)*", re.ASCII)
ISO8601 = re.compile(
    r"\s*(-?\d+)-(\d\d)-(\d\d)"
    r"(?:T(\d\d):(\d\d):(\d\d)(\.\d+)?(Z|[+-]\d\d(?::?\d\d)?)?)?\s*\Z",
    re.ASCII | re.IGNORECASE,
)


def fail(message: str) -> None:
    sys.exit(message)


def usage_or_exit(arguments: list[str], name: str) -> None:
    if not arguments or "--help" in arguments or "-h" in arguments:
        print(f"Usage: {name} RUN_DIRECTORY [RUN_DIRECTORY ...]", file=sys.stderr)
        sys.exit(2 if not arguments else 0)


def run_directory(argument: str) -> str:
    return os.path.abspath(os.path.expanduser(argument))


# --- Values -----------------------------------------------------------------

def present(value: Any) -> bool:
    """Only a missing value or false counts as absent; 0 and "" are values."""
    return value is not None and value is not False


def first_present(value: Any, fallback: Any) -> Any:
    return value if present(value) else fallback


def leading_float(text: Any) -> float:
    """Parse the leading number of a string; no number reads as 0.0."""
    if text is None:
        return 0.0
    if isinstance(text, (int, float)) and not isinstance(text, bool):
        return float(text)
    stripped = str(text).lstrip(ASCII_WHITESPACE)
    match = LEADING_FLOAT.match(stripped)
    digits = match.group(0).replace("_", "") if match else ""
    if not re.search(r"[0-9]", re.split(r"[eE]", digits)[0]):
        return 0.0
    return float(digits)


def leading_int(text: Any) -> int:
    """Parse the leading integer of a value; no integer reads as 0."""
    if text is None:
        return 0
    if isinstance(text, float):
        return int(text)
    if isinstance(text, int) and not isinstance(text, bool):
        return text
    match = LEADING_INT.match(str(text).lstrip(ASCII_WHITESPACE))
    return int(match.group(0).replace("_", "")) if match else 0


def float_text(value: float) -> str:
    """Shortest round-trip digits; fixed when a fraction remains or |value| < 1e15."""
    if math.isnan(value) or math.isinf(value):
        raise ValueError(f"{value} is not allowed in a report")
    sign = "-" if math.copysign(1.0, value) < 0 else ""
    if value == 0:
        return sign + "0.0"
    mantissa, _, exponent = repr(abs(value)).partition("e")
    whole, _, fraction = mantissa.partition(".")
    digits = (whole + fraction).lstrip("0")
    point = len(whole) + (int(exponent) if exponent else 0) - (len(whole + fraction) - len((whole + fraction).lstrip("0")))
    digits = digits.rstrip("0") or "0"
    if 0 < point and (point < len(digits) or point <= 15):
        integral = digits[:point].ljust(point, "0")
        return f"{sign}{integral}.{digits[point:] or '0'}"
    if -4 < point <= 0:
        return f"{sign}0.{'0' * -point}{digits}"
    return f"{sign}{digits[0]}.{digits[1:] or '0'}e{point - 1:+03d}"


def text_of(value: Any) -> str:
    if value is None:
        return ""
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, float):
        return float_text(value)
    return str(value)


def comparison_key(value: Any) -> Any:
    """Equality that keeps 1 and 1.0 apart and ignores object key order."""
    if isinstance(value, dict):
        return ("object", frozenset((comparison_key(k), comparison_key(v)) for k, v in value.items()))
    if isinstance(value, list):
        return ("array", tuple(comparison_key(item) for item in value))
    return (type(value).__name__, value)


def round_half_up(value: float, digits: int = 0) -> Any:
    """Round half away from zero; digits=0 returns an int."""
    if digits == 0:
        return _round_to_int(value)
    if value == 0.0:
        return value
    _, binary_exponent = math.frexp(value)
    if digits >= 17 - (_trunc_div(binary_exponent, 4) if binary_exponent > 0 else _trunc_div(binary_exponent, 3) - 1):
        return value
    if digits < -(_trunc_div(binary_exponent, 3) + 1 if binary_exponent > 0 else _trunc_div(binary_exponent, 4)):
        return 0.0
    scale = 10.0 ** digits
    scaled = value * scale
    rounded = math.copysign(float(_round_to_int(scaled)), scaled)
    if value > 0:
        if (rounded + 0.5) / scale <= value:
            rounded += 1
    elif (rounded - 0.5) / scale >= value:
        rounded -= 1
    return rounded / scale


def _trunc_div(numerator: int, denominator: int) -> int:
    quotient = abs(numerator) // denominator
    return quotient if numerator >= 0 else -quotient


def _round_to_int(value: float) -> int:
    if value < 0:
        return -_round_to_int(-value)
    floor = math.floor(value)
    return floor + 1 if value - floor >= 0.5 else floor


def compensated_sum(values: Iterable[Any]) -> Any:
    """Sum integers exactly; once a float appears, use Kahan-Babuska compensation."""
    items = list(values)
    if all(isinstance(item, int) and not isinstance(item, bool) for item in items):
        return sum(items)
    total = 0.0
    compensation = 0.0
    for item in items:
        addend = float(item)
        if math.isnan(total):
            continue
        if math.isnan(addend):
            total = addend
            continue
        if math.isinf(addend):
            if math.isinf(total) and math.copysign(1, addend) != math.copysign(1, total):
                total = math.nan
            else:
                total = addend
            continue
        if math.isinf(total):
            continue
        result = total + addend
        if abs(total) >= abs(addend):
            compensation += (total - result) + addend
        else:
            compensation += (addend - result) + total
        total = result
    return total + compensation


def percentile_index(fraction: float, length: int) -> int:
    return max(math.ceil(fraction * length) - 1, 0)


# --- Reports ----------------------------------------------------------------

def json_text(value: Any, indent: str = "") -> str:
    """Pretty JSON with two-space indent and the report's float spelling."""
    inner = indent + "  "
    if isinstance(value, dict):
        if not value:
            return "{}"
        members = [f"{inner}{json.dumps(text_of(key), ensure_ascii=False)}: {json_text(item, inner)}"
                   for key, item in value.items()]
        return "{\n" + ",\n".join(members) + "\n" + indent + "}"
    if isinstance(value, list):
        if not value:
            return "[]"
        return "[\n" + ",\n".join(inner + json_text(item, inner) for item in value) + "\n" + indent + "]"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if value is None:
        return "null"
    if isinstance(value, float):
        return float_text(value)
    if isinstance(value, int):
        return str(value)
    return json.dumps(value, ensure_ascii=False)


def print_report(value: Any) -> None:
    # Bytes, so the report is UTF-8 whatever the locale's encoding.
    sys.stdout.buffer.write((json_text(value) + "\n").encode("utf-8"))


# --- XML --------------------------------------------------------------------

def parse_xml(path: str) -> ElementTree.Element:
    return ElementTree.parse(path).getroot()


def element_text(node: ElementTree.Element) -> str:
    return "".join(node.itertext())


def strip_text(text: str) -> str:
    return text.lstrip(ASCII_WHITESPACE).rstrip(ASCII_WHITESPACE + "\0")


def referenced_value(node: Optional[ElementTree.Element], definitions: dict, attribute: str) -> Optional[str]:
    if node is None:
        return None
    if node.get(attribute) is not None:
        return node.get(attribute)
    if node.get("ref") is not None:
        return definitions.get(node.get("ref"), {}).get(attribute)
    return strip_text(element_text(node)) if attribute == "value" else None


# --- Time -------------------------------------------------------------------

def epoch_iso8601(text: str) -> Fraction:
    """Exact seconds since the epoch for an ISO 8601 timestamp."""
    match = ISO8601.match(text)
    if not match:
        raise ValueError(f"invalid xmlschema format: {text!r}")
    year, month, day, hour, minute, second, fraction, zone = match.groups()
    fields = (int(year), int(month), int(day), int(hour or 0), int(minute or 0), int(second or 0))
    if not _valid_fields(*fields):
        raise ValueError(f"argument out of range: {text!r}")
    if hour is None or zone is None:
        whole = Fraction(int(time.mktime(fields + (0, 0, -1))))
    else:
        days = _days_from_civil(fields[0], fields[1], fields[2])
        whole = Fraction(days * 86400 + fields[3] * 3600 + fields[4] * 60 + fields[5])
        if zone.upper() != "Z":
            sign = -1 if zone[0] == "-" else 1
            offset = zone[1:].replace(":", "")
            minutes = int(offset[:2]) * 60 + (int(offset[2:4]) if len(offset) > 2 else 0)
            whole -= sign * minutes * 60
    return whole + (Fraction(fraction[1:]) / 10 ** (len(fraction) - 1) if fraction else 0)


def epoch_log_time(text: str) -> Fraction:
    """Exact seconds since the epoch for a local 'YYYY-MM-DD HH:MM:SS.mmm' stamp."""
    parsed = time.strptime(text[:19], "%Y-%m-%d %H:%M:%S")
    milliseconds = text[20:23]
    if text[19:20] != "." or not re.fullmatch(r"\d{3}", milliseconds):
        raise ValueError(f"invalid log timestamp: {text!r}")
    return Fraction(int(time.mktime(parsed))) + Fraction(int(milliseconds), 1000)


def _valid_fields(year: int, month: int, day: int, hour: int, minute: int, second: int) -> bool:
    """The ranges Ruby's Time accepts: 24:00:00 and a leap second are allowed."""
    if not 1 <= month <= 12:
        return False
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    month_days = (31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)[month - 1]
    clock = (hour < 24 or (hour == 24 and minute == second == 0)) and minute < 60 and second <= 60
    return 1 <= day <= month_days and clock


def _days_from_civil(year: int, month: int, day: int) -> int:
    year -= month <= 2
    era = (year if year >= 0 else year - 399) // 400
    year_of_era = year - era * 400
    day_of_year = (153 * (month + (-3 if month > 2 else 9)) + 2) // 5 + day - 1
    day_of_era = year_of_era * 365 + year_of_era // 4 - year_of_era // 100 + day_of_year
    return era * 146097 + day_of_era - 719468


def nanoseconds_between(origin: Fraction, moment: Fraction) -> int:
    return _round_to_int(float(moment - origin) * 1_000_000_000)


# --- Runs -------------------------------------------------------------------

def load_summary(directory: str) -> dict:
    path = os.path.join(directory, "summary.json")
    if not os.path.isfile(path):
        fail(f"error: missing {path}")
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def trace_start(directory: str) -> Fraction:
    path = os.path.join(directory, "trace-toc.xml")
    if not os.path.isfile(path):
        fail(f"error: missing {path}")
    node = next(parse_xml(path).iter("start-date"), None)
    value = element_text(node) if node is not None else ""
    if not value:
        fail(f"error: trace start date missing from {path}")
    return epoch_iso8601(value)


def phase_range_ns(directory: str, summary: dict) -> tuple[int, int]:
    start_text = summary.get("phaseStartTimestamp")
    end_text = summary.get("phaseEndTimestamp")
    if present(start_text) and present(end_text):
        origin = trace_start(directory)
        range_start = nanoseconds_between(origin, epoch_iso8601(start_text))
        range_end = nanoseconds_between(origin, epoch_iso8601(end_text))
        if not range_end > range_start:
            fail(f"error: invalid measured phase in {directory}")
        return range_start, range_end

    publications = publication_times(directory)
    if not publications:
        fail(f"error: no successful publications or legacy admissions in {directory}")
    origin = trace_start(directory)
    return nanoseconds_between(origin, publications[0]), nanoseconds_between(origin, publications[-1])


def read_lines(path: str) -> list[str]:
    with open(path, encoding="utf-8", errors="surrogateescape", newline="") as handle:
        text = handle.read()
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line[:-1] if line.endswith("\r") else line for line in lines]


def publication_times(directory: str) -> list[Fraction]:
    path = os.path.join(directory, "signposts.log")
    if not os.path.isfile(path):
        fail(f"error: missing {path}")
    lines = read_lines(path)
    publications = [line for line in lines if re.search(r"normalized\.publication.*succeeded=\s*true", line, re.ASCII)]
    if not publications:
        publications = [line for line in lines if re.search(r"stream\.admission.*disposition=\s*accepted", line, re.ASCII)]
    return [epoch_log_time(line[:23]) for line in publications]


def valid_summary(summary: dict) -> bool:
    return (
        summary.get("scenarioResult") == "passed"
        and summary.get("focusStable") is True
        and summary.get("scrollStable", True) is True
        and summary.get("phaseValid", True) is True
        and summary.get("archiveSucceeded", True) is True
    )


def exclusion_reasons(summary: dict) -> list[str]:
    reasons = []
    if summary.get("scenarioResult") != "passed":
        reasons.append(f"scenario={text_of(first_present(summary.get('scenarioResult'), 'missing'))}")
    if summary.get("focusStable") is not True:
        reasons.append("focus_unstable")
    if summary.get("scrollStable", True) is not True:
        reasons.append("scroll_unstable")
    if summary.get("phaseValid", True) is not True:
        reasons.append("phase_invalid")
    if summary.get("archiveSucceeded", True) is not True:
        reasons.append("archive_failed")
    return reasons
