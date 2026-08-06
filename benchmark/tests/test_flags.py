import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from flags import detect_flags


def _run(
    mode="baseline",
    *,
    cost=0.1,
    correct=True,
    output_tokens=10,
    num_tool_calls=1,
    per_turn_output_tokens=None,
    task="task",
    error=None,
):
    record = {
        "task": task,
        "model": "model",
        "mode": mode,
        "repetition": 0,
        "total_cost_usd": cost,
        "correct": correct,
        "output_tokens": output_tokens,
        "num_tool_calls": num_tool_calls,
    }
    if per_turn_output_tokens is not None:
        record["per_turn_output_tokens"] = per_turn_output_tokens
    if error is not None:
        record["error"] = error
    return record


def _names(records):
    return {flag["flag"] for flag in detect_flags(records)}


def test_each_required_flag_positive_case():
    records = [
        _run(task="snowball", per_turn_output_tokens=[100, 120, 150]),
        _run(task="talkative", correct=False, output_tokens=100),
        _run(task="tool-storm", correct=False, num_tool_calls=100),
        _run(task="budget", cost=1.0),
        _run(task="timeout", error="timeout"),
        _run(task="output-1", output_tokens=10),
        _run(task="output-2", output_tokens=20),
        _run(task="output-3", output_tokens=30),
        _run(task="output-4", output_tokens=40),
        _run(task="tools-1", num_tool_calls=1),
        _run(task="tools-2", num_tool_calls=2),
        _run(task="tools-3", num_tool_calls=3),
        _run(task="tools-4", num_tool_calls=4),
    ]

    assert _names(records) == {
        "token_snowball",
        "talkative_failure",
        "tool_storm",
        "budget_exhausted",
        "timeout",
    }


def test_required_flags_negative_cases_and_missing_turn_data():
    records = [
        _run(task="snowball", per_turn_output_tokens=[100, 109]),
        _run(task="talkative", correct=False, output_tokens=40),
        _run(task="tool-storm", correct=False, num_tool_calls=4),
        _run(task="budget", cost=0.99),
        _run(task="timeout", error="failed"),
        _run(task="output-1", output_tokens=10),
        _run(task="output-2", output_tokens=20),
        _run(task="output-3", output_tokens=30),
        _run(task="output-4", output_tokens=40),
        _run(task="tools-1", num_tool_calls=1),
        _run(task="tools-2", num_tool_calls=2),
        _run(task="tools-3", num_tool_calls=3),
        _run(task="tools-4", num_tool_calls=4),
    ]

    assert detect_flags(records) == []

def test_percentile_equality_and_missing_data_do_not_trigger_flags():
    records = [
        _run(task="output-equal-a", correct=False, output_tokens=100, num_tool_calls=0),
        _run(task="output-equal-b", correct=False, output_tokens=100, num_tool_calls=0),
        _run(task="tools-equal-a", correct=False, output_tokens=1, num_tool_calls=4),
        _run(task="tools-equal-b", correct=False, output_tokens=1, num_tool_calls=4),
        {
            "task": "missing-evidence",
            "model": "model",
            "mode": "baseline",
            "repetition": 0,
            "correct": False,
        },
    ]

    assert detect_flags(records) == []
