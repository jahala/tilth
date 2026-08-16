---
status: trusted
last_verified: 2026-08-14
confidence: high
sources:
  - /home/paul/.local/share/cheese/paulnsorensen-tilth/specs/tilth-search-v2-roadmap.md
  - src/mcp/tools/search.rs:36-302
  - src/mcp/tools/definitions.rs:448-513
---
# Deterministic Search v2 Contract

Search v2 accepts one ordered object batch and owns intent routing. Callers no longer provide `kind`, `expand`, or `context`; the server applies deterministic precedence and returns a structured JSON envelope as MCP text.

## Context

The current search surface supports per-query overrides and several routing branches (`src/mcp/tools/search.rs:36-302`). Analytics found invented values such as `kind: text`, target/tool-name confusion, and retries that repeat the same mistake. Multi-query batching is valuable and must survive the simplification.

## Decision

The canonical request is:

```text
{ cwd, queries: [{ query, path? }] x 1..10, budget? }
```

Each optional path independently accepts a file, directory, or glob. Input order is output order. Routing precedence is:

1. exact path or unique symbol;
2. literal content;
3. regex only for unmistakable regex syntax or an empty literal result;
4. fuzzy symbol/path candidates.

Responses are JSON text with top-level `results`, `hints`, and `diagnostics`. Every query reports `resolved_as`, status, completeness, and typed hints. `routes_tried` is telemetry-only.

## Alternatives rejected

- **Public kinds or prefixes:** moves classification back to the caller and preserves adoption failures.
- **Learned/heuristic classifier:** makes routing nondeterministic and harder to benchmark or debug.
- **Single-query-only API:** sacrifices the existing high-value batching behavior.
- **Prose-only output:** forces clients to reinterpret diagnostics and follow-up conditions.

## Consequences

The server bears routing complexity but gains deterministic tests and attributable telemetry. JSON can add token overhead, so graduation includes a result-token non-regression gate. Expensive graph work remains opt-in through typed continuations rather than unconditional expansion.
