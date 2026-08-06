"""Pure evidence flags derived from parsed benchmark result records.

Initial thresholds are intentionally explicit and tunable: token snowball
requires at least 10% growth on every adjacent turn, talkative failures and
tool storms must exceed the 95th percentile of the supplied records, budget
exhaustion reaches the configured per-run budget, and timeout records contain
``"timeout"`` in their error text.
"""

from __future__ import annotations

import math
from numbers import Real
from typing import Any, Iterable

from config import DEFAULT_MAX_BUDGET_USD


TOKEN_SNOWBALL = "token_snowball"
TALKATIVE_FAILURE = "talkative_failure"
TOOL_STORM = "tool_storm"
BUDGET_EXHAUSTED = "budget_exhausted"
TIMEOUT = "timeout"

TOKEN_SNOWBALL_RATIO = 1.10
TALKATIVE_FAILURE_PERCENTILE = 95.0
TOOL_STORM_PERCENTILE = 95.0


def _number(value: Any) -> float | None:
    if isinstance(value, Real) and not isinstance(value, bool):
        return float(value)
    return None


def _key(record: dict[str, Any]) -> tuple[Any, Any, Any]:
    model = record.get("model") or record.get("model_alias")
    return record.get("task"), model, record.get("repetition")


def _flag(
    name: str,
    record: dict[str, Any],
    evidence: str,
    mitigation: str,
) -> dict[str, Any]:
    task, model, repetition = _key(record)
    return {
        "flag": name,
        "kind": name,
        "task": task,
        "model": model,
        "repetition": repetition,
        "modes": (str(record.get("mode", "")),),
        "evidence": evidence,
        "mitigation": mitigation,
    }


def _percentile(values: Iterable[float], percentile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    rank = (len(ordered) - 1) * percentile / 100
    lower = math.floor(rank)
    upper = math.ceil(rank)
    if lower == upper:
        return ordered[lower]
    weight = rank - lower
    return ordered[lower] + (ordered[upper] - ordered[lower]) * weight


def token_snowball_flags(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    flags: list[dict[str, Any]] = []
    for record in records:
        values = record.get("per_turn_output_tokens")
        if not isinstance(values, (list, tuple)) or len(values) < 2:
            continue
        tokens = [_number(value) for value in values]
        if any(value is None for value in tokens):
            continue
        numeric = [value for value in tokens if value is not None]
        if any(old <= 0 or new <= old or new < old * TOKEN_SNOWBALL_RATIO
               for old, new in zip(numeric, numeric[1:])):
            continue
        growth = ", ".join(
            f"{(new / old - 1) * 100:.1f}%"
            for old, new in zip(numeric, numeric[1:])
        )
        flags.append(_flag(
            TOKEN_SNOWBALL,
            record,
            f"per_turn_output_tokens={list(values)!r} grew by at least 10% each turn ({growth})",
            "Cap turns/output tokens and inspect the turn that triggered the growth.",
        ))
    return flags


def talkative_failure_flags(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    materialized = list(records)
    outputs = [
        value
        for value in (_number(record.get("output_tokens")) for record in materialized)
        if value is not None
    ]
    threshold = _percentile(outputs, TALKATIVE_FAILURE_PERCENTILE)
    flags: list[dict[str, Any]] = []
    for record in materialized:
        output = _number(record.get("output_tokens"))
        if record.get("correct", False) or output is None or output <= threshold:
            continue
        flags.append(_flag(
            TALKATIVE_FAILURE,
            record,
            f"incorrect run emitted {output:g} output tokens, above the "
            f"{TALKATIVE_FAILURE_PERCENTILE:g}th percentile threshold {threshold:g}",
            "Stop after a failed answer, cap output tokens, and tighten the task prompt.",
        ))
    return flags


def _tool_count(record: dict[str, Any]) -> float | None:
    direct = _number(record.get("num_tool_calls"))
    if direct is not None:
        return direct
    calls = record.get("tool_calls")
    if isinstance(calls, dict):
        values = [_number(value) for value in calls.values()]
        return sum(value for value in values if value is not None)
    if isinstance(calls, (list, tuple)):
        return float(len(calls))
    return None


def tool_storm_flags(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    materialized = list(records)
    counts = [
        value
        for value in (_tool_count(record) for record in materialized)
        if value is not None
    ]
    threshold = _percentile(counts, TOOL_STORM_PERCENTILE)
    flags: list[dict[str, Any]] = []
    for record in materialized:
        count = _tool_count(record)
        if record.get("correct", False) or count is None or count <= threshold:
            continue
        flags.append(_flag(
            TOOL_STORM,
            record,
            f"incorrect run made {count:g} tool calls, above the "
            f"{TOOL_STORM_PERCENTILE:g}th percentile threshold {threshold:g}",
            "Bound tool calls and inspect the repeated tool loop before rerunning.",
        ))
    return flags


def budget_exhausted_flags(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    flags: list[dict[str, Any]] = []
    for record in records:
        cost = _number(record.get("total_cost_usd"))
        if cost is None or cost < DEFAULT_MAX_BUDGET_USD:
            continue
        flags.append(_flag(
            BUDGET_EXHAUSTED,
            record,
            f"total_cost_usd=${cost:.6f} reached the ${DEFAULT_MAX_BUDGET_USD:.2f} budget cap",
            "Lower the per-run budget and stop/review before another expensive retry.",
        ))
    return flags


def timeout_flags(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    flags: list[dict[str, Any]] = []
    for record in records:
        error = str(record.get("error", "")).casefold()
        if "timeout" not in error:
            continue
        flags.append(_flag(
            TIMEOUT,
            record,
            f"run error={record.get('error')!r} indicates a timeout",
            "Inspect the slowest tool/turn and reduce the work or raise the execution timeout deliberately.",
        ))
    return flags


def detect_flags(records: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    """Return deterministic per-run evidence flags without mutating records."""
    materialized = [record for record in records if isinstance(record, dict)]
    flags = (
        token_snowball_flags(materialized)
        + talkative_failure_flags(materialized)
        + tool_storm_flags(materialized)
        + budget_exhausted_flags(materialized)
        + timeout_flags(materialized)
    )
    return sorted(
        flags,
        key=lambda flag: (
            flag["flag"],
            repr(flag["task"]),
            repr(flag["model"]),
            repr(flag["repetition"]),
        ),
    )
