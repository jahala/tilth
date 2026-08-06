import json
import sys
from dataclasses import dataclass, field
from typing import Any, Optional

from pricing import compute_cost_breakdown


@dataclass
class ToolCall:
    """Represents a single tool invocation."""
    name: str
    input: dict[str, Any]
    tool_use_id: str
    turn_index: int


@dataclass
class Turn:
    """Represents one assistant turn with usage and tool calls."""
    index: int
    input_tokens: int
    output_tokens: int
    cache_creation_tokens: int
    cache_read_tokens: int
    cache_creation_5m_tokens: int = 0
    cache_creation_1h_tokens: int = 0
    tool_calls: list[ToolCall] = field(default_factory=list)

    @property
    def context_tokens(self) -> int:
        """Total context processed this turn (input + cached)."""
        return self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens


@dataclass
class RunResult:
    """Complete parsed result from a claude -p run."""
    session_id: str
    turns: list[Turn]
    num_turns: int
    total_cost_usd: float
    duration_ms: int
    duration_api_ms: int
    total_input_tokens: int
    total_output_tokens: int
    total_cache_creation_tokens: int
    total_cache_read_tokens: int
    result_text: str
    task_name: str = ""
    mode_name: str = ""
    model_name: str = ""
    repetition: int = 0
    correct: bool = False
    correctness_reason: str = ""


def _cache_creation_ttl_tokens(usage: dict) -> tuple[int, int]:
    cache_creation = usage.get("cache_creation")
    if not isinstance(cache_creation, dict):
        return 0, 0
    return (
        cache_creation.get("ephemeral_5m_input_tokens", 0),
        cache_creation.get("ephemeral_1h_input_tokens", 0),
    )


def parse_stream_json(raw_output: str) -> RunResult:
    """Parse newline-delimited JSON output from claude -p --output-format stream-json --verbose."""
    lines = [line.strip() for line in raw_output.strip().split("\n") if line.strip()]
    events = [json.loads(line) for line in lines]

    session_id = ""
    turns: list[Turn] = []
    turns_by_message_id: dict[str, Turn] = {}
    result_text_parts: list[str] = []
    final_summary = {}

    for event in events:
        event_type = event.get("type")

        if event_type == "system":
            session_id = event.get("session_id", "")

        elif event_type == "assistant":
            message = event.get("message", {})
            usage = message.get("usage", {})
            message_id = message.get("id")
            turn = (
                turns_by_message_id.get(message_id)
                if isinstance(message_id, str) and message_id
                else None
            )

            if turn is None:
                turn = Turn(
                    index=len(turns),
                    input_tokens=0,
                    output_tokens=0,
                    cache_creation_tokens=0,
                    cache_read_tokens=0,
                )
                turns.append(turn)
                if isinstance(message_id, str) and message_id:
                    turns_by_message_id[message_id] = turn

            if usage:
                (
                    turn.cache_creation_5m_tokens,
                    turn.cache_creation_1h_tokens,
                ) = _cache_creation_ttl_tokens(usage)
                turn.input_tokens = usage.get("input_tokens", 0)
                turn.output_tokens = usage.get("output_tokens", 0)
                turn.cache_creation_tokens = usage.get(
                    "cache_creation_input_tokens",
                    0,
                )
                turn.cache_read_tokens = usage.get("cache_read_input_tokens", 0)

            for block in message.get("content", []):
                if block.get("type") == "tool_use":
                    tool_use_id = block.get("id", "")
                    if not any(
                        call.tool_use_id == tool_use_id for call in turn.tool_calls
                    ):
                        turn.tool_calls.append(ToolCall(
                            name=block.get("name", ""),
                            input=block.get("input", {}),
                            tool_use_id=tool_use_id,
                            turn_index=turn.index,
                        ))
                elif block.get("type") == "text":
                    result_text_parts.append(block.get("text", ""))

        elif event_type == "result":
            final_summary = event

    return RunResult(
        session_id=session_id,
        turns=turns,
        num_turns=final_summary.get("num_turns", len(turns)),
        total_cost_usd=final_summary.get("total_cost_usd", 0.0),
        duration_ms=final_summary.get("duration_ms", 0),
        duration_api_ms=final_summary.get("duration_api_ms", 0),
        total_input_tokens=final_summary.get("usage", {}).get("input_tokens", 0),
        total_output_tokens=final_summary.get("usage", {}).get("output_tokens", 0),
        total_cache_creation_tokens=final_summary.get("usage", {}).get(
            "cache_creation_input_tokens",
            0,
        ),
        total_cache_read_tokens=final_summary.get("usage", {}).get(
            "cache_read_input_tokens",
            0,
        ),
        result_text="\n".join(result_text_parts),
    )


