"""Tests for benchmark/parse.py — stream-json → RunResult parsing."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from parse import parse_stream_json


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


def test_result_text_accumulates_across_all_assistant_turns():
    """A substantive answer in an earlier turn must survive even when a later
    turn is a short wrap-up — grading reads result_text, so losing the
    earlier turn silently discards the real answer (docs/research/
    grading-audit-2026-07.md §5)."""
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


def test_result_text_single_turn_unchanged():
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
