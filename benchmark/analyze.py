#!/usr/bin/env python3
"""Benchmark analysis and report generation.

Reads JSONL results from run.py and emits a markdown report:
  1. Header — file · models · modes · tasks · tilth version
  2. TL;DR — paired baseline/tilth comparison (skipped if single-mode)
  3. Per model — paired metrics broken down by model
  4. Per task — paired metrics + cost breakdown + per-turn sparklines per task
  5. Tool usage — current placeholder; reworked with adoption % in #284
  6. Run metadata — footer with source, versions, totals
"""

import argparse
import json
import sys
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path
from statistics import median


# Anthropic Claude pricing — USD per million tokens.
PRICING = {
    "cache_creation": 3.75,
    "cache_read": 0.30,
    "output": 15.00,
    "input": 3.00,
}

# |Δ| ≥ this is treated as a Notable change: bolded in tables, flagged in summaries.
COST_REGRESSION_THRESHOLD_PCT = 5.0

# Sparkline ramp; index 0 is the lowest value, last is the highest.
SPARKLINE_CHARS = " ▁▂▃▄▅▆▇█"

# Failure-log result_text excerpt length. Long enough to surface the agent's
# concluding sentence; short enough not to drown the report.
FAILURE_PREVIEW_CHARS = 200

# Top-N most common consecutive-tool-pair patterns to list per (mode, model).
SEQUENCE_TOP_N = 2


# ---------------------------------------------------------------------------
# Pure metric helpers — each takes a list of runs, returns a number or None.
# Failures are excluded from per-correct averages (a failure didn't actually
# reach the answer, so averaging its cost would inflate baseline efficiency).
# ---------------------------------------------------------------------------


def correct_runs(runs):
    return [r for r in runs if r.get("correct")]


def accuracy(runs):
    """(correct_count, total) — total includes failures."""
    return sum(1 for r in runs if r.get("correct")), len(runs)


def cost_per_correct(runs):
    cr = correct_runs(runs)
    if not cr:
        return None
    return sum(r.get("total_cost_usd", 0.0) for r in cr) / len(cr)


def tokens_per_correct(runs):
    """Mean context_tokens (input + cache_creation + cache_read) for correct runs."""
    cr = correct_runs(runs)
    if not cr:
        return None
    return int(sum(r.get("context_tokens", 0) for r in cr) / len(cr))


def turns_per_correct(runs):
    cr = correct_runs(runs)
    if not cr:
        return None
    return sum(r.get("num_turns", 0) for r in cr) / len(cr)


# ---------------------------------------------------------------------------
# Grouping / lookup primitives
# ---------------------------------------------------------------------------


def group_by_keys(runs, *keys):
    """Partition runs into cells keyed by the named fields."""
    groups = defaultdict(list)
    for r in runs:
        groups[tuple(r.get(k) for k in keys)].append(r)
    return dict(groups)


