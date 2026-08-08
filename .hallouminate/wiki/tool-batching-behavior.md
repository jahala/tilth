# Tool-call batching: Sonnet 5 essentially never batches unprompted

Findings from the sonnet5 investigation on whether models batch multiple
tool calls per turn, and what shipped in response. Source:
`.cheese/notes/tilth-pr196-sonnet5-audit.md`.

## The finding

`<certain>` Across all three benchmark arms, Sonnet 5 batches **1-2%** of the
time it plausibly could, and emits **zero parallel tool-use turns**:

- `tilth_read` multi-item calls: 1-2% (3/123 fork, 3/202 upstream)
- `tilth_write` multi-file calls: 7-14%
- `tilth_search` always singular — upstream's schema has no query array at
  all; the fork's `queries: [...]` is batch-capable but never actually
  batched
- Native `Read`: 0%
- **0 of 1,781 tool-bearing turns** across all three arms emitted more than
  one tool call

`<certain>` Likely cause: the MCP tool descriptions' examples all show
single-item calls, which teaches singleton usage even where the schema
supports arrays. `<certain>` The benchmark corpus itself doesn't currently
exercise scenarios that reward batching, so this measures baseline model
behavior more than task pressure.

## Countermeasures shipped (pending validation)

Three PRs, built by codex workers in worktrees under
`/home/paul/Dev/tilth-wt/`, each adversarially reviewed by opus-tier agents
before merge (see [Multi-agent workflow notes](multi-agent-workflow-notes.md)):

- **PR #171** (`fix/mcp-surface-shrink`, base `main`): rewrote tool-call
  examples to be batch-first 2-item examples throughout, alongside the
  surface-shrink work (18,294 → 13,773 chars). Also added a control-char
  escape rule with a byte-lock test (see fumble class 3 in
  [Model-tool fumble taxonomy](model-tool-fumble-taxonomy.md)).
- **PR #170** (`feat/jit-batch-nudge`, base `main`): a session-state
  just-in-time nudge that prompts the model toward batching mid-session.
- **PR #169** (`feat/benchmark-batch-metric`, base
  `benchmark/sonnet5-failure-taxonomy`, stacked on #168): adds a
  `batch_sizes` field per result row and a per-arm Batching table to
  `analyze.py`'s reports, so batch rate is now a measured metric, not a
  one-off script output.

## Validation status

`<speculative>` Not yet validated — the plan is a HEAD-vs-upstream A/B run on
haiku (run #12 in the working notes) after all three PRs merge, which will
measure both #171's example-compression effect and #170's nudge effect,
using #169's new batch-rate metric. This has not run as of 2026-08-08.

## Related

- `.cheese/notes/tilth-pr196-sonnet5-audit.md`
- [MCP cost model: why tilth costs more per correct answer](mcp-cost-model-sonnet5.md)
- [Model-tool fumble taxonomy](model-tool-fumble-taxonomy.md)
- [Multi-agent workflow notes](multi-agent-workflow-notes.md)
