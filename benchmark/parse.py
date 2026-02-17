import json
from dataclasses import dataclass, field
from typing import Any


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


def parse_stream_json(raw_output: str) -> RunResult:
    """Parse newline-delimited JSON output from claude -p --output-format stream-json --verbose."""
    lines = [line.strip() for line in raw_output.strip().split("\n") if line.strip()]
    events = [json.loads(line) for line in lines]

    session_id = ""
    turns: list[Turn] = []
    result_text = ""
    final_summary = {}
    turn_index = 0

    for event in events:
        event_type = event.get("type")

        if event_type == "system":
            session_id = event.get("session_id", "")

        elif event_type == "assistant":
            message = event.get("message", {})
            usage = message.get("usage", {})
            content_blocks = message.get("content", [])

            tool_calls: list[ToolCall] = []
            text_blocks: list[str] = []

            for block in content_blocks:
                if block.get("type") == "tool_use":
                    tool_calls.append(ToolCall(
                        name=block.get("name", ""),
                        input=block.get("input", {}),
                        tool_use_id=block.get("id", ""),
                        turn_index=turn_index,
                    ))
                elif block.get("type") == "text":
                    text_blocks.append(block.get("text", ""))

            turn = Turn(
                index=turn_index,
                input_tokens=usage.get("input_tokens", 0),
                output_tokens=usage.get("output_tokens", 0),
                cache_creation_tokens=usage.get("cache_creation_input_tokens", 0),
                cache_read_tokens=usage.get("cache_read_input_tokens", 0),
                tool_calls=tool_calls,
            )
            turns.append(turn)
            turn_index += 1

            if text_blocks:
                result_text = "\n".join(text_blocks)

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
        total_cache_creation_tokens=final_summary.get("usage", {}).get("cache_creation_input_tokens", 0),
        total_cache_read_tokens=final_summary.get("usage", {}).get("cache_read_input_tokens", 0),
        result_text=result_text,
    )


def parse_codex_json(raw_output: str, model_id: str) -> RunResult:
    """Parse newline-delimited JSON output from codex exec --json."""
    lines = [line.strip() for line in raw_output.strip().split("\n") if line.strip()]
    events = [json.loads(line) for line in lines]

    # Pricing per 1M tokens (approximate)
    pricing = {
        "gpt-5-codex": {"input": 2.00, "cached": 0.50, "output": 8.00},
        "o3": {"input": 2.00, "cached": 0.50, "output": 8.00},
    }
    rates = pricing.get(model_id, pricing["gpt-5-codex"])

    session_id = ""
    result_text = ""
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

            # Extract final message text
            if item.get("type") == "agent_message":
                result_text = item.get("text", "")

        elif event_type == "turn.completed":
            usage = event.get("usage", {})
            turn_usages.append(usage)

    # Build turns
    turns: list[Turn] = []
    for turn_idx in sorted(turn_items.keys()):
        items = turn_items[turn_idx]
        usage = turn_usages[turn_idx] if turn_idx < len(turn_usages) else {}

        input_tokens = usage.get("input_tokens", 0)
        cached_input_tokens = usage.get("cached_input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)

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
            cache_creation_tokens=0,  # codex doesn't report this
            cache_read_tokens=cached_input_tokens,
            tool_calls=tool_calls,
        )
        turns.append(turn)

    # Compute totals
    total_input = sum(u.get("input_tokens", 0) for u in turn_usages)
    total_cached = sum(u.get("cached_input_tokens", 0) for u in turn_usages)
    total_output = sum(u.get("output_tokens", 0) for u in turn_usages)

    # Calculate cost
    cost_usd = (
        (total_input * rates["input"] / 1_000_000) +
        (total_cached * rates["cached"] / 1_000_000) +
        (total_output * rates["output"] / 1_000_000)
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
        total_cache_creation_tokens=0,
        total_cache_read_tokens=total_cached,
        result_text=result_text,
    )


def tool_call_counts(result: RunResult) -> dict[str, int]:
    """Count tool calls by name across all turns."""
    counts: dict[str, int] = {}
    for turn in result.turns:
        for tool_call in turn.tool_calls:
            counts[tool_call.name] = counts.get(tool_call.name, 0) + 1
    return counts
