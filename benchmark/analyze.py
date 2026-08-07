#!/usr/bin/env python3
"""
Benchmark analysis and report generation.

Reads JSONL results from run.py and generates a markdown report
with context efficiency metrics and comparisons.
"""

import argparse
import json
import math
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from statistics import mean, median, stdev

sys.path.insert(0, str(Path(__file__).parent))

import stats
from flags import detect_flags
from paired import (
    pair_modes,
    paired_accuracy_delta,
    paired_cpc_delta as paired_cpc_delta_impl,
)
from pricing import PRICING_DATA, compute_cost_breakdown, pricing_staleness_warning
from tasks import TASKS


def format_cost_breakdown(costs: dict[str, float], indent: str = "  ") -> str:
    """Format cost breakdown as single line."""
    parts = [
        f"cache_create=${costs['cache_creation_cost']:.3f}",
        f"cache_read=${costs['cache_read_cost']:.3f}",
        f"output=${costs['output_cost']:.3f}",
        f"input=${costs['input_cost']:.3f}",
    ]
    return f"{indent}{' '.join(parts)}"


def format_cost_delta(baseline_costs: dict[str, float], tilth_costs: dict[str, float], indent: str = "  ") -> str:
    """Format cost delta breakdown."""
    deltas = {
        "cache_creation": tilth_costs['cache_creation_cost'] - baseline_costs['cache_creation_cost'],
        "cache_read": tilth_costs['cache_read_cost'] - baseline_costs['cache_read_cost'],
        "output": tilth_costs['output_cost'] - baseline_costs['output_cost'],
        "input": tilth_costs['input_cost'] - baseline_costs['input_cost'],
    }
    parts = [
        f"Δcache_create={'+' if deltas['cache_creation'] >= 0 else ''}${deltas['cache_creation']:.3f}",
        f"Δcache_read={'+' if deltas['cache_read'] >= 0 else ''}${deltas['cache_read']:.3f}",
        f"Δoutput={'+' if deltas['output'] >= 0 else ''}${deltas['output']:.3f}",
        f"Δinput={'+' if deltas['input'] >= 0 else ''}${deltas['input']:.3f}",
    ]
    return f"{indent}{' '.join(parts)}"


def load_results(path: Path) -> list[dict]:
    """Load JSONL results file."""
    results = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                results.append(json.loads(line))
    return results


def group_by(results: list[dict], *keys: str) -> dict:
    """Group results by specified keys."""
    groups = defaultdict(list)
    for result in results:
        # Skip error entries that don't have all required fields
        if "error" in result:
            continue
        key = tuple(result.get(k) for k in keys)
        groups[key].append(result)
    return dict(groups)


def compute_stats(values: list) -> dict:
    """Compute statistics for a list of values."""
    if not values:
        return {
            "median": 0,
            "mean": 0,
            "stdev": 0,
            "min": 0,
            "max": 0,
        }

    return {
        "median": median(values),
        "mean": mean(values),
        "stdev": stdev(values) if len(values) > 1 else 0,
        "min": min(values),
        "max": max(values),
    }


def ascii_sparkline(values: list[int]) -> str:
    """Generate ASCII sparkline from values."""
    if not values:
        return ""

    if max(values) == min(values):
        return "▄" * len(values)

    chars = " ▁▂▃▄▅▆▇█"
    lo, hi = min(values), max(values)
    return "".join(
        chars[min(int((v - lo) / (hi - lo) * 8), 8)]
        for v in values
    )


def format_delta(baseline_val: float, tilth_val: float) -> str:
    """Format delta as percentage change."""
    if baseline_val == 0:
        return "—"
    pct_change = ((tilth_val - baseline_val) / baseline_val) * 100
    sign = "+" if pct_change > 0 else ""
    return f"{sign}{pct_change:.0f}%"


MODE_ORDER = ["no_tilth", "upstream", "fork", "baseline", "tilth", "tilth_forced"]
MODE_LABELS = {
    "no_tilth": "no_tilth",
    "upstream": "upstream",
    "fork": "fork",
    "baseline": "baseline",
    "tilth": "tilth-added",
    "tilth_forced": "tilth-only",
}


def ordered_modes(mode_names: set[str]) -> list[str]:
    """Return benchmark modes in report order, with unknown modes last."""
    known = [mode for mode in MODE_ORDER if mode in mode_names]
    unknown = sorted(mode_names - set(MODE_ORDER))
    return known + unknown


