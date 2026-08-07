#!/usr/bin/env python3
"""Task-clustered paired analysis of benchmark variants.

Runs join on ``(task, model, repetition)``, but inference treats the task as
the sampling unit: repetitions are averaged within each task before the
fixed-seed paired bootstrap.
"""

import argparse
import json
import math
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import stats

BASELINE_MODE = "baseline"
TILTH_MODE = "tilth"


MODEL_ALIASES = {
    "haiku": "claude-haiku-4-5-20251001",
    "sonnet": "claude-sonnet-4-6",
    "opus": "claude-opus-4-6",
    "gpt5": "gpt-5-codex",
    "gpt-5.1-codex": "gpt-5-codex",
    "gpt-5-codex": "gpt-5-codex",
    "gpt5mini": "gpt-5-mini",
    "openrouter/openai/gpt-5-mini": "gpt-5-mini",
    "openai/gpt-5.1-codex": "gpt-5-codex",
    "openai/o3": "o3",
}


def _model_key(run: dict) -> object:
    model = run.get("model") or run.get("model_alias")
    return MODEL_ALIASES.get(model, model)


def load_runs(path: Path) -> list[dict]:
    """Load JSONL results, keeping error records (they pair as incorrect)."""
    runs = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                runs.append(json.loads(line))
    return runs


def _cost(run: dict):
    """Run cost, or None for error records (which carry no cost)."""
    cost = run.get("total_cost_usd")
    return float(cost) if isinstance(cost, (int, float)) else None


def pair_modes(
    runs: list[dict],
    experiment_mode: str,
    baseline_mode: str,
) -> dict[tuple[str, str], list[tuple]]:
    """Join two modes on ``(task, model, repetition)``."""
    by_key: dict[tuple, dict] = {}
    selected = {baseline_mode, experiment_mode}
    for run in runs:
        task = run.get("task")
        model = _model_key(run)
        mode = run.get("mode")
        rep = run.get("repetition")
        if None in (task, model, mode, rep) or mode not in selected:
            continue
        by_key[(task, model, mode, rep)] = run

    triples = {(task, model, rep) for task, model, _mode, rep in by_key}
    pairs: dict[tuple[str, str], list[tuple]] = defaultdict(list)
    for task, model, rep in sorted(triples):
        baseline = by_key.get((task, model, baseline_mode, rep))
        experiment = by_key.get((task, model, experiment_mode, rep))
        if baseline is None or experiment is None:
            continue
        pairs[(task, model)].append((
            rep,
            bool(baseline.get("correct", False)),
            bool(experiment.get("correct", False)),
            _cost(baseline),
            _cost(experiment),
        ))
    return dict(pairs)


def pair_ab(runs: list[dict]) -> dict[tuple[str, str], list[tuple]]:
    """Backward-compatible baseline/tilth pairing."""
    return pair_modes(runs, TILTH_MODE, BASELINE_MODE)


def _cpc(runs: list[dict]) -> float:
    """Return total reported cost per correct result, or infinity."""
    costed = [run for run in runs if _cost(run) is not None]
    correct = sum(1 for run in costed if run.get("correct", False))
    if not costed or correct == 0:
        return float("inf")
    return sum(_cost(run) or 0.0 for run in costed) / correct