def parse_codex_json(raw_output: str, model_id: str) -> RunResult:
    """Parse newline-delimited JSON output from codex exec --json."""
    lines = [line.strip() for line in raw_output.strip().split("\n") if line.strip()]
    events = [json.loads(line) for line in lines]

    session_id = ""
    result_text_parts: list[str] = []
    turn_items: dict[int, list] = {}  # turn_index -> items in that turn
    current_turn = -1
    turn_usages: list[dict] = []

    # Collect events
    for event in events:
        event_type = event.get("type")

        if event_type == "thread.started":
            session_id = event.get("thread_id", "")

        elif event_type == "turn.started":
            current_turn += 1
            turn_items[current_turn] = []

        elif event_type == "item.completed":
            item = event.get("item", {})
            if current_turn >= 0:
                turn_items[current_turn].append(item)

            # Accumulate assistant message text across turns
            if item.get("type") == "agent_message":
                text = item.get("text", "")
                if text:
                    result_text_parts.append(text)

        elif event_type == "turn.completed":
            usage = event.get("usage", {})
            turn_usages.append(usage)

    # Build turns
    turns: list[Turn] = []
    for turn_idx in sorted(turn_items.keys()):
        items = turn_items[turn_idx]
        usage = turn_usages[turn_idx] if turn_idx < len(turn_usages) else {}

        reported_input_tokens = usage.get("input_tokens") or 0
        cached_input_tokens = usage.get("cached_input_tokens") or 0
        cache_write_input_tokens = (
            usage.get("cache_write_input_tokens")
            or usage.get("cache_write_tokens")
            or 0
        )
        input_tokens = max(
            reported_input_tokens
            - cached_input_tokens
            - cache_write_input_tokens,
            0,
        )
        output_tokens = usage.get("output_tokens") or 0

        # Build tool calls from items
        tool_calls: list[ToolCall] = []
        for item in items:
            item_type = item.get("type")
            item_id = item.get("id", "")

            if item_type == "command_execution":
                tool_calls.append(ToolCall(
                    name="Bash",
                    input={"command": item.get("command", "")},
                    tool_use_id=item_id,
                    turn_index=turn_idx,
                ))

            elif item_type == "mcp_tool_call":
                tool_name = item.get("tool", "unknown")
                tool_calls.append(ToolCall(
                    name=tool_name,
                    input=item.get("arguments", {}),
                    tool_use_id=item_id,
                    turn_index=turn_idx,
                ))

            elif item_type == "file_edit":
                tool_calls.append(ToolCall(
                    name="Edit",
                    input={"file_path": item.get("file_path", "")},
                    tool_use_id=item_id,
                    turn_index=turn_idx,
                ))

            elif item_type == "file_write":
                tool_calls.append(ToolCall(
                    name="Write",
                    input={"file_path": item.get("file_path", "")},
                    tool_use_id=item_id,
                    turn_index=turn_idx,
                ))

        turn = Turn(
            index=turn_idx,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            cache_creation_tokens=cache_write_input_tokens,
            cache_read_tokens=cached_input_tokens,
            tool_calls=tool_calls,
        )
        turns.append(turn)

    # The API reports cached and cache-write tokens as subsets of input_tokens.
    total_input = sum(turn.input_tokens for turn in turns)
    total_cache_creation = sum(turn.cache_creation_tokens for turn in turns)
    total_cached = sum(turn.cache_read_tokens for turn in turns)
    total_output = sum(turn.output_tokens for turn in turns)

    per_turn_usage = [
        {
            "input_tokens": turn.input_tokens,
            "cache_creation_tokens": turn.cache_creation_tokens,
            "cache_read_tokens": turn.cache_read_tokens,
            "output_tokens": turn.output_tokens,
        }
        for turn in turns
    ]
    cost_usd = sum(
        compute_cost_breakdown({
            "model": model_id,
            "per_turn_token_usage": per_turn_usage,
        }).values()
    )

    return RunResult(
        session_id=session_id,
        turns=turns,
        num_turns=len(turn_usages),
        total_cost_usd=cost_usd,
        duration_ms=0,  # set by caller from subprocess timing
        duration_api_ms=0,
        total_input_tokens=total_input,
        total_output_tokens=total_output,
        total_cache_creation_tokens=total_cache_creation,
        total_cache_read_tokens=total_cached,
        result_text="\n".join(result_text_parts),
    )


