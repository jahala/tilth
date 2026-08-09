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
    assert "Task-clustered paired accuracy" in report
    assert "McNemar" not in report
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


def test_paired_accuracy_delta_weights_tasks_not_repetitions(monkeypatch):
    records = []
    for repetition in range(10):
        records.extend([
            _run(
                task="many-reps",
                mode="upstream",
                cost=1.0,
                correct=False,
                repetition=repetition,
            ),
            _run(
                task="many-reps",
                mode="fork",
                cost=1.0,
                correct=True,
                repetition=repetition,
            ),
        ])
    records.extend([
        _run(task="one-rep", mode="upstream", cost=1.0, correct=True),
        _run(task="one-rep", mode="fork", cost=1.0, correct=False),
    ])
    observed: dict[str, object] = {}

    def fake_bootstrap(deltas, *, n_resamples, seed):
        observed["deltas"] = list(deltas)
        observed["n_resamples"] = n_resamples
        observed["seed"] = seed
        return (-0.75, 0.75)

    monkeypatch.setattr(paired.stats, "paired_bootstrap_ci", fake_bootstrap)

    result = paired.paired_accuracy_delta(records, "fork", "upstream")

    assert observed == {
        "deltas": [1.0, -1.0],
        "n_resamples": 10_000,
        "seed": 0,
    }
    assert result == (0.0, -0.75, 0.75, 2)


def test_three_way_report_prioritizes_fork_upstream_and_guardrails():
    records = [
        _run(task=task, mode=mode, cost=1.0, correct=correct)
        for task, outcomes in {
            "one": {"no_tilth": False, "upstream": True, "fork": True},
            "two": {"no_tilth": True, "upstream": False, "fork": True},
        }.items()
        for mode, correct in outcomes.items()
    ]

    report = analyze.generate_report(records)

    assert "fork vs upstream" in report
    assert "fork vs no_tilth" in report
    assert "upstream vs no_tilth" in report
    assert "task is the sampling unit" in report


def test_failure_type_covers_runner_and_grader_reasons():
    assert analyze._failure_type({"error": "timeout after 10m"}) == ("runner_error", "timeout")
    assert analyze._failure_type({"correctness_reason": "Contains forbidden: TODO"}) == (
        "forbidden_literal",
        "TODO",
    )
    assert analyze._failure_type({"correctness_reason": "Test failed: pytest"}) == (
        "test_failed",
        "pytest",
    )
    assert analyze._failure_type({"correctness_reason": "No changes in target file"}) == (
        "missing_edit",
        "No changes in target file",
    )


def test_failure_taxonomy_reports_types_transitions_and_tool_signal():
    baseline = _run(task="one", mode="baseline", cost=1.0, correct=False)
    baseline["correctness_reason"] = "Missing: exact-symbol"
    tilth = _run(task="one", mode="tilth", cost=1.0, correct=False)
    tilth["correctness_reason"] = "Missing: exact-symbol"
    tilth["tool_calls"] = {"mcp__tilth__tilth_search": 2}
    passed_baseline = _run(task="two", mode="baseline", cost=1.0, correct=True)
    passed_tilth = _run(task="two", mode="tilth", cost=1.0, correct=True)

    report = analyze.generate_report([baseline, tilth, passed_baseline, passed_tilth])

    assert report.index("## Failure taxonomy") < report.index("## Context Efficiency")
    assert "| missing_required_literal | exact-symbol | 1 | 1 | one |" in report
    assert "| tilth-added vs baseline | 1 | 0 | 0 | 1 |" in report
    assert "| tilth-added | 1 | 1 | 2 |" in report


def test_tool_usage_section_reports_per_arm_counts_and_availability():
    """The tool-usage table must separate tilth from native calls per arm and
    expose whether tilth was actually available — an arm with tilth attached
    but zero availability is a misconfigured comparison, not evidence."""
    baseline = _run(task="one", mode="baseline", cost=1.0, correct=True)
    baseline["tool_calls"] = {"Grep": 3, "Read": 2}
    baseline["available_tools"] = ["Bash", "Grep", "Read"]
    tilth = _run(task="one", mode="tilth", cost=1.0, correct=True)
    # Bare server-side names (e.g. from the mcp-shortening fork) must still
    # count as tilth calls alongside the registered mcp__tilth__ alias.
    tilth["tool_calls"] = {"Read": 1, "mcp__tilth__tilth_search": 4, "tilth_read": 2}
    tilth["available_tools"] = ["Read", "mcp__tilth__tilth_search"]
    legacy = _run(task="one", mode="tilth", cost=1.0, correct=True, repetition=1)
    legacy["tool_calls"] = {"Grep": 1}

    report = analyze.generate_report([baseline, tilth, legacy])

    assert "## Tool usage" in report
    assert "| Grep | 3 | 1 |" in report
    assert "| mcp__tilth__tilth_search | 0 | 4 |" in report
    assert "| tilth_read | 0 | 2 |" in report
    assert "| **Native subtotal** | 5 | 2 |" in report
    assert "| **Tilth subtotal** | 0 | 6 |" in report
    assert "| **Total** | 5 | 8 |" in report
    # Availability: baseline 0/1 recorded rows have tilth; tilth arm 1/1
    # recorded (the legacy row lacks the field and only shrinks the known
    # denominator); one tilth cell made >=1 tilth call.
    assert "| baseline | 1 | 0/1 | 0 |" in report
    assert "| tilth-added | 2 | 1/1 | 1 |" in report
    # No row carries batch_sizes -> the batching table degrades explicitly
    # instead of rendering zeros.
    assert "_Batch sizes unrecorded" in report


