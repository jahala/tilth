import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

import analyze
import paired


def _run(*, task, mode, cost, correct, repetition=0, model="claude-haiku-4-5-20251001", capability=None):
    record = {
        "task": task,
        "mode": mode,
        "model": model,
        "repetition": repetition,
        "correct": correct,
        "total_cost_usd": cost,
        "context_tokens": 10,
        "output_tokens": 5,
        "input_tokens": 2,
        "cache_creation_tokens": 3,
        "cache_read_tokens": 4,
        "num_turns": 1,
        "num_tool_calls": 0,
        "duration_ms": 1,
        "result_text": f"{task}-{mode}-{repetition}",
    }
    if capability is not None:
        record["capability"] = capability
    return record


def test_headline_delta_has_direction_and_deterministic_paired_ci():
    records = [
        _run(task="one", mode="baseline", cost=1.0, correct=True, capability="locate"),
        _run(task="one", mode="tilth", cost=2.0, correct=True, capability="locate"),
        _run(task="two", mode="baseline", cost=2.0, correct=True, capability="trace"),
        _run(task="two", mode="tilth", cost=1.0, correct=True, capability="trace"),
    ]

    first = analyze.paired_cpc_delta(records, "tilth")
    second = analyze.paired_cpc_delta(records, "tilth")

    assert first == second
    assert first[0] == 0.0
    report = analyze.generate_report(records)
    assert report.index("## Headline") < report.index("## Context Efficiency")
    assert "| tilth-added | $1.5000 | $1.5000 | +0% |" in report
    assert "95% CI" in report
    assert "## Evidence flags" in report
    assert "## Statistical Analysis" in report
    assert "Accuracy with 95% Wilson CIs" in report
    assert "McNemar p=" in report
    assert "MDE@N" in report
    assert "## Control-task delta" in report

def test_report_header_keeps_error_count_with_run_metadata():
    valid = _run(task="one", mode="baseline", cost=1.0, correct=True)
    failed = {**valid, "error": "boom"}

    report = analyze.generate_report([valid, failed])

    assert "**Runs:** 1 valid (1 errors) | **Models:**" in report


def test_headline_reports_baseline_zero_correct_without_dividing():
    records = [
        _run(task="one", mode="baseline", cost=1.0, correct=False),
        _run(task="one", mode="tilth", cost=1.0, correct=True),
    ]

    report = analyze.generate_report(records)

    assert "n/a (baseline 0-correct)" in report
    assert analyze.paired_cpc_delta(records, "tilth") == (None, None, None)

def test_report_renders_flag_counts_by_mode():
    report = analyze.generate_report([
        _run(task="budget", mode="baseline", cost=1.0, correct=False),
    ])

    assert "Flag counts by mode: baseline=1" in report
    assert "| budget_exhausted |" in report


def test_capability_and_control_sections_use_record_capability():
    records = [
        _run(task="locate-task", mode="baseline", cost=1.0, correct=True, capability="locate"),
        _run(task="locate-task", mode="tilth", cost=2.0, correct=True, capability="locate"),
        _run(task="control-task", mode="baseline", cost=1.0, correct=True, capability="control"),
        _run(task="control-task", mode="tilth", cost=1.5, correct=False, capability="control"),
    ]

    report = analyze.generate_report(records)

    assert "## Capability breakdown" in report
    assert "| locate |" in report
    assert "| control |" in report
    assert "## Control-task delta" in report
    assert "tilth-added" in report
    assert "-100pp" in report



def test_legacy_record_capability_falls_back_to_task_registry(monkeypatch):
    class LegacyTask:
        capability = "trace"

    monkeypatch.setitem(analyze.TASKS, "legacy-task", LegacyTask())

    assert analyze.capability_for_record({"task": "legacy-task"}) == "trace"

def test_pairing_uses_task_model_and_repetition_keys():
    records = [
        _run(
            task="same",
            mode="baseline",
            model="sonnet",
            cost=1.0,
            correct=True,
            repetition=0,
        ),
        _run(
            task="same",
            mode="tilth",
            model="claude-sonnet-4-6",
            cost=2.0,
            correct=False,
            repetition=0,
        ),
        _run(
            task="same",
            mode="baseline",
            model="sonnet",
            cost=3.0,
            correct=True,
            repetition=1,
        ),
        _run(
            task="same",
            mode="tilth",
            model="claude-sonnet-4-6",
            cost=4.0,
            correct=True,
            repetition=2,
        ),
        _run(
            task="unpaired",
            mode="baseline",
            model="sonnet",
            cost=5.0,
            correct=True,
        ),
    ]

    assert paired.pair_ab(records) == {
        ("same", "claude-sonnet-4-6"): [(0, True, False, 1.0, 2.0)]
    }


def test_paired_cpc_delta_bootstraps_task_deltas_with_fixed_seed(monkeypatch):
    records = [
        _run(task="one", mode="baseline", model="haiku", cost=1.0, correct=True),
        _run(
            task="one",
            mode="tilth",
            model="claude-haiku-4-5-20251001",
            cost=2.0,
            correct=True,
        ),
        _run(
            task="two",
            mode="baseline",
            model="claude-haiku-4-5-20251001",
            cost=2.0,
            correct=True,
        ),
        _run(task="two", mode="tilth", model="haiku", cost=1.0, correct=True),
        _run(task="one", mode="baseline", model="sonnet", cost=3.0, correct=True),
        _run(
            task="one",
            mode="tilth",
            model="claude-sonnet-4-6",
            cost=2.0,
            correct=True,
        ),
        _run(task="unpaired", mode="baseline", model="haiku", cost=100.0, correct=True),
    ]
    observed: dict[str, object] = {}

    def fake_bootstrap(deltas, *, n_resamples, seed):
        observed["deltas"] = list(deltas)
        observed["n_resamples"] = n_resamples
        observed["seed"] = seed
        return (-0.25, 0.75)

    monkeypatch.setattr(paired.stats, "paired_bootstrap_ci", fake_bootstrap)

    result = paired.paired_cpc_delta(records, "tilth")

    assert observed == {
        "deltas": [0.0, -0.5],
        "n_resamples": 10_000,
        "seed": 0,
    }
    assert result[0] == pytest.approx(-1 / 6)
    assert result[1:] == (-0.25, 0.75)