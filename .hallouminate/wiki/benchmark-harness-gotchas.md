# Benchmark harness gotchas (Sonnet 5 investigation, 2026-08-08)

Four independent ways the benchmark harness silently produced invalid or
misleading numbers, found while investigating PR #196
(`jahala/tilth#196`). Full forensics in `.cheese/notes/tilth-pr196-sonnet5-audit.md`
and `.cheese/notes/tilth-sonnet5-cost-attribution.md`.

## `--safe-mode` strips ALL MCP servers, including `--mcp-config` ones

The 2026-08-07 sonnet5 run (`benchmark_20260807_075044_sonnet5.jsonl`) compared
three arms that were supposed to differ by which tilth MCP server was attached.
`--safe-mode` disabled every MCP server — including ones passed via
`--mcp-config` — so all three arms ran native-tools-only. Init messages showed
`tools=[5 native], mcp_servers=[]` in every cell; first-turn prompt size was
identical (~8,124 tokens) across arms. Every accuracy/cost delta in that run
was sampling noise (fork-upstream +$0.21 total, bootstrap 95% CI
`[-0.66, +1.13]`). The PR #196 failure-audit and benchmark comments posted
against this run (comments `5223809583`, `5216075559`) rest on the invalid
comparison.

Fix: `--setting-sources ""` for harness isolation instead of `--safe-mode`.

## Guards added on PR #168 (commit `d4d41b5`)

- Per-cell `available_tools`, `mcp_servers`, and `model_usage` recorded in
  every result row.
- `McpUnavailableError` — the harness now aborts fast if a pinned arm's MCP
  server fails to attach, instead of silently degrading to native tools.
- `check_experiment` validates tilth-availability per pinned cell (fails the
  run if a tilth arm didn't actually get tilth attached).
- Per-arm and per-task tool-usage reporting in `analyze.py`.

Verified by a 6-cell haiku smoke run
(`benchmark_20260808_021626_haiku.jsonl`): tilth attached and was called in
both pinned arms.

## `pricing.yaml` drift vs Claude Code's native billing

Sonnet 5 was initially priced in `benchmark/pricing.yaml` using the wrong
rate family, understating computed cost by **-33.7%** vs Claude Code's native
billing. Corrected to the introductory Sonnet-5 rates ($2/M input, $10/M
output, $2.50/M 5m cache writes, $4/M 1h cache writes, $0.20/M cache reads,
`as_of: 2026-08-07`) — residual dropped to -0.50% (remainder attributed to a
haiku sidecar call, decomposable via the new per-row `model_usage` field).
Reports now show a `Δnative` residual line so a future pricing drift is
visible instead of silent.

## Stale installed binary served pre-#151 instructions for weeks

`~/.local/bin/tilth` was a stale build that predated PR #151 (the 2KB
instruction-cap shortening) — it served an 8,688-char instructions blob for
weeks after #151 landed. It was mistaken for current HEAD behavior during an
early measurement pass (the "24,917-char fork surface" reading). Root cause:
version is pinned at `0.8.4` across the whole fork (see CLAUDE.md fork law),
so `tilth --version` cannot distinguish a stale binary from current HEAD.

**Check byte sizes (or rebuild from source), not `--version`, when a fork
binary's prompt surface is in question.**

## Related

- `.cheese/notes/tilth-pr196-sonnet5-audit.md`
- `.cheese/notes/tilth-sonnet5-cost-attribution.md`
- PR #168 (paulnsorensen/tilth): harness + reporting fixes, commits `d4d41b5`, `f8a92c9`
