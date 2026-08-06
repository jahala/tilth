import json
from datetime import date
from pathlib import Path
from typing import Any


# Prices are USD per million tokens. The .yaml file is deliberately JSON-shaped
# so the benchmark stays stdlib-only.
PRICING_PATH = Path(__file__).with_name("pricing.yaml")


def load_pricing(path: Path | str = PRICING_PATH) -> dict[str, Any]:
    """Load and lightly validate the versioned pricing table."""
    path = Path(path)
    table = json.loads(path.read_text())
    if not isinstance(table, dict) or not isinstance(table.get("models"), dict):
        raise ValueError(f"invalid pricing table: {path}")
    if not isinstance(table.get("as_of"), str):
        raise ValueError(f"pricing table has no as_of date: {path}")
    table.setdefault("aliases", {})
    return table


PRICING_DATA = load_pricing()
PRICING = PRICING_DATA["models"]


def _token_value(run: dict, *keys: str) -> float:
    for key in keys:
        value = run.get(key)
        if isinstance(value, (int, float)):
            return float(value)
    return 0.0


def _pricing_rates(run: dict) -> dict[str, Any]:
    models = PRICING_DATA["models"]
    aliases = PRICING_DATA.get("aliases", {})
    candidate = run.get("model") or run.get("model_alias")
    model_id = aliases.get(candidate, candidate)
    if model_id not in models:
        raise ValueError(f"no pricing entry for model {candidate!r}")
    return models[model_id]


def _usage_costs(usage: dict, rates: dict[str, Any]) -> dict[str, float]:
    input_tokens = _token_value(usage, "input_tokens", "total_input_tokens")
    cache_creation_tokens = _token_value(
        usage,
        "cache_write_tokens",
        "cache_creation_tokens",
        "total_cache_creation_tokens",
    )
    cache_creation_5m_tokens = _token_value(usage, "cache_creation_5m_tokens")
    cache_creation_1h_tokens = _token_value(usage, "cache_creation_1h_tokens")
    classified_cache_creation_tokens = (
        cache_creation_5m_tokens + cache_creation_1h_tokens
    )
    cache_read_tokens = _token_value(
        usage,
        "cache_read_tokens",
        "total_cache_read_tokens",
    )
    output_tokens = _token_value(usage, "output_tokens", "total_output_tokens")

    threshold = rates.get("long_context_threshold")
    context_tokens = (
        input_tokens
        + max(cache_creation_tokens, classified_cache_creation_tokens)
        + cache_read_tokens
    )
    if threshold is not None and context_tokens > threshold:
        rates = rates["long_context"]

    cache_creation_rate = rates.get("cache_creation", rates.get("cache_write", 0.0))
    cache_creation_5m_rate = rates.get("cache_creation_5m", cache_creation_rate)
    cache_creation_1h_rate = rates.get("cache_creation_1h", cache_creation_rate)
    unclassified_cache_creation_tokens = max(
        cache_creation_tokens - classified_cache_creation_tokens,
        0.0,
    )
    cache_creation_cost = (
        unclassified_cache_creation_tokens * cache_creation_rate
        + cache_creation_5m_tokens * cache_creation_5m_rate
        + cache_creation_1h_tokens * cache_creation_1h_rate
    ) / 1_000_000
    return {
        "cache_creation_cost": cache_creation_cost,
        "cache_read_cost": cache_read_tokens * rates["cache_read"] / 1_000_000,
        "output_cost": output_tokens * rates["output"] / 1_000_000,
        "input_cost": input_tokens * rates["input"] / 1_000_000,
    }


def compute_cost_breakdown(run: dict) -> dict[str, float]:
    """Compute model-specific cost by input/cache-write/cache-read/output."""
    rates = _pricing_rates(run)
    per_turn_usage = run.get("per_turn_token_usage")
    if not isinstance(per_turn_usage, list) or "long_context_threshold" not in rates:
        return _usage_costs(run, rates)

    totals = {
        "cache_creation_cost": 0.0,
        "cache_read_cost": 0.0,
        "output_cost": 0.0,
        "input_cost": 0.0,
    }
    for usage in per_turn_usage:
        for category, cost in _usage_costs(usage, rates).items():
            totals[category] += cost
    return totals


def pricing_staleness_warning(as_of: str, today: date | None = None) -> str | None:
    """Return a report warning when pricing is more than 30 days old."""
    published = date.fromisoformat(as_of)
    age = ((today or date.today()) - published).days
    if age > 30:
        return f"WARNING: pricing table as_of {as_of} is {age} days old (>30 days)."
    return None
