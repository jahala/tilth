# MCP cost model: why tilth costs more per correct answer on Sonnet 5

From the valid three-way sonnet5 re-run (`benchmark_20260808_030838_sonnet5.jsonl`,
243 rows, 0 errors, tilth attached 81/81 pinned-arm cells). Full data in
`.cheese/notes/tilth-sonnet5-cost-attribution.md`.

## Headline result

On gin/fastapi tasks, `<certain>` tilth arms cost **18-21% more per correct
answer** than no-tilth, with **no accuracy gain**:

| arm | correct | CPC | vs no-tilth |
|---|---|---|---|
| no_tilth | 79/81 (97.5%) | $0.1160 | — |
| upstream (0.9.0) | 78/81 | $0.1405 | +21% (paired 95% CI [+19%,+68%]) |
| fork (PR #196) | 78/81 | $0.1364 | +18% (CI [+15%,+46%]) |

Fork vs upstream cost delta is -$0.32, CI `[-1.40, +0.83]` — statistically
indistinguishable. The task corpus is at a 97.5% no-tilth ceiling, so there
was no accuracy headroom for tilth to fill on this corpus.

## Where the cost actually goes: the MCP prefix, not per-call output

`<certain>` (attribution script `tool_cost_attr.py` over stream logs
`benchmark/results/streams/20260808_030838/`): tilth's **per-call outputs are
cheaper than native**, not more expensive — `tilth_read` mean 2.9KB vs native
`Read` 5.1KB, roughly equal total result volume across arms.

The cost driver is a **fixed ~5,350-token prefix** (tool schemas + server
instructions) that gets cache-written at the start of every cell:

- First-turn prompt size: tilth arms ~13,463-13,465 tokens vs no-tilth
  ~8,113 tokens.
- Each benchmark cell is a fresh session, so this prefix is a fresh
  cache-write (1h rate) every cell, then a cache-read on every subsequent
  turn within the cell.
- Gross prefix cost ≈ $3.60 per arm-run; tilth's more-compact outputs claw
  back ~$2 of that; net cost delta ≈ +$1.80 (upstream) / +$1.48 (fork).
- Category-level deltas confirm this: almost the entire cost difference is
  in `cache_write`/`cache_read`, not `output` tokens.

## Mixed adoption is the worst posture

`<certain>` Tool adoption differs sharply between arms:

- **Upstream**: full commitment — 500 tilth calls / 128 native calls, zero
  `Grep`/`Read`/`Edit`/`Glob` calls, zero tilth→native fallbacks.
- **Fork (PR #196)**: mixed — 331 tilth / 265 native calls, 76/81 cells used
  tilth at all, and **28 tilth→native fallback transitions** (search→Read
  ×10, search→Grep ×6, grok→Grep ×4, read→Grep ×4, search→Glob ×3, diff→Read
  ×1).

Mixed adoption pays the full prefix cost *and* re-fetches content via native
tools when tilth fails or the model loses confidence in it — worse than
either full commitment or not attaching tilth at all.

## Wasted trips

`<certain>` 24 empty/error `tilth_search` results (16 upstream, 8 fork) — see
[Model-tool fumble taxonomy](model-tool-fumble-taxonomy.md) for the query-grammar
root cause. Also: `tilth_read` returned the full 29.1KB `tree.go` (no
smart-view benefit) 3×/arm on `gin_radix_tree`, and 3 redundant identical
`tilth_read` calls were observed.

## Prompt-surface delta investigation (resolved)

`<certain>` Measuring the raw MCP surface (`initialize` instructions +
`tools/list` schemas, `--mcp --edit`):

| build | total chars | instructions chars |
|---|---|---|
| upstream 0.9.0 | 14,981 | 2,187 |
| PR #196 | 13,779 | 1,810 |
| local fork 0.8.4 (stale binary) | 24,917 | 8,688 |

The 24,917-char fork reading was the stale-binary artifact — see
[Benchmark harness gotchas](benchmark-harness-gotchas.md). Real HEAD-vs-196 gap
(current HEAD instructions are 2,003 chars, PR #151's cap held) is in tool
descriptions/schemas: HEAD 18,294 chars total vs #196's 13,779 (write
4,302 vs 2,867; read 3,407 vs 1,939; search 2,981 vs 1,861; diff 2,093 vs
1,485). Part of this gap is fork-inherent (the whole-file-tag ops grammar
needs more schema text than upstream's line:hash anchors); part is
trimmable — see PR #171 below.

Conclusion: there was no "shortening PR #196 missed" — upstream 0.9.0
contains an instructions/schema slimming the 0.8.4 fork never synced.
Benchmark arms are unaffected by this finding (they run upstream/196
binaries, not local HEAD).

## Follow-up: surface-shrink work

PR #171 (`fix/mcp-surface-shrink`) cut HEAD's surface from 18,294 → 13,773
chars — below PR #196's 13,779 — while restoring items an adversarial review
caught missing (MD032 fix, N:-prefix guard, kind grammar, `#n` gloss,
`next_view` semantics, anti-patterns section). See
[Multi-agent workflow notes](multi-agent-workflow-notes.md) for the review process.

## Related

- `.cheese/notes/tilth-sonnet5-cost-attribution.md`
- [Benchmark harness gotchas](benchmark-harness-gotchas.md)
- [Model-tool fumble taxonomy](model-tool-fumble-taxonomy.md)
- PR #168 (harness fix), PR #171 (surface shrink), upstream PR #196
