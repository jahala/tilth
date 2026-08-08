import sys
from datetime import date
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))
import analyze
import pricing


def _report_run():
    return {
        "task": "task",
        "mode": "baseline",
        "model": "haiku",
        "repetition": 0,
        "correct": True,
        "total_cost_usd": 1.0,
        "context_tokens": 1,
        "output_tokens": 1,
        "input_tokens": 1,
        "cache_creation_tokens": 1,
        "cache_read_tokens": 1,
        "num_turns": 1,
        "num_tool_calls": 0,
        "duration_ms": 1,
    }


def test_model_specific_breakdown_uses_all_token_categories():
    run = {
        "model": "claude-haiku-4-5-20251001",
        "input_tokens": 1_000_000,
        "cache_creation_tokens": 1_000_000,
        "cache_read_tokens": 1_000_000,
        "output_tokens": 1_000_000,
        "per_turn_token_usage": [{"output_tokens": 1}],
    }

    costs = pricing.compute_cost_breakdown(run)

    assert costs == {
        "input_cost": 1.0,
        "cache_creation_cost": 2.0,
        "cache_read_cost": 0.10,
        "output_cost": 5.0,
    }


@pytest.mark.parametrize(
    ("model", "expected"),
    [
        (
            "claude-sonnet-5",
            # Standard rates, not the dated introductory pricing: Claude Code's
            # native total_cost_usd bills sonnet-5 at $3/$15 (verified against
            # modelUsage.costUSD to the cent), and the computed breakdown must
            # reconcile with the native totals the report treats as primary.
            {
                "input": 3.0,
                "cache_creation": 6.0,
                "cache_creation_5m": 3.75,
                "cache_creation_1h": 6.0,
                "cache_read": 0.3,
                "output": 15.0,
            },
        ),
        (
            "claude-opus-5",
            {
                "input": 5.0,
                "cache_creation": 10.0,
                "cache_creation_5m": 6.25,
                "cache_creation_1h": 10.0,
                "cache_read": 0.5,
                "output": 25.0,
            },
        ),
        (
            "gpt-5.6-sol",
            {"input": 5.0, "cache_creation": 6.25, "cache_read": 0.5, "output": 30.0},
        ),
    ],
)
def test_current_frontier_model_short_context_pricing(
    model: str,
    expected: dict[str, float],
) -> None:
    assert {key: pricing.PRICING[model][key] for key in expected} == expected


def test_gpt_5_6_long_context_pricing_is_applied_per_turn() -> None:
    run = {
        "model": "gpt-5.6-sol",
        "per_turn_token_usage": [
            {
                "input_tokens": 100_000,
                "cache_creation_tokens": 100_000,
                "cache_read_tokens": 100_000,
                "output_tokens": 100_000,
            },
            {
                "input_tokens": 100_000,
                "cache_creation_tokens": 100_000,
                "cache_read_tokens": 0,
                "output_tokens": 100_000,
            },
        ],
    }

    assert pricing.compute_cost_breakdown(run) == {
        "cache_creation_cost": 1.875,
        "cache_read_cost": 0.1,
        "output_cost": 7.5,
        "input_cost": 1.5,
    }


def test_claude_cache_creation_uses_reported_ttl() -> None:
    run = {
        "model": "claude-opus-5",
        "input_tokens": 0,
        "cache_creation_tokens": 2_000_000,
        "cache_creation_5m_tokens": 1_000_000,
        "cache_creation_1h_tokens": 1_000_000,
        "cache_read_tokens": 0,
        "output_tokens": 0,
    }
    assert pricing.compute_cost_breakdown(run)["cache_creation_cost"] == 16.25


def test_alias_pricing_resolves_when_actual_model_id_is_absent():
    costs = pricing.compute_cost_breakdown({
        "model_alias": "haiku",
        "input_tokens": 1_000_000,
        "cache_creation_tokens": 1_000_000,
        "cache_read_tokens": 1_000_000,
        "output_tokens": 1_000_000,
    })

    assert costs == {
        "input_cost": 1.0,
        "cache_creation_cost": 2.0,
        "cache_read_cost": 0.10,
        "output_cost": 5.0,
    }


def test_actual_model_does_not_fall_back_to_alias_pricing():
    with pytest.raises(ValueError, match="no pricing entry"):
        pricing.compute_cost_breakdown({
            "model": "unpriced-model",
            "model_alias": "haiku",
            "input_tokens": 1,
        })


def test_stale_pricing_warns_only_after_thirty_days():
    assert pricing.pricing_staleness_warning("2026-06-01", today=date(2026, 8, 4))
    assert not pricing.pricing_staleness_warning("2026-07-05", today=date(2026, 8, 4))


def test_generated_report_renders_stale_pricing_warning(monkeypatch):
    monkeypatch.setitem(analyze.PRICING_DATA, "as_of", "2026-06-01")

    report = analyze.generate_report([_report_run()])

    assert "WARNING: pricing table as_of 2026-06-01" in report
