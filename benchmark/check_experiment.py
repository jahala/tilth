#!/usr/bin/env python3
"""Validate a three-arm benchmark result before analysis."""

import argparse
import json
import re
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

from variants import Experiment, Variant, load_experiment

_SHA256_RE = re.compile(r"[0-9a-f]{64}")
_BLOCK_FIELDS = ("task", "model_alias", "repetition")


@dataclass(frozen=True)
class ExperimentRunSummary:
    rows: int
    blocks: int


def _load_rows(path: Path) -> list[dict]:
    try:
        lines = path.read_text().splitlines()
    except OSError as error:
        raise ValueError(f"cannot read benchmark results {path}: {error}") from error

    rows: list[dict] = []
    for line_number, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(
                f"benchmark results {path} line {line_number} is not valid JSON: {error}"
            ) from error
        if not isinstance(row, dict):
            raise ValueError(
                f"benchmark results {path} line {line_number} must be a JSON object"
            )
        rows.append(row)
    if not rows:
        raise ValueError(f"benchmark results {path} contain no rows")
    return rows


def _expected_variant_metadata(variant: Variant) -> dict:
    return {
        "label": variant.name,
        "repository": variant.repository,
        "git_ref": variant.git_ref,
        "git_sha": variant.git_sha,
        "binary_path": str(variant.binary_path) if variant.binary_path else None,
    }


def _validate_variant_metadata(row: dict, variant: Variant, row_number: int) -> None:
    metadata = row.get("variant")
    expected = _expected_variant_metadata(variant)
    if not isinstance(metadata, dict) or any(
        metadata.get(key) != value for key, value in expected.items()
    ):
        raise ValueError(
            f"row {row_number} variant metadata does not match manifest for {variant.name}"
        )

    binary_sha256 = metadata.get("binary_sha256")
    if variant.binary_path is None:
        if binary_sha256 is not None:
            raise ValueError(
                f"row {row_number} no_tilth variant must not have a binary SHA-256"
            )
    elif not isinstance(binary_sha256, str) or not _SHA256_RE.fullmatch(binary_sha256):
        raise ValueError(
            f"row {row_number} variant {variant.name} lacks a valid binary SHA-256"
        )

    # A tilth-armed row that recorded its session tool list must actually have
    # had tilth tools; otherwise the cell ran native-only and the arm
    # comparison is invalid (rows without the field predate recording).
    available_tools = row.get("available_tools")
    if (
        variant.binary_path is not None
        and isinstance(available_tools, list)
        and not any(str(name).startswith("mcp__tilth__") for name in available_tools)
    ):
        raise ValueError(
            f"row {row_number} variant {variant.name} recorded no mcp__tilth__ "
            "tools in its session; the cell ran native-only"
        )


def _block_key(row: dict, row_number: int) -> tuple[object, ...]:
    missing = [field for field in _BLOCK_FIELDS if field not in row]
    if missing:
        raise ValueError(
            f"row {row_number} is missing block fields: {', '.join(missing)}"
        )
    return tuple(row[field] for field in _BLOCK_FIELDS)


def _validate_block(
    key: tuple[object, ...],
    rows: list[tuple[int, dict]],
    experiment: Experiment,
) -> None:
    expected_arms = {variant.name for variant in experiment.variants}
    modes = [row.get("mode") for _, row in rows]
    if len(rows) != len(expected_arms) or set(modes) != expected_arms:
        raise ValueError(
            f"block {key} does not contain each experiment arm exactly once"
        )

    declared_order = rows[0][1].get("arm_order")
    if (
        not isinstance(declared_order, list)
        or len(declared_order) != len(expected_arms)
        or set(declared_order) != expected_arms
    ):
        raise ValueError(f"block {key} has an invalid arm order")

    for row_number, row in rows:
        if row.get("arm_order") != declared_order:
            raise ValueError(f"block {key} has inconsistent arm orders")
        arm_index = row.get("arm_order_index")
        if (
            not isinstance(arm_index, int)
            or isinstance(arm_index, bool)
            or not 0 <= arm_index < len(declared_order)
            or declared_order[arm_index] != row["mode"]
        ):
            raise ValueError(
                f"row {row_number} arm order index does not match its mode"
            )


