"""Regression tests for benchmark model aliases and configured IDs."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from config import MODELS, RUNNERS


def test_claude_ids_are_pinned_snapshot_ids() -> None:
    assert MODELS["haiku"] == "claude-haiku-4-5-20251001"
    assert MODELS["sonnet"] == "claude-sonnet-4-6"
    assert MODELS["opus"] == "claude-opus-5"


def test_openai_frontier_id_is_pinned() -> None:
    assert MODELS["gpt5"] == "gpt-5.6-sol"


def test_runner_aliases_remain_stable() -> None:
    assert RUNNERS["haiku"] == "claude"
    assert RUNNERS["sonnet"] == "claude"
    assert RUNNERS["opus"] == "claude"
    assert RUNNERS["gpt5"] == "codex"
    assert RUNNERS["o3"] == "codex"
    assert RUNNERS["gpt5mini"] == "opencode"