def _parse_opencode_events(raw_output: str) -> list[dict]:
    """opencode `run --format json` emits NDJSON; tolerate a JSON array too."""
    raw_output = raw_output.strip()
    if not raw_output:
        return []
    try:
        parsed = json.loads(raw_output)
        return parsed if isinstance(parsed, list) else [parsed]
    except json.JSONDecodeError:
        pass
    events = []
    skipped = 0
    for line in raw_output.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            skipped += 1
    if skipped:
        # Fail loud, not silent: a truncated run (e.g. killed at the timeout)
        # would otherwise parse as a clean cell with undercounted cost/tokens.
        print(
            f"[parse_opencode] WARNING: skipped {skipped} malformed JSON "
            f"line(s); cost/token totals may be undercounted (truncated run?).",
            file=sys.stderr,
        )
    return events


def parse_opencode_json(raw_output: str) -> RunResult:
    """Parse NDJSON from `opencode run --format json` (OpenRouter-backed).

    Schema (one event per line): {type, part, sessionID, timestamp}. A step is
    bracketed by step_start/step_finish; tool_use and text events fall between.
    `step-finish` parts carry PER-STEP `cost` and `tokens` — summing them is the
    whole point. A last-write-wins read under-reports cost by the step count.
    Cost is the real OpenRouter charge, so no local pricing table is needed.
    """
    events = _parse_opencode_events(raw_output)

    session_id = ""
    result_text_parts: list[str] = []
    turns: list[Turn] = []
    pending_calls: list[ToolCall] = []
    turn_index = 0

    for event in events:
        if not session_id:
            session_id = event.get("sessionID", "")
        event_type = event.get("type")
        part = event.get("part", {})

        if event_type == "tool_use":
            # opencode namespaces MCP tools as "<server>_<tool>" -> "tilth_tilth_search";
            # strip the duplicate server prefix so it aggregates with the claude/codex key.
            tool_name = part.get("tool", "unknown")
            if tool_name.startswith("tilth_tilth_"):
                tool_name = tool_name[len("tilth_"):]
            pending_calls.append(ToolCall(
                name=tool_name,
                input=part.get("state", {}).get("input", {}),
                tool_use_id=part.get("callID", part.get("id", "")),
                turn_index=turn_index,
            ))

        elif event_type == "text":
            text = part.get("text", "")
            if isinstance(text, str) and text:
                result_text_parts.append(text)

        elif event_type == "step_finish":
            tokens = part.get("tokens") or {}
            cache = tokens.get("cache") or {}
            turns.append(Turn(
                index=turn_index,
                input_tokens=tokens.get("input") or 0,
                output_tokens=tokens.get("output") or 0,
                cache_creation_tokens=cache.get("write") or 0,
                cache_read_tokens=cache.get("read") or 0,
                tool_calls=pending_calls,
            ))
            pending_calls = []
            turn_index += 1

    return RunResult(
        session_id=session_id,
        turns=turns,
        num_turns=len(turns),
        total_cost_usd=sum(
            (e.get("part", {}).get("cost") or 0.0)
            for e in events if e.get("type") == "step_finish"
        ),
        duration_ms=0,  # set by caller from subprocess timing
        duration_api_ms=0,
        total_input_tokens=sum(t.input_tokens for t in turns),
        total_output_tokens=sum(t.output_tokens for t in turns),
        total_cache_creation_tokens=sum(t.cache_creation_tokens for t in turns),
        total_cache_read_tokens=sum(t.cache_read_tokens for t in turns),
        result_text="\n".join(result_text_parts),
    )


def tool_call_counts(result: RunResult) -> dict[str, int]:
    """Count tool calls by name across all turns."""
    counts: dict[str, int] = {}
    for turn in result.turns:
        for tool_call in turn.tool_calls:
            counts[tool_call.name] = counts.get(tool_call.name, 0) + 1
    return counts


def extract_stream_error(stdout: str) -> Optional[str]:
    """Pull the human-readable error out of a codex/opencode JSON event stream.

    The real cause (e.g. a provider ContentFilterError) is emitted as a late
    ``{"type": "error", ...}`` event, often after many step events — a
    head-truncated stdout dump hides it. Returns "Name: message" for the last
    such event, or None if the stream carries no error event.
    """
    found: Optional[str] = None
    for line in stdout.splitlines():
        line = line.strip()
        if not line or '"error"' not in line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(obj, dict):
            continue
        if obj.get("type") != "error":
            continue
        err = obj.get("error") or {}
        name = err.get("name") or "error"
        msg = (err.get("data") or {}).get("message") or err.get("message") or ""
        found = f"{name}: {msg}".rstrip(": ").strip()
    return found