def test_batching_table_reports_multi_item_share_per_arm():
    """The batching table is the needle for batch-instruction experiments:
    calls / multi-item / share / max per batchable tool, per arm. A mode whose
    rows never recorded batch sizes renders "unrecorded", never "-" — the two
    support opposite conclusions."""
    baseline = _run(task="one", mode="baseline", cost=1.0, correct=True)
    baseline["tool_calls"] = {"Read": 2}
    baseline["batch_sizes"] = {}
    tilth = _run(task="one", mode="tilth", cost=1.0, correct=True)
    tilth["tool_calls"] = {"mcp__tilth__tilth_read": 3}
    tilth["batch_sizes"] = {"mcp__tilth__tilth_read": [1, 4, 1],
                            "mcp__tilth__tilth_search": [1, 1]}
    legacy = _run(task="one", mode="tilth_forced", cost=1.0, correct=True)
    legacy["tool_calls"] = {"mcp__tilth__tilth_read": 5}

    report = analyze.generate_report([baseline, tilth, legacy])

    assert "**Batching**" in report
    assert "| mcp__tilth__tilth_read | - | 3 / 1 / 33% / 4 | unrecorded |" in report
    assert "| mcp__tilth__tilth_search | - | 2 / 0 / 0% / 1 | unrecorded |" in report
    assert "_Batch sizes unrecorded" not in report


def test_op_kinds_table_reports_counts_per_arm():
    """Op-kind capture is the needle for replace_text adoption: per-arm counts
    by op kind, with the unrecorded/subset-of-rows degradation matching the
    batch-size table's semantics."""
    baseline = _run(task="one", mode="baseline", cost=1.0, correct=True)
    baseline["op_kinds"] = {}
    tilth = _run(task="one", mode="tilth", cost=1.0, correct=True)
    tilth["op_kinds"] = {"replace_text": 5, "replace": 2}
    legacy = _run(task="one", mode="tilth_forced", cost=1.0, correct=True)

    report = analyze.generate_report([baseline, tilth, legacy])

    assert "**Op kinds**" in report
    assert "| replace | 0 | 2 | unrecorded |" in report
    assert "| replace_text | 0 | 5 | unrecorded |" in report


def test_per_task_tools_used_totals_and_native_cost_reconciliation():
    """Per-task sections must show which tools each arm called (totals, so a
    tool used in one of three reps still appears) and reconcile the computed
    category breakdown against the runner's native caching-aware total."""
    runs = []
    for repetition, tilth_calls in enumerate([1, 0, 0]):
        run = _run(task="one", mode="tilth", cost=1.0, correct=True,
                   repetition=repetition, model="claude-sonnet-5")
        run["tool_calls"] = {"Read": 2}
        if tilth_calls:
            run["tool_calls"]["mcp__tilth__tilth_search"] = tilth_calls
        runs.append(run)
    # Native total deliberately above the computed sum: the residual line
    # must surface the gap instead of hiding it.
    for run in runs:
        run["model_usage"] = {
            "claude-sonnet-5": {"costUSD": 0.9},
            "claude-haiku-4-5-20251001": {"costUSD": 0.1},
        }
    baseline = _run(task="one", mode="baseline", cost=1.0, correct=True,
                    model="claude-sonnet-5")
    baseline["tool_calls"] = {"Grep": 4}

    report = analyze.generate_report(runs + [baseline])

    assert "**Tools used (total calls across reps):**" in report
    assert "tilth-added: Read=6, mcp__tilth__tilth_search=1" in report
    assert "baseline   : Grep=4" in report  # label ljust-padded to "tilth-added"
    assert "Tool breakdown (median counts)" not in report
    assert "native=$1.0000" in report
    assert "Δnative=" in report
    assert "native per-model: claude-haiku-4-5-20251001=$0.1000, claude-sonnet-5=$0.9000" in report