def mode_label(mode_name: str) -> str:
    return MODE_LABELS.get(mode_name, mode_name)


def reference_mode(modes: list[str]) -> str | None:
    """Return the descriptive comparison arm for a report."""
    for candidate in ("no_tilth", "baseline"):
        if candidate in modes:
            return candidate
    return modes[0] if modes else None


def comparison_pairs(modes: list[str]) -> list[tuple[str, str]]:
    """Return primary and guardrail comparisons in report order."""
    present = set(modes)
    three_way = [
        ("fork", "upstream"),
        ("fork", "no_tilth"),
        ("upstream", "no_tilth"),
    ]
    selected = [pair for pair in three_way if set(pair) <= present]
    if selected:
        return selected
    reference = reference_mode(modes)
    if reference is None:
        return []
    return [(mode, reference) for mode in modes if mode != reference]


def format_metric_value(key: str, value: float) -> str:
    if key == "total_cost_usd":
        return f"${value:.4f}"
    return f"{value:.0f}"


def correctness_pct(runs: list[dict]) -> float:
    if not runs:
        return 0.0
    return (sum(1 for r in runs if r.get("correct", False)) / len(runs)) * 100


def correctness_with_ci(runs: list[dict]) -> tuple[float, float, float]:
    """Accuracy with a Wilson 95% CI, all in 0-100."""
    if not runs:
        return (0.0, 0.0, 0.0)
    successes = sum(1 for r in runs if r.get("correct", False))
    lo, hi = stats.wilson_interval(successes, len(runs))
    return (successes / len(runs) * 100, lo * 100, hi * 100)


def _fmt_usd(value: float) -> str:
    return "∞" if value == float("inf") else f"${value:.4f}"


def cost_per_correct(runs: list[dict]) -> tuple[float, float, float]:
    """Cost per correct answer (total cost / correct count) with a bootstrap CI.

    The expected cost under retry (the README metric). The CI is a ratio-of-sums
    bootstrap (sum(cost)/sum(correct)) via stats.ratio_bootstrap_ci. value is inf
    when no run is correct.
    """
    if not runs:
        return (float("inf"), float("inf"), float("inf"))
    costs = [float(r.get("total_cost_usd", 0.0)) for r in runs]
    correct = [1.0 if r.get("correct") else 0.0 for r in runs]
    total_correct = sum(correct)
    if total_correct == 0:
        return (float("inf"), float("inf"), float("inf"))
    value = sum(costs) / total_correct
    lo, hi = stats.ratio_bootstrap_ci(costs, correct)
    return (value, lo, hi)


