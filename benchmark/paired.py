#!/usr/bin/env python3
"""Paired A/B analysis of benchmark results.

The benchmark runs baseline (grep/cat/find) and tilth (MCP tools) on the SAME
tasks, so the two arms are paired: each (task, model, repetition) yields one
baseline run and one tilth run. This module joins them on that key, runs an
exact McNemar test on the correctness discordances, and bootstraps the paired
cost delta.

    python benchmark/paired.py <results.jsonl> [--model MODEL]
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


def pair_ab(runs: list[dict]) -> dict[tuple[str, str], list[tuple]]:
    """Join baseline vs tilth runs on (task, model, repetition).

    Returns {(task, model): [(rep, base_correct, tilth_correct, base_cost,
    tilth_cost)]}. Error records count as incorrect with cost None, so they pair
    cleanly for McNemar but drop out of the cost-delta CI (a crashed run has no
    measurable cost). A pair is emitted only when BOTH arms have a run for that
    (task, model, rep).
    """
    by_key: dict[tuple, dict] = {}
    for run in runs:
        task = run.get("task")
        model = _model_key(run)
        mode = run.get("mode")
        rep = run.get("repetition")
        if None in (task, model, mode, rep) or mode not in (BASELINE_MODE, TILTH_MODE):
            continue
        by_key[(task, model, mode, rep)] = run

    triples = {(task, model, rep) for (task, model, _mode, rep) in by_key}
    pairs: dict[tuple[str, str], list[tuple]] = defaultdict(list)
    for task, model, rep in sorted(triples):
        base = by_key.get((task, model, BASELINE_MODE, rep))
        tilth = by_key.get((task, model, TILTH_MODE, rep))
        if base is None or tilth is None:
            continue
        pairs[(task, model)].append((
            rep,
            bool(base.get("correct", False)),
            bool(tilth.get("correct", False)),
            _cost(base),
            _cost(tilth),
        ))
    return dict(pairs)


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


def paired_report(pairs: dict[tuple[str, str], list[tuple]]) -> None:
    """Print, per model: discordant b/c, exact McNemar p, accuracy delta, and a
    paired cost-delta bootstrap CI."""
    print("=" * 72)
    print("PAIRED A/B  (baseline vs tilth, joined on task+model+repetition)")
    print("=" * 72)

    if not pairs:
        print("\nNo paired runs found (need both baseline and tilth on the same task/rep).")
        return

    print("McNemar uses the per-rep join, so p can be anti-conservative under rep correlation;")
    print("the accuracy delta and MDE elsewhere treat the task as the sampling unit.")
    by_model: dict[str, list[tuple]] = defaultdict(list)
    for (_task, model), tuples in pairs.items():
        by_model[model].extend(tuples)

    for model in sorted(by_model):
        tuples = by_model[model]
        n = len(tuples)
        base_correct = sum(1 for (_r, bc, _tc, _bk, _tk) in tuples if bc)
        tilth_correct = sum(1 for (_r, _bc, tc, _bk, _tk) in tuples if tc)
        b = sum(1 for (_r, bc, tc, _bk, _tk) in tuples if bc and not tc)
        c = sum(1 for (_r, bc, tc, _bk, _tk) in tuples if tc and not bc)
        p_value, direction = stats.mcnemar_exact(b, c)
        base_acc = base_correct / n * 100
        tilth_acc = tilth_correct / n * 100

        cost_deltas = [
            tk - bk
            for (_r, _bc, _tc, bk, tk) in tuples
            if bk is not None and tk is not None
        ]
        dropped = n - len(cost_deltas)

        print(f"\n## model: {model}")
        print(f"  paired reps:        {n}")
        print(f"  accuracy:           baseline {base_acc:.0f}%  ->  tilth {tilth_acc:.0f}%  (Δ {tilth_acc - base_acc:+.0f}pp)")
        print(f"  discordant pairs:   b={b} (baseline-only correct)  c={c} (tilth-only correct)")
        verdict = "significant" if p_value < 0.05 else "not significant"
        favors = "" if direction == "tie" else f", favors {direction}"
        print(f"  McNemar exact p:    {p_value:.4f}  ({verdict} at α=0.05{favors})")

        if cost_deltas:
            mean_delta = sum(cost_deltas) / len(cost_deltas)
            lo, hi = stats.paired_bootstrap_ci(cost_deltas)
            excludes_zero = lo > 0 or hi < 0
            sig = "significant" if excludes_zero else "includes 0"
            print(f"  cost Δ/rep (tilth-baseline): ${mean_delta:+.4f}  95% CI [${lo:+.4f}, ${hi:+.4f}] ({sig})")
        else:
            print("  cost Δ/rep: n/a (no rep had cost on both arms)")
        if dropped:
            print(f"  note: {dropped} rep(s) excluded from cost CI (error/missing cost; still counted in McNemar)")


def main() -> None:
    parser = argparse.ArgumentParser(description="Paired A/B analysis of benchmark results")
    parser.add_argument("results_file", type=Path, help="Path to JSONL results file from run.py")
    parser.add_argument("--model", help="Restrict to a model alias or configured model ID")
    args = parser.parse_args()

    if not args.results_file.exists():
        print(f"ERROR: File not found: {args.results_file}", file=sys.stderr)
        sys.exit(1)

    runs = load_runs(args.results_file)
    if args.model:
        target_model = MODEL_ALIASES.get(args.model, args.model)
        runs = [r for r in runs if _model_key(r) == target_model]

    paired_report(pair_ab(runs))


if __name__ == "__main__":
    main()
