---
status: trusted
last_verified: 2026-08-14
confidence: high
sources:
  - /home/paul/.local/share/cheese/paulnsorensen-tilth/specs/tilth-search-v2-roadmap.md
  - benchmark/README.md:204-287
  - .cheese/research/tilth-api-analytics-verdict/report.md
---
# Measured Parallel Search v2 Trial

Tilth will advertise temporary `tilth_search_v2` beside the existing discovery tools, compare matched v1/v2 benchmark cells and per-harness real telemetry, then perform one clean canonical cutover only after predeclared gates pass.

## Context

An internal-only rewrite would conceal model tool-selection behavior. An immediate replacement would make regressions difficult to attribute. Advertising both surfaces indefinitely would add permanent schema tax and let callers choose inconsistently.

## Decision

1. Measure the current API on every available harness.
2. Freeze paired correctness, first-call/recovery, p95 latency, result-token, dependency coverage/deadline, and minimum per-harness real-call floors before v2 is advertised.
3. Add temporary `tilth_search_v2`; keep current `tilth_search`, `tilth_grok`, and `tilth_deps` during the trial.
4. Run matched benchmark variants that explicitly select v1 or v2. Treat natural both-advertised selection as a separate adoption metric.
5. Block graduation if any paired threshold or sample floor is missing or failing.
6. On pass, replace canonical `tilth_search` with v2 and remove the temporary v2 alias plus MCP grok/deps schemas, prompts, dispatch arms, and obsolete tests.
7. Preserve internal and public Rust grok/dependency APIs.

## Alternatives rejected

- **Atomic unmeasured cutover:** fastest, but loses causal comparison and rollback evidence.
- **Internal shadow mode only:** measures result quality but not public schema/tool-selection behavior.
- **Permanent v1/v2 aliases:** preserves ambiguity and ongoing schema cost.
- **Calendar-only sunset:** removes old tools without evidence that the replacement works.

## Consequences

The temporary registry is larger than either final design and natural telemetry is confounded by model choice, so matched variants are the graduation authority. The final state is clean: one canonical search verb, no compatibility aliases. Numeric thresholds are deliberately set from Wave 1 baseline data rather than guessed in this ADR.