def find_median_run(runs: list[dict], metric: str) -> dict:
    """Find the run with median value for given metric."""
    if not runs:
        return {}
    sorted_runs = sorted(runs, key=lambda r: r.get(metric, 0))
    return sorted_runs[len(sorted_runs) // 2]


def merge_tool_calls(runs: list[dict]) -> dict[str, float]:
    """Merge tool_calls dicts from multiple runs and compute median counts."""
    # Collect all tool names
    all_tools = set()
    for run in runs:
        if "tool_calls" in run:
            all_tools.update(run["tool_calls"].keys())

    # Compute median count for each tool
    result = {}
    for tool in all_tools:
        counts = [run.get("tool_calls", {}).get(tool, 0) for run in runs]
        result[tool] = median(counts)

    return result

CAPABILITIES = ("locate", "trace", "fix", "debug", "control")


def capability_for_record(record: dict) -> str:
    """Resolve recorded capability, falling back to the current task registry."""
    capability = record.get("capability")
    if capability in CAPABILITIES:
        return capability
    task = TASKS.get(record.get("task"))
    registry_capability = getattr(task, "capability", "") if task else ""
    return registry_capability if registry_capability in CAPABILITIES else "unknown"


def _fraction_text(value: float | None) -> str:
    if value is None:
        return "n/a (baseline 0-correct)"
    if math.isinf(value):
        return "∞"
    return f"{value * 100:+.0f}%"


def paired_cpc_delta(
    results: list[dict],
    experiment_mode: str,
    baseline_mode: str = "baseline",
) -> tuple[float | None, float | None, float | None]:
    """Expose the headline CPC delta as a fraction and paired 95% CI."""
    return paired_cpc_delta_impl(results, experiment_mode, baseline_mode)


def _headline_section(results: list[dict], modes: list[str]) -> list[str]:
    reference = reference_mode(modes)
    reference_runs = [
        run for run in results if run.get("mode") == reference
    ]
    reference_cpc = cost_per_correct(reference_runs)[0]
    lines = [
        "## Headline",
        "",
        f"Cost-per-correct delta versus {reference or 'the reference arm'}, with a "
        "fixed-seed 10,000-resample paired percentile 95% CI over per-task "
        "percentage deltas.",
        "",
        "| Experiment | Reference CPC | Experiment CPC | Delta | Paired 95% CI |",
        "|---|---:|---:|---:|---:|",
    ]
    experiments = [mode for mode in modes if mode != reference]
    if reference is None or not experiments:
        lines.extend(["| — | — | — | — | — |", ""])
        return lines
    for mode in experiments:
        experiment = [run for run in results if run.get("mode") == mode]
        delta, lo, hi = paired_cpc_delta(results, mode, reference)
        if delta is None:
            lines.append(
                f"| {mode_label(mode)} | n/a | {_fmt_usd(cost_per_correct(experiment)[0])} | "
                "n/a (baseline 0-correct) | n/a (baseline 0-correct) |"
            )
            continue
        lines.append(
            f"| {mode_label(mode)} | {_fmt_usd(reference_cpc)} | "
            f"{_fmt_usd(cost_per_correct(experiment)[0])} | {_fraction_text(delta)} | "
            f"[{_fraction_text(lo)}, {_fraction_text(hi)}] |"
        )
    lines.append("")
    return lines


def _capability_section(results: list[dict], modes: list[str]) -> list[str]:
    capabilities = [capability for capability in CAPABILITIES if any(
        capability_for_record(run) == capability for run in results
    )]
    if not capabilities:
        return []
    lines = [
        "## Capability breakdown",
        "",
        "| Capability | Mode | Correctness | Cost/correct |",
        "|---|---|---:|---:|",
    ]
    for capability in capabilities:
        for mode in modes:
            runs = [
                run for run in results
                if run.get("mode") == mode and capability_for_record(run) == capability
            ]
            if not runs:
                continue
            lines.append(
                f"| {capability} | {mode_label(mode)} | {correctness_pct(runs):.0f}% | "
                f"{_fmt_usd(cost_per_correct(runs)[0])} |"
            )
    lines.append("")
    return lines


def _control_section(results: list[dict], modes: list[str]) -> list[str]:
    reference = reference_mode(modes)
    lines = [
        "## Control-task delta",
        "",
        f"Control tasks should show little tool advantage; deltas are relative to {reference}.",
        "",
    ]
    control = [run for run in results if capability_for_record(run) == "control"]
    reference_runs = [
        run for run in control if run.get("mode") == reference
    ]
    if not control or not reference_runs:
        lines.extend(["No paired control-task reference data.", ""])
        return lines
    reference_accuracy = correctness_pct(reference_runs)
    for mode in modes:
        if mode == reference:
            continue
        experiment = [run for run in control if run.get("mode") == mode]
        if not experiment:
            continue
        delta, _lo, _hi = paired_cpc_delta(control, mode, reference)
        lines.append(
            f"- **{mode_label(mode)} vs {reference}:** correctness "
            f"{correctness_pct(experiment) - reference_accuracy:+.0f}pp; "
            f"cost/correct {_fraction_text(delta)}"
        )
    lines.append("")
    return lines


def _flags_section(results: list[dict]) -> list[str]:
    flags = detect_flags(results)
    lines = ["## Evidence flags", ""]
    if not flags:
        lines.append("No evidence flags detected.")
        lines.append("")
        return lines
    counts: dict[str, int] = defaultdict(int)
    for flag in flags:
        for mode in flag["modes"]:
            counts[mode] += 1
    counts_text = ", ".join(
        f"{mode_label(mode)}={count}" for mode, count in sorted(counts.items())
    )
    lines.extend([f"Flag counts by mode: {counts_text}", ""])
    lines.extend([
        "| Flag | Task | Model | Rep | Evidence | Mitigation |",
        "|---|---|---|---:|---|---|",
    ])
    for flag in flags:
        evidence = str(flag["evidence"]).replace("|", "\\|")
        mitigation = str(flag["mitigation"]).replace("|", "\\|")
        lines.append(
            f"| {flag['flag']} | {flag['task']} | {flag['model']} | "
            f"{flag['repetition']} | {evidence} | {mitigation} |"
        )
    lines.append("")
    return lines


def generate_report(results: list[dict]) -> str:
    """Generate markdown report from results."""
    if not results:
        return "# Error\n\nNo valid results found in file.\n"

    # Filter out error entries
    valid_results = [r for r in results if "error" not in r]
    error_count = len(results) - len(valid_results)

    if not valid_results:
        lines = [f"# Error\n\nAll {len(results)} runs failed.", ""]
        lines.extend(_flags_section(results))
        return "\n".join(lines) + "\n"

    # Extract metadata
    models = sorted(set(r.get("model") or r.get("model_alias") or "unknown" for r in valid_results))
    tasks = sorted(set(r.get("task", "unknown") for r in valid_results))
    modes = ordered_modes(set(r.get("mode", "unknown") for r in valid_results))
    repos = sorted(set(r.get("repo", "synthetic") for r in valid_results))
    max_rep = max(int(r.get("repetition", 0)) for r in valid_results)
    num_reps = max_rep + 1

    run_summary = f"**Runs:** {len(valid_results)} valid"
    if error_count > 0:
        run_summary += f" ({error_count} errors)"

    lines = [
        "# tilth Benchmark Results",
        "",
        f"**Generated:** {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}",
        "",
        f"{run_summary} | **Models:** {', '.join(models)} | "
        f"**Repos:** {', '.join(repos)} | **Reps:** {num_reps}",
        "",
    ]
    warning = pricing_staleness_warning(PRICING_DATA["as_of"])
    if warning:
        lines.extend([warning, ""])
    lines.extend(_headline_section(valid_results, modes))
    lines.extend(_capability_section(valid_results, modes))
    lines.extend(_control_section(valid_results, modes))
    lines.extend(_flags_section(results))
    lines.extend([
        "## Context Efficiency",
        "",
        "The primary metric. Context tokens (input + cached) represent the actual context processed each turn. This compounds because each turn re-sends conversation history.",
        "",
        "### Per-task comparison",
        "",
    ])

    # Group by task
    task_groups = group_by(valid_results, "task")

    for task_name in tasks:
        task_results = task_groups.get((task_name,), [])
        if not task_results:
            continue

        lines.append(f"#### {task_name}")
        lines.append("")

        # Show repo for the task
        task_repo = task_results[0].get("repo", "synthetic") if task_results else "synthetic"
        if task_repo != "synthetic":
            lines.append(f"*Repo: {task_repo}*")
            lines.append("")

        # Group by mode and show every present mode side by side.
        mode_groups = group_by(task_results, "mode")
        present_modes = [mode for mode in modes if (mode,) in mode_groups]
        runs_by_mode = {mode: mode_groups[(mode,)] for mode in present_modes}
        reference = reference_mode(present_modes)

        if not present_modes:
            lines.append("_No valid mode results._")
            lines.append("")
            continue

        metrics = [
            ("Context tokens", "context_tokens"),
            ("Output tokens", "output_tokens"),
            ("Turns", "num_turns"),
            ("Tool calls", "num_tool_calls"),
            ("Cost USD", "total_cost_usd"),
            ("Duration ms", "duration_ms"),
        ]

        delta_modes = [mode for mode in present_modes if mode != reference]
        headers = ["Metric"] + [mode_label(mode) for mode in present_modes]
        headers += [f"{mode_label(mode)} Δ" for mode in delta_modes]
        lines.append("| " + " | ".join(headers) + " |")
        lines.append("|" + "|".join(["---"] * len(headers)) + "|")

        for label, key in metrics:
            medians = {
                mode: compute_stats([r.get(key, 0) for r in runs])["median"]
                for mode, runs in runs_by_mode.items()
            }
            row = [f"{label} (median)"]
            row.extend(format_metric_value(key, medians[mode]) for mode in present_modes)
            if reference is not None:
                reference_value = medians[reference]
                row.extend(format_delta(reference_value, medians[mode]) for mode in delta_modes)
            lines.append("| " + " | ".join(row) + " |")

        correctness = {mode: correctness_pct(runs) for mode, runs in runs_by_mode.items()}
        row = ["Correctness"]
        row.extend(f"{correctness[mode]:.0f}%" for mode in present_modes)
        if reference is not None:
            reference_correctness = correctness[reference]
            row.extend(f"{correctness[mode] - reference_correctness:+.0f}pp" for mode in delta_modes)
        lines.append("| " + " | ".join(row) + " |")
        lines.append("")

        # Cost breakdown
        median_cost_runs = {
            mode: find_median_run(runs, "total_cost_usd")
            for mode, runs in runs_by_mode.items()
        }
        median_costs = {
            mode: compute_cost_breakdown(run)
            for mode, run in median_cost_runs.items()
        }
        label_width = max(len(mode_label(mode)) for mode in present_modes)

        lines.append("**Cost breakdown (median run):**")
        lines.append("")
        for mode in present_modes:
            run = median_cost_runs[mode]
            total = run.get("total_cost_usd", 0.0)
            turns = run.get("num_turns", 0)
            correct_str = "correct" if run.get("correct", False) else "incorrect"
            label = mode_label(mode).ljust(label_width)
            lines.append(f"  {label}: {turns} turns, ${total:.2f}, {correct_str}")
            lines.append(format_cost_breakdown(median_costs[mode]))

        if reference is not None and delta_modes:
            reference_run = median_cost_runs[reference]
            reference_costs = median_costs[reference]
            reference_total = reference_run.get("total_cost_usd", 0.0)
            reference_turns = reference_run.get("num_turns", 0)
            for mode in delta_modes:
                run = median_cost_runs[mode]
                total_delta = run.get("total_cost_usd", 0.0) - reference_total
                turns_delta = run.get("num_turns", 0) - reference_turns
                lines.append(
                    f"  {mode_label(mode)} vs {reference}: "
                    f"{'+' if turns_delta >= 0 else ''}{turns_delta} turns, "
                    f"{'+' if total_delta >= 0 else ''}${total_delta:.2f}"
                )
                lines.append(format_cost_delta(reference_costs, median_costs[mode]))
        lines.append("")

        # Per-turn sparklines
        median_context_runs = {
            mode: find_median_run(runs, "context_tokens")
            for mode, runs in runs_by_mode.items()
        }
        per_turn_by_mode = {
            mode: run.get("per_turn_context_tokens", [])
            for mode, run in median_context_runs.items()
        }
        if any(per_turn_by_mode.values()):
            lines.append("**Per-turn context tokens (median run):**")
            lines.append("")
            for mode in present_modes:
                per_turn = per_turn_by_mode[mode]
                if not per_turn:
                    continue
                spark = ascii_sparkline(per_turn)
                token_range = f"{min(per_turn):,} → {max(per_turn):,}"
                label = mode_label(mode).ljust(label_width)
                lines.append(f"  {label}: {spark} ({token_range})")
            lines.append("")

        # Tool breakdown
        tools_by_mode = {
            mode: merge_tool_calls(runs)
            for mode, runs in runs_by_mode.items()
        }
        if any(tools_by_mode.values()):
            lines.append("**Tool breakdown (median counts):**")
            lines.append("")
            for mode in present_modes:
                tools = tools_by_mode[mode]
                if not tools:
                    continue
                tool_strs = [f"{name}={count:.0f}" for name, count in sorted(tools.items())]
                label = mode_label(mode).ljust(label_width)
                lines.append(f"  {label}: {', '.join(tool_strs)}")
            lines.append("")

        lines.append("")

    # Summary section (if multiple modes are present)
    runs_by_mode_all = {
        mode: [r for r in valid_results if r["mode"] == mode]
        for mode in modes
    }
    present_modes_all = [mode for mode, runs in runs_by_mode_all.items() if runs]

    if len(present_modes_all) > 1:
        lines.append("## Summary")
        lines.append("")
        lines.append("Averaged across all tasks (median of medians):")
        lines.append("")

        reference = reference_mode(present_modes_all)
        delta_modes = [mode for mode in present_modes_all if mode != reference]
        headers = ["Metric"] + [mode_label(mode) for mode in present_modes_all]
        headers += [f"{mode_label(mode)} Δ" for mode in delta_modes]
        lines.append("| " + " | ".join(headers) + " |")
        lines.append("|" + "|".join(["---"] * len(headers)) + "|")

        metrics = [
            ("Context tokens", "context_tokens"),
            ("Turns", "num_turns"),
            ("Tool calls", "num_tool_calls"),
            ("Cost USD", "total_cost_usd"),
        ]

        for label, key in metrics:
            medians_by_mode = {}
            for mode in present_modes_all:
                by_task = group_by(runs_by_mode_all[mode], "task")
                task_medians = [
                    compute_stats([r.get(key, 0) for r in runs])["median"]
                    for runs in by_task.values()
                ]
                if task_medians:
                    medians_by_mode[mode] = median(task_medians)

            if not medians_by_mode:
                continue

            row = [label]
            row.extend(
                format_metric_value(key, medians_by_mode.get(mode, 0))
                for mode in present_modes_all
            )
            if reference is not None and reference in medians_by_mode:
                reference_value = medians_by_mode[reference]
                row.extend(
                    format_delta(reference_value, medians_by_mode[mode])
                    if mode in medians_by_mode else "—"
                    for mode in delta_modes
                )
            lines.append("| " + " | ".join(row) + " |")

        lines.append("")

    lines.append("")
    lines.extend(statistical_analysis_section(valid_results, results))

    return "\n".join(lines)


def statistical_analysis_section(valid_results: list[dict], all_results: list[dict]) -> list[str]:
    """Build the '## Statistical Analysis' section: accuracy CIs, cost-per-correct,
    per-repo clustering, synthetic-vs-real split, and the paired A/B power readout."""
    modes = ordered_modes(set(r["mode"] for r in valid_results))
    runs_by_mode = {m: [r for r in valid_results if r["mode"] == m] for m in modes}
    present = [m for m in modes if runs_by_mode[m]]

    lines = ["## Statistical Analysis", ""]
    if not present:
        lines.append("_No valid results to analyze._")
        return lines

    def acc_cell(runs):
        if not runs:
            return "—"
        pct, lo, hi = correctness_with_ci(runs)
        return f"{pct:.0f}% [{lo:.0f}–{hi:.0f}] (n={len(runs)})"

    divider = "|" + "|".join(["---"] * (len(present) + 1)) + "|"

    lines.append("**Accuracy with 95% Wilson CIs (all tasks pooled):**")
    lines.append("")
    lines.append("| Mode | Accuracy [95% CI] |")
    lines.append("|---|---|")
    for m in present:
        lines.append(f"| {mode_label(m)} | " + acc_cell(runs_by_mode[m]) + " |")
    lines.append("")

    lines.append("**Cost per correct answer (total cost / correct, expected cost under retry):**")
    lines.append("")
    lines.append("| Mode | Cost/correct | 95% bootstrap CI |")
    lines.append("|---|---|---|")
    for m in present:
        value, lo, hi = cost_per_correct(runs_by_mode[m])
        lines.append(f"| {mode_label(m)} | {_fmt_usd(value)} | [{_fmt_usd(lo)}, {_fmt_usd(hi)}] |")
    lines.append("")

    repos = sorted(set(r.get("repo", "synthetic") for r in valid_results))
    lines.append("**Accuracy clustered by repo:**")
    lines.append("")
    lines.append("| Repo | " + " | ".join(mode_label(m) for m in present) + " |")
    lines.append(divider)
    for repo in repos:
        cells = [acc_cell([r for r in runs_by_mode[m] if r.get("repo", "synthetic") == repo]) for m in present]
        lines.append(f"| {repo} | " + " | ".join(cells) + " |")
    lines.append("")

    lines.append("**Synthetic vs real-repo split:**")
    lines.append("")
    lines.append("| Bucket | " + " | ".join(mode_label(m) for m in present) + " |")
    lines.append(divider)
    buckets = [
        ("synthetic", lambda r: r.get("repo", "synthetic") == "synthetic"),
        ("real", lambda r: r.get("repo", "synthetic") != "synthetic"),
    ]
    for name, pred in buckets:
        cells = [acc_cell([r for r in runs_by_mode[m] if pred(r)]) for m in present]
        lines.append(f"| {name} | " + " | ".join(cells) + " |")
    lines.append("")

    lines.extend(_power_readout(all_results))
    return lines


def _power_readout(all_results: list[dict]) -> list[str]:
    """Report task-clustered paired accuracy and task-level power by model."""
    modes = ordered_modes({
        run.get("mode")
        for run in all_results
        if isinstance(run.get("mode"), str)
    })
    comparisons = comparison_pairs(modes)
    lines = [
        "**Task-clustered paired accuracy differences:**",
        "_The task is the sampling unit. Repetitions and models are averaged "
        "within task before a fixed-seed 10,000-resample paired bootstrap._",
        "_Errored cells count as incorrect. A comparison is significant at "
        "α=0.05 when its 95% interval excludes zero._",
        "",
        "| Comparison | Accuracy Δ | Task-paired 95% CI | N tasks |",
        "|---|---:|---:|---:|",
    ]
    observed = False
    for experiment_mode, baseline_mode in comparisons:
        delta, lo, hi, n_tasks = paired_accuracy_delta(
            all_results,
            experiment_mode,
            baseline_mode,
        )
        if delta is None:
            continue
        observed = True
        lines.append(
            f"| {experiment_mode} vs {baseline_mode} | {delta * 100:+.1f}pp | "
            f"[{lo * 100:+.1f}, {hi * 100:+.1f}] | {n_tasks} |"
        )
    if not observed:
        lines.append("| — | — | — | — |")
    lines.extend([
        "",
        "**Power readout by model:**",
        "_MDE@N is an optimistic single-proportion bound; the paired bootstrap "
        "interval is the inferential decision rule._",
        "",
    ])

    for experiment_mode, baseline_mode in comparisons:
        pairs = pair_modes(all_results, experiment_mode, baseline_mode)
        by_model: dict[str, dict[str, list[tuple]]] = defaultdict(
            lambda: defaultdict(list)
        )
        for (task, model), tuples in pairs.items():
            by_model[model][task].extend(tuples)
        for model in sorted(by_model):
            by_task = by_model[model]
            deltas = []
            baseline_rates = []
            for tuples in by_task.values():
                baseline_rate = (
                    sum(1 for (_rep, correct, _other, _a, _b) in tuples if correct)
                    / len(tuples)
                )
                experiment_rate = (
                    sum(1 for (_rep, _other, correct, _a, _b) in tuples if correct)
                    / len(tuples)
                )
                baseline_rates.append(baseline_rate)
                deltas.append(experiment_rate - baseline_rate)
            n_tasks = len(deltas)
            baseline_rate = sum(baseline_rates) / n_tasks
            delta = sum(deltas) / n_tasks
            lo, hi = stats.paired_bootstrap_ci(
                deltas,
                n_resamples=10_000,
                seed=0,
            )
            mde = stats.min_detectable_effect(n_tasks, baseline_rate)
            if lo > 0 or hi < 0:
                verdict = "effect SIGNIFICANT"
            elif abs(delta) < mde:
                verdict = "N INSUFFICIENT for observed effect — grow TASK pool"
            else:
                verdict = "CI includes zero — more data advised"
            lines.append(
                f"- **{experiment_mode} vs {baseline_mode}; {model}**: "
                f"N={n_tasks} tasks, reference {baseline_rate * 100:.0f}%, "
                f"observed Δ {delta * 100:+.0f}pp, "
                f"95% CI [{lo * 100:+.0f}, {hi * 100:+.0f}], "
                f"MDE@N≈{mde * 100:.0f}pp → {verdict}"
            )
    if not comparisons:
        lines.append("_No paired variant comparisons available._")
    lines.append("")
    return lines


def main():
    parser = argparse.ArgumentParser(
        description="Analyze benchmark results and generate report",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python analyze.py results/benchmark_20260212_150000.jsonl
  python analyze.py results/benchmark_20260212_150000.jsonl -o report.md
        """,
    )

    parser.add_argument(
        "results_file",
        type=Path,
        help="Path to JSONL results file from run.py",
    )
    parser.add_argument(
        "-o", "--output",
        type=Path,
        help="Output path for markdown report (default: print to stdout)",
    )

    args = parser.parse_args()

    if not args.results_file.exists():
        print(f"ERROR: File not found: {args.results_file}", file=sys.stderr)
        sys.exit(1)

    try:
        results = load_results(args.results_file)
    except Exception as e:
        print(f"ERROR: Failed to load results: {e}", file=sys.stderr)
        sys.exit(1)

    report = generate_report(results)

    # Output
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(report)
        print(f"Report written to: {args.output}")
    else:
        print(report)


if __name__ == "__main__":
    main()
