# tilth wiki — index

This wiki is what an LLM working in the `tilth` repo writes to and reads from
when it wants to remember things across sessions. It lives at
`.hallouminate/wiki/` and is indexed as the `repo:tilth:wiki` corpus, separate
from the source-code corpus (`repo:tilth:corpus`) and the per-session reports
under `.cheese/`.

## Topics

- [Benchmark harness gotchas (Sonnet 5 investigation)](benchmark-harness-gotchas.md) — `--safe-mode` silently strips all MCP servers including `--mcp-config` ones (invalidated a full sonnet5 run); the PR #168 guards (available_tools/mcp_servers/model_usage recording, McpUnavailableError abort); pricing.yaml drift vs native billing; the stale `~/.local/bin/tilth` binary that made byte-size, not `--version`, the only trustworthy staleness check.

- [Diff: git ref resolution and exit-code handling](diff-git-ref-resolution.md) — why the root commit needs git's empty-tree hash rather than `{hash}^..{hash}`, why `^!` looks right and is not (it degrades to a working-tree diff and breaks `overlay.rs`'s `..`-splitting), git diff's 0-or-1 success convention, and the three constraints on default-branch teaching hints.
- [Diff: symbol output order is not deterministic](diff-symbol-order-nondeterminism.md) — open bug: `match_symbols` iterates a `HashMap`, so formatted symbol line order varies between `diff()` calls; how to write tests around it and what the workaround costs.


- [Edit-anchor design: per-line hash vs whole-file tag](edit-anchor-design.md) — why tilth originally anchored edits with a per-line content hash, the FNV low-bit-mask bug, the measured ~25% per-read token tax vs oh-my-pi's O(1) whole-file tag, and the analysis behind the since-shipped switch to the whole-file-tag model.
- [Local gate gotchas (macOS)](local-gate-gotchas.md) — why a local `cargo test` shows one failure CI does not (`batch_budget_represents_every_query`), and why CI's `cargo clippy -- -D warnings` is clean while `--all-targets` is not. Both are baseline; check before "fixing" either.
- [MCP cost model: why tilth costs more per correct answer on Sonnet 5](mcp-cost-model-sonnet5.md) — valid three-way benchmark result (+18-21% cost per correct, no accuracy gain at a 97.5% ceiling); the cost is a ~5,350-token fixed MCP prefix cache-written per cell, not per-call output volume; why mixed tool adoption (28 tilth→native fallbacks) is the worst posture.
- [MCP cwd / workspace-root binding](mcp-cwd-root-binding.md) — why tilth uses a required per-call `cwd` param (renamed from `root` in PR #113, hook removed in #144; not the MCP `roots` capability) to resolve paths to the right git-worktree checkout; the silent worktree gotcha; 8-harness client survey.
- [MCP instructions & tool descriptions — host limits and format recipe](mcp-instructions-limits-and-format.md) — Claude Code's 2KB-per-field truncation (the root cause behind the July-2026 claude-side adoption gaps), spec semantics of `instructions`/`description`, the examples-beat-prose format recipe, and the schema-property channel for parameter docs.
- [Model-tool fumble taxonomy (Sonnet 5)](model-tool-fumble-taxonomy.md) — ranked catalogue of Sonnet 5 tool-call failure classes: missing cwd on relative paths (30 errors, biggest class), grep-style queries breaking tilth_search's grammar (29 empty results), literal tab bytes killing tilth_write JSON, upstream line:hash anchor mangling (fork immune), and the now-fixed tilth_grok Go type_declaration resolution gap (commit 9b17e8e).
- [Multi-agent workflow notes (fan-out builds)](multi-agent-workflow-notes.md) — worktree workers can't commit (gitdir points outside sandbox); adversarial opus review caught real defects on every fanned-out PR (#169/#170/#171); byte-lock tests on prompt files; `rebase --onto` needed after a stacked PR's base gets squash-merged.


- [Search and dependency discovery domain model](domain-model.md) — ubiquitous-language entities for the search-v2 roadmap (search v2 request/envelope, client profile, worktree key, dependency index, verified-only partial, graduation manifest, search continuation); all marked NEW ENTITY — none landed in code yet. Decisions live in `adr/tilth-search-v2-roadmap-001..005`.
- [tilth MCP usage analytics — July 2026](usage-analytics-2026-07.md) — one month of real-session error/usage data across claude/codex/omp, the tilth_write error taxonomy (74% op-shape guessing), the cross-harness tilth_diff directory-as-file signal, and the oh-my-pi built-in comparison showing ~5–6% is the write-tool floor; evidence base + ranked fixes for the per-tool spec sessions.
- [Tool-call batching: Sonnet 5 essentially never batches unprompted](tool-batching-behavior.md) — 1-2% batch rate, zero parallel tool-use turns across 1,781 turns; singleton-call examples as the likely cause; countermeasures shipped in PR #171 (batch-first examples), #170 (JIT nudge), #169 (batch-rate metric) pending A/B validation.
- [`tilth_read` budget accounting and vacuous budget guards](read-budget-accounting.md) — `finalize_response` is the only budget gate and `record_savings` has two easily-conflated call sites; why a `<= budget` assertion is vacuous on Linux CI (50-token flat header reserve) while failing on macOS, why the obvious token-count differential is also wrong (`estimate_tokens` is subadditive), and the exact-equality assertion that works. Also the known macOS-only `batch_budget_represents_every_query` failure.

## How to use this index

`index.md` is a table of contents, not a topic. Add new pages to the list
above (alphabetical), keeping a one-line gloss per entry. Anything substantive
belongs in a topic file — one topic per file.

If you read this index and don't see the topic you need, run `list_files`
against the `repo:tilth:wiki` corpus first — the index may be out of date
relative to the directory.