def find_median_run(runs, key):
    """Pick the run whose value for `key` is the median of the group.

    Returns {} for an empty list. Used to anchor per-task sparklines and cost
    breakdowns to a single representative run rather than a synthetic average.
    """
    if not runs:
        return {}
    ordered = sorted(runs, key=lambda r: r.get(key, 0))
    return ordered[len(ordered) // 2]


# ---------------------------------------------------------------------------
# Formatting helpers
# ---------------------------------------------------------------------------


def fmt_money(value):
    return "—" if value is None else f"${value:.4f}"


def fmt_int(value):
    return "—" if value is None else f"{value:,}"


def fmt_float(value, precision=2):
    return "—" if value is None else f"{value:.{precision}f}"


def fmt_pct_delta(baseline, tilth, bold_threshold=COST_REGRESSION_THRESHOLD_PCT):
    """% change baseline→tilth. Bold when |Δ| ≥ threshold."""
    if baseline is None or tilth is None or baseline == 0:
        return "—"
    delta = (tilth - baseline) / baseline * 100
    sign = "+" if delta >= 0 else ""
    text = f"{sign}{delta:.0f}%"
    return f"**{text}**" if abs(delta) >= bold_threshold else text


def markdown_table(headers, rows):
    lines = [
        "| " + " | ".join(headers) + " |",
        "|" + "|".join("---" for _ in headers) + "|",
    ]
    for row in rows:
        lines.append("| " + " | ".join(str(c) for c in row) + " |")
    return "\n".join(lines)


def sparkline(values):
    if not values:
        return ""
    lo, hi = min(values), max(values)
    if lo == hi:
        return SPARKLINE_CHARS[-1] * len(values)
    span = hi - lo
    last_idx = len(SPARKLINE_CHARS) - 1
    return "".join(
        SPARKLINE_CHARS[min(int((v - lo) / span * last_idx), last_idx)] for v in values
    )


# ---------------------------------------------------------------------------
# Cost breakdown by token category
# ---------------------------------------------------------------------------


def cost_breakdown(run):
    """Per-category USD cost for a single run."""
    return {
        category: run.get(f"{category}_tokens", 0) * price / 1_000_000
        for category, price in PRICING.items()
    }


def cost_breakdown_line(run):
    cb = cost_breakdown(run)
    return (
        f"cache_create=${cb['cache_creation']:.3f} "
        f"cache_read=${cb['cache_read']:.3f} "
        f"output=${cb['output']:.3f} "
        f"input=${cb['input']:.3f}"
    )


# ---------------------------------------------------------------------------
# Section builders — each returns a markdown string, or None to skip the section.
# ---------------------------------------------------------------------------


def section_header(runs, source_path, error_count):
    models = sorted({r["model"] for r in runs})
    modes = sorted({r["mode"] for r in runs})
    tasks = sorted({r["task"] for r in runs})
    tilth_versions = sorted({r["tilth_version"] for r in runs if r.get("tilth_version")})
    version_str = ", ".join(tilth_versions) if tilth_versions else "—"

    fname = Path(source_path).name if source_path else "(stdin)"
    meta = (
        f"_{len(runs)} runs · models: {', '.join(models)} · "
        f"modes: {', '.join(modes)} · {len(tasks)} tasks · tilth: {version_str}_"
    )
    if error_count > 0:
        meta += f" _· {error_count} errors_"
    return f"# tilth bench: {fname}\n{meta}"


def find_accuracy_regressions(runs):
    """List (task, model) cells where tilth correctness ratio < baseline's.

    Each entry carries enough detail to render a one-line summary in the
    warning banner. Cells without both modes are skipped (no comparison
    possible).
    """
    cells = group_by_keys(runs, "task", "model")
    regressions = []
    for (task, model), cell_runs in cells.items():
        baseline = [r for r in cell_runs if r["mode"] == "baseline"]
        tilth = [r for r in cell_runs if r["mode"] == "tilth"]
        if not baseline or not tilth:
            continue
        b_correct, b_total = accuracy(baseline)
        t_correct, t_total = accuracy(tilth)
        if not b_total or not t_total:
            continue
        if (t_correct / t_total) < (b_correct / b_total):
            regressions.append({
                "task": task,
                "model": model,
                "baseline_correct": b_correct,
                "baseline_total": b_total,
                "tilth_correct": t_correct,
                "tilth_total": t_total,
            })
    return regressions


def find_cost_regressions(runs):
    """List (task, model) cells where tilth cost-per-correct exceeds baseline's
    by COST_REGRESSION_THRESHOLD_PCT or more. Cells without correct runs in
    either mode are skipped — there's no per-correct cost to compare.
    """
    cells = group_by_keys(runs, "task", "model")
    regressions = []
    for (task, model), cell_runs in cells.items():
        baseline = [r for r in cell_runs if r["mode"] == "baseline"]
        tilth = [r for r in cell_runs if r["mode"] == "tilth"]
        b_cost = cost_per_correct(baseline)
        t_cost = cost_per_correct(tilth)
        if b_cost is None or t_cost is None or b_cost == 0:
            continue
        delta_pct = (t_cost - b_cost) / b_cost * 100
        if delta_pct >= COST_REGRESSION_THRESHOLD_PCT:
            regressions.append({
                "task": task,
                "model": model,
                "baseline_cost": b_cost,
                "tilth_cost": t_cost,
                "delta_pct": delta_pct,
            })
    return regressions


def find_best_worst_gain(runs):
    """Find (task, model) cells with the most negative and most positive cost Δ.

    A cell is eligible only if both modes have ≥1 correct run. Returns
    (best_pair, best_delta_pct, worst_pair, worst_delta_pct), or four Nones
    when no eligible cells exist.
    """
    cells = group_by_keys(runs, "task", "model")
    deltas = []
    for (task, model), cell_runs in cells.items():
        baseline = [r for r in cell_runs if r["mode"] == "baseline"]
        tilth = [r for r in cell_runs if r["mode"] == "tilth"]
        b_cost = cost_per_correct(baseline)
        t_cost = cost_per_correct(tilth)
        if b_cost is None or t_cost is None or b_cost == 0:
            continue
        deltas.append(((task, model), (t_cost - b_cost) / b_cost * 100))
    if not deltas:
        return None, None, None, None
    deltas.sort(key=lambda x: x[1])
    return deltas[0][0], deltas[0][1], deltas[-1][0], deltas[-1][1]


def _fmt_gain_cell(pair, delta):
    """Format a best/worst gain cell — task · model + bolded percent."""
    if pair is None:
        return "—", "—"
    text = f"{pair[0]} · {pair[1]}"
    sign = "+" if delta >= 0 else ""
    delta_text = f"{sign}{delta:.0f}% cost"
    if abs(delta) >= COST_REGRESSION_THRESHOLD_PCT:
        delta_text = f"**{delta_text}**"
    return text, delta_text


def section_regression_banner(runs):
    """Blockquoted warning above TL;DR listing every accuracy regression.

    Accuracy regressions are hard-fail signals: any (task, model) cell where
    tilth scored fewer correct runs than baseline deserves a top-of-report
    callout, regardless of how much cost it saved.

    Returns None when accuracy held across every paired cell.
    """
    regressions = find_accuracy_regressions(runs)
    if not regressions:
        return None
    noun = "regression" if len(regressions) == 1 else "regressions"
    lines = [
        f"> **{len(regressions)} accuracy {noun}** — tilth scored lower than baseline:",
        ">",
    ]
    for r in regressions:
        b_pct = r["baseline_correct"] / r["baseline_total"] * 100
        t_pct = r["tilth_correct"] / r["tilth_total"] * 100
        lines.append(
            f"> - `{r['task']}` · {r['model']}: "
            f"baseline {r['baseline_correct']}/{r['baseline_total']} ({b_pct:.0f}%) "
            f"→ tilth {r['tilth_correct']}/{r['tilth_total']} ({t_pct:.0f}%)"
        )
    return "\n".join(lines)


def section_failures(runs):
    """Detail every failed run: reason, repro command, result-text preview.

    Sorted by (task, model, mode, rep) so failures of the same cell cluster.
    Returns None when every run succeeded — the section disappears entirely
    rather than rendering a 'no failures' placeholder.
    """
    failures = [r for r in runs if not r.get("correct")]
    if not failures:
        return None
    failures.sort(key=lambda r: (r["task"], r["model"], r["mode"], r.get("repetition", 0)))

    parts = [f"## Failures ({len(failures)})"]
    for f in failures:
        task = f["task"]
        model = f["model"]
        mode = f["mode"]
        rep = f.get("repetition", "?")
        reason = f.get("correctness_reason") or "—"
        repro = (
            f"python3 benchmark/run.py "
            f"--models {model} --reps 1 "
            f"--tasks {task} --modes {mode}"
        )
        preview = (f.get("result_text") or "").strip().replace("\n", " ")
        if len(preview) > FAILURE_PREVIEW_CHARS:
            preview = preview[:FAILURE_PREVIEW_CHARS].rstrip() + "…"
        elif not preview:
            preview = "_(no result_text captured)_"

        parts.append(
            f"\n### `{task}` · {model} · {mode} · rep {rep}\n\n"
            f"**Reason:** {reason}\n\n"
            f"**Repro:**\n```\n{repro}\n```\n\n"
            f"**Result preview:** {preview}"
        )
    return "\n".join(parts)


def section_notable_cost_regressions(runs):
    """Diagnostic list of cells where tilth costs ≥ threshold% more than baseline.

    Lower-priority than the accuracy banner — these are informational, not
    hard-fail. Sorted worst-first so the most expensive cells lead.

    Returns None when no cells exceed the threshold.
    """
    regressions = find_cost_regressions(runs)
    if not regressions:
        return None
    regressions.sort(key=lambda r: -r["delta_pct"])
    lines = [
        "## Notable cost regressions",
        f"_Cells where tilth costs ≥{COST_REGRESSION_THRESHOLD_PCT:.0f}% more than baseline._\n",
    ]
    for r in regressions:
        lines.append(
            f"- `{r['task']}` · {r['model']}: "
            f"{fmt_money(r['baseline_cost'])} → {fmt_money(r['tilth_cost'])} "
            f"(**+{r['delta_pct']:.0f}%**)"
        )
    return "\n".join(lines)


def section_tldr(runs):
    """TL;DR markdown table; None if the file isn't a paired baseline/tilth comparison."""
    baseline = [r for r in runs if r["mode"] == "baseline"]
    tilth = [r for r in runs if r["mode"] == "tilth"]
    if not baseline or not tilth:
        return None

    b_correct, b_total = accuracy(baseline)
    t_correct, t_total = accuracy(tilth)
    b_cost, t_cost = cost_per_correct(baseline), cost_per_correct(tilth)
    b_tok, t_tok = tokens_per_correct(baseline), tokens_per_correct(tilth)
    b_turn, t_turn = turns_per_correct(baseline), turns_per_correct(tilth)

    correct_delta = "no regressions"
    if t_total and b_total and (t_correct / t_total) < (b_correct / b_total):
        correct_delta = "**regression** (see Failures)"

    best_pair, best_d, worst_pair, worst_d = find_best_worst_gain(runs)
    best_text, best_delta = _fmt_gain_cell(best_pair, best_d)
    worst_text, worst_delta = _fmt_gain_cell(worst_pair, worst_d)

    rows = [
        ["Correct", f"{b_correct}/{b_total}", f"{t_correct}/{t_total}", correct_delta],
        ["Cost per correct answer", fmt_money(b_cost), fmt_money(t_cost), fmt_pct_delta(b_cost, t_cost)],
        ["Tokens per correct answer", fmt_int(b_tok), fmt_int(t_tok), fmt_pct_delta(b_tok, t_tok)],
        ["Turns per correct answer", fmt_float(b_turn), fmt_float(t_turn), fmt_pct_delta(b_turn, t_turn)],
        ["Best gain", "—", best_text, best_delta],
        ["Worst gain", "—", worst_text, worst_delta],
    ]
    return "## TL;DR\n\n" + markdown_table(["Headline", "baseline", "tilth", "Δ"], rows)


def section_per_model(runs):
    """One row per model that has both modes. Skipped entirely if no paired model."""
    rows = []
    for model in sorted({r["model"] for r in runs}):
        m_runs = [r for r in runs if r["model"] == model]
        baseline = [r for r in m_runs if r["mode"] == "baseline"]
        tilth = [r for r in m_runs if r["mode"] == "tilth"]
        if not baseline or not tilth:
            continue
        b_correct, b_total = accuracy(baseline)
        t_correct, t_total = accuracy(tilth)
        b_cost, t_cost = cost_per_correct(baseline), cost_per_correct(tilth)
        b_tok, t_tok = tokens_per_correct(baseline), tokens_per_correct(tilth)
        b_turn, t_turn = turns_per_correct(baseline), turns_per_correct(tilth)
        rows.append([
            model,
            f"{b_correct}/{b_total} → {t_correct}/{t_total}",
            f"{fmt_money(b_cost)} → {fmt_money(t_cost)}",
            fmt_pct_delta(b_cost, t_cost),
            f"{fmt_int(b_tok)} → {fmt_int(t_tok)}",
            fmt_pct_delta(b_tok, t_tok),
            f"{fmt_float(b_turn)} → {fmt_float(t_turn)}",
            fmt_pct_delta(b_turn, t_turn),
        ])
    if not rows:
        return None
    headers = [
        "Model", "n correct",
        "cost (B → T)", "Δ cost",
        "tokens (B → T)", "Δ tok",
        "turns (B → T)", "Δ turns",
    ]
    return "## Per model\n\n" + markdown_table(headers, rows)


def _per_task_paired_block(task_runs):
    """Render the metric table + cost breakdown + sparklines for a paired task."""
    baseline = [r for r in task_runs if r["mode"] == "baseline"]
    tilth = [r for r in task_runs if r["mode"] == "tilth"]
    b_correct, b_total = accuracy(baseline)
    t_correct, t_total = accuracy(tilth)
    b_cost, t_cost = cost_per_correct(baseline), cost_per_correct(tilth)
    b_tok, t_tok = tokens_per_correct(baseline), tokens_per_correct(tilth)
    b_turn, t_turn = turns_per_correct(baseline), turns_per_correct(tilth)
    b_calls = sum(r.get("num_tool_calls", 0) for r in baseline) / len(baseline)
    t_calls = sum(r.get("num_tool_calls", 0) for r in tilth) / len(tilth)

    rows = [
        ["Correct", f"{b_correct}/{b_total}", f"{t_correct}/{t_total}", "—"],
        ["Cost per correct", fmt_money(b_cost), fmt_money(t_cost), fmt_pct_delta(b_cost, t_cost)],
        ["Tokens per correct", fmt_int(b_tok), fmt_int(t_tok), fmt_pct_delta(b_tok, t_tok)],
        ["Turns per correct", fmt_float(b_turn), fmt_float(t_turn), fmt_pct_delta(b_turn, t_turn)],
        ["Tool calls (mean)", fmt_float(b_calls), fmt_float(t_calls), fmt_pct_delta(b_calls, t_calls)],
    ]
    parts = [markdown_table(["Metric", "baseline", "tilth", "Δ"], rows)]

    b_med_cost = find_median_run(baseline, "total_cost_usd")
    t_med_cost = find_median_run(tilth, "total_cost_usd")
    if b_med_cost and t_med_cost:
        parts.append("\n**Cost breakdown (median run):**\n")
        parts.append(
            f"  baseline ({b_med_cost.get('num_turns', '?')} turns): "
            + cost_breakdown_line(b_med_cost)
        )
        parts.append(
            f"  tilth    ({t_med_cost.get('num_turns', '?')} turns): "
            + cost_breakdown_line(t_med_cost)
        )

    b_med_ctx = find_median_run(baseline, "context_tokens")
    t_med_ctx = find_median_run(tilth, "context_tokens")
    b_pt = b_med_ctx.get("per_turn_context_tokens", [])
    t_pt = t_med_ctx.get("per_turn_context_tokens", [])
    if b_pt or t_pt:
        parts.append("\n**Per-turn context tokens (median run):**\n")
        if b_pt:
            parts.append(f"  baseline: {sparkline(b_pt)} ({min(b_pt):,} → {max(b_pt):,})")
        if t_pt:
            parts.append(f"  tilth:    {sparkline(t_pt)} ({min(t_pt):,} → {max(t_pt):,})")

    return "\n".join(parts)


def _per_task_single_mode_block(task_runs):
    """Render a single-mode task summary. Used when only one mode ran the task."""
    mode = task_runs[0]["mode"]
    c, t = accuracy(task_runs)
    rows = [
        ["Correct", f"{c}/{t}"],
        ["Cost per correct", fmt_money(cost_per_correct(task_runs))],
        ["Tokens per correct", fmt_int(tokens_per_correct(task_runs))],
        ["Turns per correct", fmt_float(turns_per_correct(task_runs))],
    ]
    return f"_mode: {mode}_\n\n" + markdown_table(["Metric", "value"], rows)


def section_per_task(runs):
    tasks = sorted({r["task"] for r in runs})
    parts = ["## Per task"]
    for task in tasks:
        task_runs = [r for r in runs if r["task"] == task]
        parts.append(f"\n### {task}")
        repo = task_runs[0].get("repo")
        if repo and repo != "synthetic":
            parts.append(f"_repo: {repo}_\n")
        baseline = [r for r in task_runs if r["mode"] == "baseline"]
        tilth = [r for r in task_runs if r["mode"] == "tilth"]
        if baseline and tilth:
            parts.append(_per_task_paired_block(task_runs))
        else:
            parts.append(_per_task_single_mode_block(task_runs))
    return "\n".join(parts)


def tool_adoption_stats(cell_runs):
    """For each tool invoked at least once in this cell, return adoption + median + max.

    Adoption is the primary metric (% of runs that touched the tool at all).
    median count surfaces sequence-length signal — agents leaning on a tool.
    max ceilings the heaviest single run. Tools with zero adoption are dropped:
    a row of zeros for an unused tool is noise.
    """
    n_runs = len(cell_runs)
    all_names = set()
    for run in cell_runs:
        all_names.update(run.get("tool_calls", {}).keys())
    stats = {}
    for name in all_names:
        counts = [run.get("tool_calls", {}).get(name, 0) for run in cell_runs]
        adoption_pct = 100 * sum(1 for c in counts if c > 0) / n_runs
        if adoption_pct == 0:
            continue
        stats[name] = {
            "adoption": adoption_pct,
            "median": median(counts),
            "max": max(counts),
        }
    return stats


def sequence_patterns(cell_runs, top_n=SEQUENCE_TOP_N):
    """Top-N most common consecutive tool-name pairs across the cell's runs.

    Aggregates pair counts over *all* runs in the cell — a run that runs
    `search → read → search → read` contributes two `search → read` pairs.
    Returns list of ((tool_a, tool_b), count) tuples sorted by frequency.

    Empty when no tool_sequence data exists (early Feb files lack the field).
    """
    pairs = Counter()
    for run in cell_runs:
        seq = run.get("tool_sequence") or []
        names = [step.get("name") for step in seq if isinstance(step, dict) and step.get("name")]
        for i in range(len(names) - 1):
            pairs[(names[i], names[i + 1])] += 1
    return pairs.most_common(top_n)


def section_tool_usage(runs):
    """Per (mode × model): tool adoption table + top sequence patterns.

    Cells with no tool usage at all are skipped (some early MCP-less runs
    may carry empty tool_calls). Section returns None if no cell has any
    tool data — keeps the report tight on minimal-input files.
    """
    cells = group_by_keys(runs, "mode", "model")
    parts = []
    for cell_key in sorted(cells.keys()):
        mode, model = cell_key
        cell_runs = cells[cell_key]
        n_runs = len(cell_runs)
        stats = tool_adoption_stats(cell_runs)
        if not stats:
            continue

        ordered = sorted(stats.items(), key=lambda kv: (-kv[1]["adoption"], -kv[1]["median"]))
        rows = [
            [
                f"`{name}`",
                f"{s['adoption']:.0f}%",
                f"{s['median']:.0f}",
                f"{s['max']}",
            ]
            for name, s in ordered
        ]
        parts.append(f"\n### {mode}/{model} ({n_runs} runs)\n")
        parts.append(markdown_table(["Tool", "adoption", "median count", "max"], rows))

        patterns = sequence_patterns(cell_runs)
        if patterns:
            parts.append("\n**Top sequence patterns:**\n")
            for (tool_a, tool_b), count in patterns:
                avg_per_run = count / n_runs
                parts.append(
                    f"- `{tool_a}` → `{tool_b}`: "
                    f"{count} occurrences (avg {avg_per_run:.1f}/run)"
                )
    if not parts:
        return None
    return "## Tool usage" + "\n".join(parts)


def section_metadata(runs, source_path):
    parts = ["## Run metadata"]
    if source_path:
        parts.append(f"- source: `{source_path}`")
    tilth_versions = sorted({r["tilth_version"] for r in runs if r.get("tilth_version")})
    parts.append(f"- tilth versions: {', '.join(tilth_versions) if tilth_versions else '—'}")
    parts.append(f"- total runs: {len(runs)}")
    parts.append(f"- total cost (all runs): ${sum(r.get('total_cost_usd', 0) for r in runs):.2f}")
    total_ms = sum(r.get("duration_ms", 0) for r in runs)
    if total_ms:
        parts.append(f"- total duration: {total_ms / 1000 / 60:.1f} min")
    parts.append(f"- generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Top-level orchestration
# ---------------------------------------------------------------------------


def filter_valid(runs):
    """Split into (valid, error_count). A row is invalid if it has an `error`
    field set by the runner, or lacks the `correct` field we need to score it."""
    valid = [r for r in runs if "error" not in r and "correct" in r]
    return valid, len(runs) - len(valid)


def generate_report(runs, source_path=None):
    valid, error_count = filter_valid(runs)
    if not valid:
        return f"# Error\n\nNo valid runs found ({len(runs)} total, {error_count} errors)."

    sections = [
        section_header(valid, source_path, error_count),
        section_regression_banner(valid),
        section_tldr(valid),
        section_per_model(valid),
        section_per_task(valid),
        section_failures(valid),
        section_notable_cost_regressions(valid),
        section_tool_usage(valid),
        section_metadata(valid, source_path),
    ]
    return "\n\n".join(s for s in sections if s)


def _mode_summary(mode_runs):
    """Per-mode summary block used by build_summary_data."""
    correct, total = accuracy(mode_runs)
    return {
        "correct": correct,
        "total": total,
        "cost_per_correct": cost_per_correct(mode_runs),
        "tokens_per_correct": tokens_per_correct(mode_runs),
        "turns_per_correct": turns_per_correct(mode_runs),
    }


def build_summary_data(runs, source_path=None):
    """Machine-readable summary that mirrors the markdown report.

    Both renderers (markdown via generate_report, JSON via this function)
    consume the same metric helpers, so the two representations cannot
    drift. The shape is documented in benchmark/README.md.
    """
    valid, error_count = filter_valid(runs)

    metadata = {
        "source": Path(source_path).name if source_path else None,
        "valid_runs": len(valid),
        "error_runs": error_count,
        "models": sorted({r["model"] for r in valid}),
        "modes": sorted({r["mode"] for r in valid}),
        "tasks": sorted({r["task"] for r in valid}),
        "tilth_versions": sorted({r["tilth_version"] for r in valid if r.get("tilth_version")}),
        "total_cost_usd": sum(r.get("total_cost_usd", 0) for r in valid),
        "total_duration_min": sum(r.get("duration_ms", 0) for r in valid) / 1000 / 60,
        "generated": datetime.now().isoformat(timespec="seconds"),
    }

    baseline = [r for r in valid if r["mode"] == "baseline"]
    tilth = [r for r in valid if r["mode"] == "tilth"]

    tldr = None
    if baseline and tilth:
        best_pair, best_d, worst_pair, worst_d = find_best_worst_gain(valid)
        tldr = {
            "baseline": _mode_summary(baseline),
            "tilth": _mode_summary(tilth),
            "best_gain": (
                {"task": best_pair[0], "model": best_pair[1], "cost_delta_pct": best_d}
                if best_pair else None
            ),
            "worst_gain": (
                {"task": worst_pair[0], "model": worst_pair[1], "cost_delta_pct": worst_d}
                if worst_pair else None
            ),
        }

    per_model = []
    for model in sorted({r["model"] for r in valid}):
        m_runs = [r for r in valid if r["model"] == model]
        m_base = [r for r in m_runs if r["mode"] == "baseline"]
        m_tilth = [r for r in m_runs if r["mode"] == "tilth"]
        if not m_base or not m_tilth:
            continue
        per_model.append({
            "model": model,
            "baseline": _mode_summary(m_base),
            "tilth": _mode_summary(m_tilth),
        })

    failures = []
    failed_runs = sorted(
        [r for r in valid if not r.get("correct")],
        key=lambda r: (r["task"], r["model"], r["mode"], r.get("repetition", 0)),
    )
    for f in failed_runs:
        failures.append({
            "task": f["task"],
            "model": f["model"],
            "mode": f["mode"],
            "rep": f.get("repetition"),
            "correctness_reason": f.get("correctness_reason"),
        })

    return {
        "metadata": metadata,
        "tldr": tldr,
        "per_model": per_model,
        "accuracy_regressions": find_accuracy_regressions(valid),
        "cost_regressions": find_cost_regressions(valid),
        "failures": failures,
    }


def load_results(path):
    """Parse a JSONL file, skipping lines that fail to decode.

    Some historical JSONLs contain corrupt lines — usually a runner that
    forgot to escape a newline inside a long `result_text` string. We surface
    the count via the metadata error-row tally rather than crashing the
    report on otherwise-good neighbors.
    """
    results = []
    bad_lines = 0
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                results.append(json.loads(line))
            except json.JSONDecodeError:
                bad_lines += 1
    if bad_lines:
        # Inject a synthetic error-marker row so the header surfaces the count
        # via the standard error_count path (no special-case rendering needed).
        results.extend([{"error": "json decode failed"}] * bad_lines)
    return results


def main():
    parser = argparse.ArgumentParser(description="Analyze benchmark results")
    parser.add_argument("results_file", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON instead of markdown.",
    )
    args = parser.parse_args()

    if not args.results_file.exists():
        print(f"ERROR: File not found: {args.results_file}", file=sys.stderr)
        sys.exit(1)

    runs = load_results(args.results_file)
    if args.json:
        data = build_summary_data(runs, source_path=str(args.results_file))
        output = json.dumps(data, indent=2)
    else:
        output = generate_report(runs, source_path=str(args.results_file))

    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(output)
        print(f"Report written to: {args.output}")
    else:
        print(output)


if __name__ == "__main__":
    main()
