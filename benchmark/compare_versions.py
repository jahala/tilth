#!/usr/bin/env python3
"""Compare two benchmark JSONL files — useful for before/after a tilth release.

Imports the same metric helpers as analyze.py so the diff cannot drift from
the per-file report. Default output is markdown; --json emits the same shape
as a machine-readable dict.

Comparison scope:
  - Aggregate per-mode metrics (cost/tokens/turns per correct answer)
  - Per (task, model) cells present in both files: cost delta, accuracy delta
  - Tasks only in old or only in new (surface-shift indicator)
  - Accuracy regressions / improvements
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from analyze import (
    COST_REGRESSION_THRESHOLD_PCT,
    accuracy,
    build_summary_data,
    cost_per_correct,
    fmt_money,
    fmt_pct_delta,
    group_by_keys,
    load_results,
    markdown_table,
    tokens_per_correct,
    turns_per_correct,
)


def _delta_pct(old, new):
    """Return percent change old→new, or None when either side is None or old is 0."""
    if old is None or new is None or old == 0:
        return None
    return (new - old) / old * 100


def compare_summaries(old, new):
    """Diff two build_summary_data outputs into a structured delta dict."""
    old_tldr = old.get("tldr")
    new_tldr = new.get("tldr")

    tldr_delta = None
    if old_tldr and new_tldr:
        tldr_delta = {}
        for mode in ("baseline", "tilth"):
            old_m = old_tldr[mode]
            new_m = new_tldr[mode]
            tldr_delta[mode] = {
                "cost_per_correct": {
                    "old": old_m["cost_per_correct"],
                    "new": new_m["cost_per_correct"],
                    "delta_pct": _delta_pct(old_m["cost_per_correct"], new_m["cost_per_correct"]),
                },
                "tokens_per_correct": {
                    "old": old_m["tokens_per_correct"],
                    "new": new_m["tokens_per_correct"],
                    "delta_pct": _delta_pct(old_m["tokens_per_correct"], new_m["tokens_per_correct"]),
                },
                "turns_per_correct": {
                    "old": old_m["turns_per_correct"],
                    "new": new_m["turns_per_correct"],
                    "delta_pct": _delta_pct(old_m["turns_per_correct"], new_m["turns_per_correct"]),
                },
                "correct": {"old": old_m["correct"], "new": new_m["correct"]},
                "total": {"old": old_m["total"], "new": new_m["total"]},
            }

    return {
        "old_file": old.get("metadata", {}).get("source"),
        "new_file": new.get("metadata", {}).get("source"),
        "old_tilth_versions": old.get("metadata", {}).get("tilth_versions", []),
        "new_tilth_versions": new.get("metadata", {}).get("tilth_versions", []),
        "tldr_delta": tldr_delta,
    }


def _cell_summary(runs):
    """Compact stats for a (task, model, mode) cell."""
    c, t = accuracy(runs)
    return {
        "correct": c,
        "total": t,
        "cost_per_correct": cost_per_correct(runs),
        "tokens_per_correct": tokens_per_correct(runs),
        "turns_per_correct": turns_per_correct(runs),
    }


def compare_cells(old_runs, new_runs):
    """Per (task, model, mode) cell diff. Returns dict with:
      - common: cells in both files, with old/new/delta
      - old_only: cells absent from new file
      - new_only: cells absent from old file
    Cells with no correct runs in either side are still listed (their cost_delta_pct is None).
    """
    old_cells = group_by_keys(old_runs, "task", "model", "mode")
    new_cells = group_by_keys(new_runs, "task", "model", "mode")

    common_keys = sorted(old_cells.keys() & new_cells.keys())
    old_only = sorted(old_cells.keys() - new_cells.keys())
    new_only = sorted(new_cells.keys() - old_cells.keys())

    common = []
    for key in common_keys:
        task, model, mode = key
        old_summary = _cell_summary(old_cells[key])
        new_summary = _cell_summary(new_cells[key])
        common.append({
            "task": task,
            "model": model,
            "mode": mode,
            "old": old_summary,
            "new": new_summary,
            "cost_delta_pct": _delta_pct(
                old_summary["cost_per_correct"], new_summary["cost_per_correct"]
            ),
            "accuracy_delta": (
                (new_summary["correct"] / new_summary["total"] if new_summary["total"] else 0)
                - (old_summary["correct"] / old_summary["total"] if old_summary["total"] else 0)
            ),
        })

    return {
        "common": common,
        "old_only": [{"task": t, "model": m, "mode": md} for (t, m, md) in old_only],
        "new_only": [{"task": t, "model": m, "mode": md} for (t, m, md) in new_only],
    }


def render_markdown(diff, cells):
    """Render the diff + cell comparison as a markdown report."""
    old_ver = (
        f" ({', '.join(diff['old_tilth_versions'])})" if diff["old_tilth_versions"] else ""
    )
    new_ver = (
        f" ({', '.join(diff['new_tilth_versions'])})" if diff["new_tilth_versions"] else ""
    )
    parts = [
        "# tilth bench comparison",
        f"_old:_ `{diff['old_file']}`{old_ver} → _new:_ `{diff['new_file']}`{new_ver}",
    ]

    # Aggregate TL;DR delta
    if diff["tldr_delta"]:
        parts.append("\n## Aggregate TL;DR delta\n")
        for mode in ("baseline", "tilth"):
            block = diff["tldr_delta"][mode]
            parts.append(f"\n### {mode}\n")
            rows = []
            for metric_key, label, fmt in (
                ("cost_per_correct", "Cost per correct", fmt_money),
                ("tokens_per_correct", "Tokens per correct", lambda v: "—" if v is None else f"{v:,}"),
                ("turns_per_correct", "Turns per correct", lambda v: "—" if v is None else f"{v:.2f}"),
            ):
                m = block[metric_key]
                rows.append([
                    label,
                    fmt(m["old"]),
                    fmt(m["new"]),
                    fmt_pct_delta(m["old"], m["new"]),
                ])
            rows.append([
                "Correct",
                f"{block['correct']['old']}/{block['total']['old']}",
                f"{block['correct']['new']}/{block['total']['new']}",
                "—",
            ])
            parts.append(markdown_table(["Metric", "old", "new", "Δ"], rows))

    # Per-cell improvements / regressions
    if cells["common"]:
        with_cost = [c for c in cells["common"] if c["cost_delta_pct"] is not None]
        improvements = sorted(
            [c for c in with_cost if c["cost_delta_pct"] <= -COST_REGRESSION_THRESHOLD_PCT],
            key=lambda c: c["cost_delta_pct"],
        )
        regressions = sorted(
            [c for c in with_cost if c["cost_delta_pct"] >= COST_REGRESSION_THRESHOLD_PCT],
            key=lambda c: -c["cost_delta_pct"],
        )

        if improvements:
            parts.append(f"\n## Improvements ({len(improvements)})")
            parts.append(f"_Cells where new is ≥{COST_REGRESSION_THRESHOLD_PCT:.0f}% cheaper per correct answer._\n")
            for c in improvements:
                parts.append(
                    f"- `{c['task']}` · {c['model']} · {c['mode']}: "
                    f"{fmt_money(c['old']['cost_per_correct'])} → "
                    f"{fmt_money(c['new']['cost_per_correct'])} "
                    f"(**{c['cost_delta_pct']:+.0f}%**)"
                )

        if regressions:
            parts.append(f"\n## Regressions ({len(regressions)})")
            parts.append(f"_Cells where new is ≥{COST_REGRESSION_THRESHOLD_PCT:.0f}% more expensive per correct answer._\n")
            for c in regressions:
                parts.append(
                    f"- `{c['task']}` · {c['model']} · {c['mode']}: "
                    f"{fmt_money(c['old']['cost_per_correct'])} → "
                    f"{fmt_money(c['new']['cost_per_correct'])} "
                    f"(**{c['cost_delta_pct']:+.0f}%**)"
                )

        accuracy_shifts = [
            c for c in cells["common"]
            if abs(c["accuracy_delta"]) > 0.01  # >1 percentage-point shift
        ]
        if accuracy_shifts:
            parts.append(f"\n## Accuracy shifts ({len(accuracy_shifts)})\n")
            for c in sorted(accuracy_shifts, key=lambda x: x["accuracy_delta"]):
                old_pct = (
                    c["old"]["correct"] / c["old"]["total"] * 100 if c["old"]["total"] else 0
                )
                new_pct = (
                    c["new"]["correct"] / c["new"]["total"] * 100 if c["new"]["total"] else 0
                )
                parts.append(
                    f"- `{c['task']}` · {c['model']} · {c['mode']}: "
                    f"{c['old']['correct']}/{c['old']['total']} ({old_pct:.0f}%) → "
                    f"{c['new']['correct']}/{c['new']['total']} ({new_pct:.0f}%)"
                )

    if cells["old_only"]:
        parts.append(f"\n## Tasks only in old ({len(cells['old_only'])})\n")
        for c in cells["old_only"]:
            parts.append(f"- `{c['task']}` · {c['model']} · {c['mode']}")
    if cells["new_only"]:
        parts.append(f"\n## Tasks only in new ({len(cells['new_only'])})\n")
        for c in cells["new_only"]:
            parts.append(f"- `{c['task']}` · {c['model']} · {c['mode']}")

    return "\n".join(parts)


def main():
    parser = argparse.ArgumentParser(
        description="Compare two benchmark JSONL files",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("old", type=Path, help="Older / baseline JSONL")
    parser.add_argument("new", type=Path, help="Newer / candidate JSONL")
    parser.add_argument("--json", action="store_true", help="Emit JSON instead of markdown")
    parser.add_argument("-o", "--output", type=Path)
    args = parser.parse_args()

    for path in (args.old, args.new):
        if not path.exists():
            print(f"ERROR: File not found: {path}", file=sys.stderr)
            sys.exit(1)

    old_runs = load_results(args.old)
    new_runs = load_results(args.new)

    # Filter to valid runs for the cell comparison (the summary builder
    # already drops invalid rows internally).
    old_valid = [r for r in old_runs if "error" not in r and "correct" in r]
    new_valid = [r for r in new_runs if "error" not in r and "correct" in r]

    old_summary = build_summary_data(old_runs, source_path=str(args.old))
    new_summary = build_summary_data(new_runs, source_path=str(args.new))
    diff = compare_summaries(old_summary, new_summary)
    cells = compare_cells(old_valid, new_valid)

    if args.json:
        output = json.dumps({"diff": diff, "cells": cells}, indent=2)
    else:
        output = render_markdown(diff, cells)

    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(output)
        print(f"Comparison written to: {args.output}")
    else:
        print(output)


if __name__ == "__main__":
    main()
