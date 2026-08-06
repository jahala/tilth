"""Tests for benchmark/parse.py — transcript → RunResult parsing.

Each parser accumulates result_text across every assistant turn: a short
wrap-up turn at the end must add to, not replace, a substantive earlier turn.
Grading reads result_text (tasks/base.py check_correctness), so losing an
earlier turn silently discards the real answer.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from parse import parse_codex_json, parse_opencode_json, parse_stream_json


# --- stream-json (claude -p) ------------------------------------------------


def _assistant_event(text: str) -> dict:
    """Build a minimal stream-json 'assistant' event carrying one text block."""
    return {
        "type": "assistant",
        "message": {
            "usage": {
                "input_tokens": 10,
                "output_tokens": 10,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
            "content": [{"type": "text", "text": text}],
        },
    }


def _result_event() -> dict:
    return {"type": "result", "num_turns": 5, "total_cost_usd": 0.01}


def test_stream_json_result_text_accumulates_across_all_assistant_turns():
    """A substantive answer in an earlier turn must survive even when a later
    turn is a short wrap-up — grading reads result_text, so losing the earlier
    turn silently discards the real answer."""
    substantive = "The dependency resolution flow starts in get_dependant()."
    wrapup = "Let me know if you want more detail."

    events = [
        {"type": "system", "session_id": "abc123"},
        _assistant_event("Looking into the codebase now."),
        _assistant_event("Still investigating."),
        _assistant_event(substantive),
        _assistant_event("Just a status update, no new info."),
        _assistant_event(wrapup),
        _result_event(),
    ]
    raw_output = "\n".join(json.dumps(e) for e in events)

    result = parse_stream_json(raw_output)

    assert substantive in result.result_text
    assert wrapup in result.result_text


def test_stream_json_result_text_single_turn_unchanged():
    """A single-turn transcript's result_text is just that turn's text —
    accumulation must not introduce leading separators or duplication."""
    events = [
        {"type": "system", "session_id": "abc123"},
        _assistant_event("The answer is 42."),
        _result_event(),
    ]
    raw_output = "\n".join(json.dumps(e) for e in events)

    result = parse_stream_json(raw_output)

    assert result.result_text == "The answer is 42."


def test_stream_json_preserves_cache_creation_ttl_breakdown():
    event = _assistant_event("The answer is 42.")
    event["message"]["usage"].update({
        "cache_creation_input_tokens": 6_300,
        "cache_creation": {
            "ephemeral_5m_input_tokens": 300,
            "ephemeral_1h_input_tokens": 6_000,
        },
    })
    raw_output = "\n".join(json.dumps(e) for e in [event, _result_event()])

    result = parse_stream_json(raw_output)

    assert result.turns[0].cache_creation_tokens == 6_300
    assert result.turns[0].cache_creation_5m_tokens == 300
    assert result.turns[0].cache_creation_1h_tokens == 6_000


def test_stream_json_counts_each_message_usage_once():
    thinking = _assistant_event("")
    thinking["message"].update({
        "id": "msg_1",
        "content": [{"type": "thinking", "thinking": "working"}],
    })
    text = _assistant_event("The answer is 42.")
    text["message"]["id"] = "msg_1"
    raw_output = "\n".join(json.dumps(e) for e in [thinking, text, _result_event()])

    result = parse_stream_json(raw_output)

    assert len(result.turns) == 1
    assert result.turns[0].input_tokens == 10
    assert result.turns[0].output_tokens == 10


# --- codex exec --json ------------------------------------------------------


def _codex_agent_message(text: str) -> dict:
    return {"type": "item.completed", "item": {"type": "agent_message", "text": text}}


def test_codex_result_text_accumulates_across_all_assistant_turns():
    substantive = "The router dispatches through ServeHTTP in engine.go."
    wrapup = "Happy to dig deeper if needed."

    events = [
        {"type": "thread.started", "thread_id": "t1"},
        {"type": "turn.started"},
        _codex_agent_message(substantive),
        {"type": "turn.completed", "usage": {"input_tokens": 5, "output_tokens": 5}},
        {"type": "turn.started"},
        _codex_agent_message(wrapup),
        {"type": "turn.completed", "usage": {"input_tokens": 3, "output_tokens": 3}},
    ]
    raw_output = "\n".join(json.dumps(e) for e in events)

    result = parse_codex_json(raw_output, "gpt-5-codex")

    assert substantive in result.result_text
    assert wrapup in result.result_text


def test_codex_result_text_single_turn_unchanged():
    events = [
        {"type": "thread.started", "thread_id": "t1"},
        {"type": "turn.started"},
        _codex_agent_message("The answer is 42."),
        {"type": "turn.completed", "usage": {"input_tokens": 5, "output_tokens": 5}},
    ]
    raw_output = "\n".join(json.dumps(e) for e in events)

    result = parse_codex_json(raw_output, "gpt-5-codex")

    assert result.result_text == "The answer is 42."


def test_codex_usage_partitions_cached_and_cache_write_tokens():
    events = [
        {"type": "thread.started", "thread_id": "t1"},
        {"type": "turn.started"},
        {
            "type": "turn.completed",
            "usage": {
                "input_tokens": 10,
                "cached_input_tokens": 4,
                "cache_write_input_tokens": 3,
                "output_tokens": 2,
            },
        },
    ]

    result = parse_codex_json("\n".join(json.dumps(e) for e in events), "gpt-5.6-sol")

    assert result.turns[0].input_tokens == 3
    assert result.turns[0].cache_creation_tokens == 3
    assert result.turns[0].cache_read_tokens == 4
    assert result.turns[0].context_tokens == 10
    assert result.total_input_tokens == 3
    assert result.total_cache_creation_tokens == 3

    assert result.total_cost_usd == 0.00009575


def test_codex_gpt_5_6_cost_uses_short_and_long_context_rates_per_turn():
    events = [
        {"type": "thread.started", "thread_id": "t1"},
        {"type": "turn.started"},
        {
            "type": "turn.completed",
            "usage": {
                "input_tokens": 200_000,
                "cached_input_tokens": 100_000,
                "output_tokens": 100_000,
            },
        },
        {"type": "turn.started"},
        {
            "type": "turn.completed",
            "usage": {
                "input_tokens": 300_000,
                "cached_input_tokens": 200_000,
                "output_tokens": 100_000,
            },
        },
    ]

    result = parse_codex_json("\n".join(json.dumps(e) for e in events), "gpt-5.6-sol")

    assert result.total_cost_usd == 9.25


# --- opencode run --format json ---------------------------------------------


def _opencode_text(text: str) -> dict:
    return {"type": "text", "part": {"text": text}}


def _opencode_step_finish() -> dict:
    return {"type": "step_finish", "part": {"tokens": {"input": 5, "output": 5}}}


def test_opencode_result_text_accumulates_across_all_assistant_turns():
    substantive = "Search dispatch is classified in classify.rs."
    wrapup = "Let me know if that helps."

    events = [
        {"type": "text", "part": {"text": substantive}, "sessionID": "s1"},
        _opencode_step_finish(),
        _opencode_text(wrapup),
        _opencode_step_finish(),
    ]
    raw_output = "\n".join(json.dumps(e) for e in events)

    result = parse_opencode_json(raw_output)

    assert substantive in result.result_text
    assert wrapup in result.result_text


def test_opencode_result_text_single_turn_unchanged():
    events = [
        {"type": "text", "part": {"text": "The answer is 42."}, "sessionID": "s1"},
        _opencode_step_finish(),
    ]
    raw_output = "\n".join(json.dumps(e) for e in events)

    result = parse_opencode_json(raw_output)

    assert result.result_text == "The answer is 42."