def _expected_block_keys(
    tasks: list[str],
    models: list[str],
    repetitions: int,
) -> set[tuple[object, ...]]:
    if not tasks or not models:
        raise ValueError("expected tasks and models must not be empty")
    if repetitions < 1:
        raise ValueError("expected repetitions must be at least 1")
    return {
        (task, model, repetition)
        for task in tasks
        for model in models
        for repetition in range(repetitions)
    }


def validate_experiment_run(
    result_path: Path,
    manifest_path: Path,
    *,
    expected_tasks: list[str] | None = None,
    expected_models: list[str] | None = None,
    expected_repetitions: int | None = None,
) -> ExperimentRunSummary:
    """Reject incomplete, errored, or mismatched three-arm result files."""
    schedule_parts = (
        expected_tasks,
        expected_models,
        expected_repetitions,
    )
    if any(part is not None for part in schedule_parts) and any(
        part is None for part in schedule_parts
    ):
        raise ValueError(
            "expected tasks, models, and repetitions must be provided together"
        )
    experiment = load_experiment(manifest_path)
    variants = {variant.name: variant for variant in experiment.variants}
    rows = _load_rows(result_path)

    error_rows = [row for row in rows if "error" in row]
    if error_rows:
        suffix = "" if len(error_rows) == 1 else "s"
        raise ValueError(
            f"benchmark results {result_path} contains {len(error_rows)} error row{suffix}"
        )

    blocks: dict[tuple[object, ...], list[tuple[int, dict]]] = defaultdict(list)
    for row_number, row in enumerate(rows, start=1):
        mode = row.get("mode")
        variant = variants.get(mode)
        if variant is None:
            raise ValueError(f"row {row_number} has unknown experiment arm: {mode!r}")
        if row.get("arm_order_seed") != experiment.arm_order_seed:
            raise ValueError(f"row {row_number} arm order seed does not match manifest")
        _validate_variant_metadata(row, variant, row_number)
        blocks[_block_key(row, row_number)].append((row_number, row))

    for key, block_rows in blocks.items():
        _validate_block(key, block_rows, experiment)

    if expected_tasks is not None:
        expected_keys = _expected_block_keys(
            expected_tasks,
            expected_models or [],
            expected_repetitions or 0,
        )
        actual_keys = set(blocks)
        if actual_keys != expected_keys:
            missing = len(expected_keys - actual_keys)
            unexpected = len(actual_keys - expected_keys)
            raise ValueError(
                "result blocks do not match expected schedule: "
                f"{missing} missing, {unexpected} unexpected"
            )

    return ExperimentRunSummary(rows=len(rows), blocks=len(blocks))


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Validate three-arm benchmark results before analysis."
    )
    parser.add_argument("result", type=Path, help="Benchmark JSONL result path")
    parser.add_argument("manifest", type=Path, help="Experiment manifest path")
    parser.add_argument("--tasks", help="Expected comma-separated task names")
    parser.add_argument("--models", help="Expected comma-separated model aliases")
    parser.add_argument("--reps", type=int, help="Expected repetitions")
    return parser


def main() -> int:
    parser = _parser()
    args = parser.parse_args()
    schedule = (args.tasks, args.models, args.reps)
    if any(value is not None for value in schedule) and any(
        value is None for value in schedule
    ):
        parser.error("--tasks, --models, and --reps must be provided together")
    try:
        summary = validate_experiment_run(
            args.result,
            args.manifest,
            expected_tasks=args.tasks.split(",") if args.tasks else None,
            expected_models=args.models.split(",") if args.models else None,
            expected_repetitions=args.reps,
        )
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"Validated {summary.rows} rows across {summary.blocks} matched blocks")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