def _matched_runs_by_task(
    runs: list[dict],
    experiment_mode: str,
    baseline_mode: str,
) -> dict[object, dict[str, list[dict]]]:
    by_key: dict[tuple[object, object, object], dict[str, dict]] = defaultdict(dict)
    for run in runs:
        mode = run.get("mode")
        task = run.get("task")
        model = _model_key(run)
        repetition = run.get("repetition")
        if (
            mode not in {baseline_mode, experiment_mode}
            or task is None
            or model is None
            or repetition is None
        ):
            continue
        by_key[(task, model, repetition)][mode] = run

    by_task: dict[object, dict[str, list[dict]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for (task, _model, _repetition), modes in by_key.items():
        baseline = modes.get(baseline_mode)
        experiment = modes.get(experiment_mode)
        if baseline is None or experiment is None:
            continue
        by_task[task][baseline_mode].append(baseline)
        by_task[task][experiment_mode].append(experiment)
    return by_task


def paired_accuracy_delta(
    runs: list[dict],
    experiment_mode: str,
    baseline_mode: str = BASELINE_MODE,
) -> tuple[float | None, float | None, float | None, int]:
    """Return a task-weighted accuracy delta and paired bootstrap interval."""
    by_task = _matched_runs_by_task(runs, experiment_mode, baseline_mode)
    deltas = []
    for task in sorted(by_task, key=str):
        baseline = by_task[task][baseline_mode]
        experiment = by_task[task][experiment_mode]
        baseline_rate = sum(bool(run.get("correct", False)) for run in baseline) / len(baseline)
        experiment_rate = sum(bool(run.get("correct", False)) for run in experiment) / len(experiment)
        deltas.append(experiment_rate - baseline_rate)
    if not deltas:
        return (None, None, None, 0)
    point = sum(deltas) / len(deltas)
    lo, hi = stats.paired_bootstrap_ci(deltas, n_resamples=10_000, seed=0)
    return (point, lo, hi, len(deltas))


def _task_cpc_percentage_deltas(
    by_task: dict[object, dict[str, list[dict]]],
    experiment_mode: str,
    baseline_mode: str,
) -> list[float]:
    deltas: list[float] = []
    for task in sorted(by_task, key=str):
        baseline_cpc = _cpc(by_task[task][baseline_mode])
        experiment_cpc = _cpc(by_task[task][experiment_mode])
        if (
            baseline_cpc == 0
            or not math.isfinite(baseline_cpc)
            or not math.isfinite(experiment_cpc)
        ):
            continue
        deltas.append((experiment_cpc - baseline_cpc) / baseline_cpc)
    return deltas


def paired_cpc_percentage_deltas(
    runs: list[dict],
    experiment_mode: str,
    baseline_mode: str = BASELINE_MODE,
) -> list[float]:
    """Return finite per-task CPC deltas from matched mode repetitions."""
    by_task = _matched_runs_by_task(runs, experiment_mode, baseline_mode)
    return _task_cpc_percentage_deltas(
        by_task,
        experiment_mode,
        baseline_mode,
    )


def paired_cpc_delta(
    runs: list[dict],
    experiment_mode: str,
    baseline_mode: str = BASELINE_MODE,
) -> tuple[float | None, float | None, float | None]:
    """Return aggregate CPC delta and a fixed-seed paired percentile CI.

    Values are fractions (``0.25`` means +25%).  A missing or zero-correct
    baseline returns ``(None, None, None)`` so callers never divide infinity.
    """
    by_task = _matched_runs_by_task(runs, experiment_mode, baseline_mode)
    baseline_runs = [
        run
        for modes in by_task.values()
        for run in modes[baseline_mode]
    ]
    experiment_runs = [
        run
        for modes in by_task.values()
        for run in modes[experiment_mode]
    ]
    baseline_cpc = _cpc(baseline_runs)
    if not baseline_runs or not math.isfinite(baseline_cpc):
        return (None, None, None)
    if baseline_cpc == 0:
        return (float("inf"), float("inf"), float("inf"))
    experiment_cpc = _cpc(experiment_runs)
    aggregate_delta = (experiment_cpc - baseline_cpc) / baseline_cpc
    if not math.isfinite(experiment_cpc):
        return (aggregate_delta, float("inf"), float("inf"))

    deltas = _task_cpc_percentage_deltas(
        by_task,
        experiment_mode,
        baseline_mode,
    )
    if not deltas:
        return (aggregate_delta, aggregate_delta, aggregate_delta)
    lo, hi = stats.paired_bootstrap_ci(deltas, n_resamples=10_000, seed=0)
    return (aggregate_delta, lo, hi)


def paired_report(
    runs: list[dict],
    experiment_mode: str,
    baseline_mode: str,
) -> None:
    """Print task-clustered paired accuracy and CPC differences by model."""
    print("=" * 72)
    print(
        f"PAIRED VARIANTS ({experiment_mode} vs {baseline_mode}; "
        "task is the sampling unit)"
    )
    print("=" * 72)

    models = sorted({_model_key(run) for run in runs if _model_key(run) is not None})
    found = False
    for model in models:
        model_runs = [run for run in runs if _model_key(run) == model]
        delta, lo, hi, n_tasks = paired_accuracy_delta(
            model_runs,
            experiment_mode,
            baseline_mode,
        )
        if delta is None:
            continue
        found = True
        significance = "excludes 0" if lo > 0 or hi < 0 else "includes 0"
        cpc_delta, cpc_lo, cpc_hi = paired_cpc_delta(
            model_runs,
            experiment_mode,
            baseline_mode,
        )
        print(f"\n## model: {model}")
        print(f"  paired tasks:       {n_tasks}")
        print(
            f"  accuracy Δ:         {delta * 100:+.1f}pp  "
            f"95% task bootstrap CI [{lo * 100:+.1f}, {hi * 100:+.1f}] "
            f"({significance})"
        )
        if cpc_delta is None:
            print("  cost/correct Δ:     n/a")
        else:
            print(
                f"  cost/correct Δ:     {cpc_delta * 100:+.1f}%  "
                f"95% task bootstrap CI [{cpc_lo * 100:+.1f}, {cpc_hi * 100:+.1f}]"
            )
    if not found:
        print(
            f"\nNo paired runs found for {experiment_mode} vs {baseline_mode} "
            "on the same task/model/repetition."
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results_file", type=Path, help="JSONL results from run.py")
    parser.add_argument("--model", help="Restrict to a model alias or configured model ID")
    parser.add_argument("--experiment-mode", default=TILTH_MODE)
    parser.add_argument("--baseline-mode", default=BASELINE_MODE)
    args = parser.parse_args()

    if not args.results_file.exists():
        parser.error(f"file not found: {args.results_file}")
    runs = load_runs(args.results_file)
    if args.model:
        target_model = MODEL_ALIASES.get(args.model, args.model)
        runs = [run for run in runs if _model_key(run) == target_model]
    paired_report(runs, args.experiment_mode, args.baseline_mode)


if __name__ == "__main__":
    main()
