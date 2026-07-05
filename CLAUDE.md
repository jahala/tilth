# tilth

Rust MCP server + CLI for AST-aware code intelligence. Tree-sitter outlines, symbol search, callers/callees, file-level deps analysis. Replaces grep/cat/find for AI agents with structured, token-efficient output.

## Project structure

```
src/
  main.rs              CLI entry (clap). Dispatches to MCP, map, or single-query mode.
  lib.rs               Public API: classify query → read/search/glob → formatted output.
  mcp/mod.rs           MCP server (JSON-RPC on stdio). Embeds SERVER_INSTRUCTIONS + EDIT_MODE_EXTRA via include_str! from prompts/.
  classify.rs          Query type detection (file path, glob, symbol, content, fallthrough).
  lang/
    mod.rs             Shared language infrastructure: detect_file_type(), package_root().
    outline.rs         Tree-sitter outline extraction: outline_language(), walk_top_level(), get_outline_entries().
    treesitter.rs      Shared AST constants: DEFINITION_KINDS, extract_definition_name(), definition_weight().
    detection.rs       Generated file detection (lockfiles, .min.js) and binary detection.
  diff/
    mod.rs             Structural diff types, source resolution, orchestrator pipeline (diff()).
    parse.rs           Unified diff parser: git diff output → Vec<FileDiff>.
    matching.rs        Three-phase symbol matching: identity → structural hash → fuzzy similarity.
    overlay.rs         Per-file structural overlay: outline old/new, match symbols, attribute hunks.
    format.rs          Progressive-disclosure formatters: overview, file detail, function detail, log, conflicts.
  read/
    mod.rs             File reading with smart view (full vs outline based on token count).
    outline/
      code.rs          Outline string formatting for code files. Uses lang/outline for extraction.
      markdown.rs      Markdown heading-based outlines.
      structured.rs    JSON/YAML/TOML structured outlines.
      test_file.rs     Test file detection (suppresses outline noise).
    imports.rs         Import extraction for deps analysis.
  search/
    mod.rs             Search orchestration. Symbol, content, regex, callers search types.
    symbol.rs          AST-based symbol search (definitions first, then usages).
    content.rs         Literal text / regex search via ripgrep internals.
    callers.rs         Structural call-site detection (tree-sitter + memchr pre-filter).
    callees.rs         Callee extraction and resolution for expanded definitions.
    siblings.rs        Sibling symbol surfacing in search results.
    deps.rs            File-level dependency analysis (imports + dependents with symbols).
    rank.rs            Result ranking (definition weight, basename boost, context proximity).
    facets.rs          Faceted result grouping (definitions, usages, implementations).
    strip.rs           Cognitive load stripping (comments, blank lines in expanded code).
    truncate.rs        Smart truncation to fit budget constraints.
    glob.rs            File glob search.
  index/
    bloom.rs           Bloom filter cache for fast "file contains symbol?" pre-check.
  cache.rs             OutlineCache — DashMap of path → (mtime, outline). Shared across tools.
  session.rs           MCP session state — tracks previously expanded definitions for dedup.
  edit/
    mod.rs             Path-key normalization; re-exports the whole-file-tag edit modules below.
    tag.rs             Whole-file content tag (`[path#TAG]`) mint/verify + numbered-line rendering.
    parser.rs          Op-grammar parser: `[path#TAG]` sections into ops.
    block.rs           Resolves a `#symbol`/line block anchor to a concrete line span (wires to `lang/outline`).
    apply.rs           Applies parsed ops to file content (line-op splicing).
    recovery.rs        3-way-merge recovery when the live file drifted since the tagged read.
    snapshots.rs       Per-session file snapshots keyed by tag for edit verification.
    mismatch.rs        Tag-mismatch classification (drift vs fabricated).
  install.rs           `tilth install <host>` — writes MCP config for 6 hosts.
  format.rs            Output formatting helpers.
  budget.rs            Token budget enforcement.
  map.rs               Codebase map generation (CLI only, disabled as MCP tool).
  types.rs             Shared types (QueryType, Lang, OutlineEntry, etc.).
  error.rs             Error types with exit codes.
npm/                   npm wrapper — postinstall downloads binary, run.js proxies to it.
benchmark/             Evaluation harness (see Benchmarks section below).
prompts/               MCP server instruction source (mcp-base.md + mcp-edit.md). Embedded into the binary at compile time and regenerated into AGENTS.md.
AGENTS.md              User-facing copy of the MCP instructions. Generated from prompts/*.md via scripts/regen-agents-md.sh — do not edit directly.
```

## Languages supported

Rust, TypeScript, TSX, JavaScript, Python, Go, Java, C, C++, Ruby, PHP, C#, Swift, Kotlin, Scala, Elixir, Bash.
Dockerfile, Make detected but have no tree-sitter grammar (outline returns None).

## Build, test, install

```bash
cargo build --release        # release build
cargo test                   # unit tests (in-source #[cfg(test)] modules)
cargo clippy -- -D warnings  # lint
cargo fmt --check            # format check
cargo install --path .       # install to ~/.cargo/bin/tilth
```

CI runs `fmt --check`, `clippy -D warnings`, `cargo test` on every push/PR.

## Fork law

This is a **fork**. Some divergence from upstream is permanent and intentional; do not "fix" it toward upstream on a sync.

**Version stays 0.8.4.** Do **not** bump the version on this fork. Leave `version` in `Cargo.toml` and `npm/package.json` untouched; do not tag releases. Version bumps are owned upstream and synced in via the upstream-merge commits — a fork-side bump diverges from upstream and conflicts on the next sync.

**Keep-ours fork features** (take the fork side on any sync conflict):

- The whole-file-tag edit model — JSON `edits` array of `{path, tag?, ops}` sections lowered onto the tag/seen-lines-gate/3-way-merge-recovery machinery (per #116) — the fork's `tilth_write` surface, not upstream's.
- cwd anchoring and the trust-absolute posture: every path-taking MCP tool takes a required `cwd` (renamed from upstream's optional `root`); relative paths anchor under `cwd` with `..` refused, absolute paths are trusted as-is. The MCP roots one-shot handshake is removed. The `root`→`cwd` rename and the trust-absolute posture are permanent fork patches — expect them to conflict on every upstream sync and always resolve to the fork side.

**Never-merge upstream commits:** `399721c9` and `10bec56a` must never land on this fork. Skip them when syncing.

**Sync mechanics:** pull upstream onto a `sync/upstream-<date>` branch (never straight onto the working branch), resolve the known conflicts to the fork side per the rules above, and verify the gates before merging.

Releases publish **two npm names** from the same `npm/` wrapper: the canonical unscoped `tilth` and the org anchor `@plotplot/tilth` (the `publish-npm` job renames the artifact and republishes with `--access public`). Both publishes authenticate with `NPM_TOKEN` (`NODE_AUTH_TOKEN`); the `@plotplot/tilth` step is `continue-on-error` (best-effort) so a scope-setup failure never fails the release.

## Benchmarks

51 tasks across a synthetic repo and 4 real repos (Express/JS, FastAPI/Python, Gin/Go, ripgrep/Rust), spanning code navigation, multi-file edits, diff comprehension, and symbol "grok" understanding. Navigation/grok tasks run headless `claude -p` and check the answer against ground-truth strings; edit/diff tasks inject mutations and pass only when the task's `test_command` goes green.

**Setup** (one-time — clones repos at pinned commits):

```bash
python benchmark/fixtures/setup.py
```

**Run** (from project root — works inside Conductor/Claude Code sessions, `run.py` strips `CLAUDECODE` env var):

```bash
# Full suite: all tasks, baseline + tilth, 3 reps per task
python benchmark/run.py --models sonnet --reps 3 --tasks all --modes all

# Specific tasks
python benchmark/run.py --models haiku --reps 3 --tasks rg_search_dispatch,rg_trait_implementors --modes tilth

# Models: sonnet, opus, haiku, gpt5, o3
# Modes: baseline (built-in tools), tilth (built-in + tilth MCP), tilth_forced (tilth MCP only)
# Tasks: all, or comma-separated names from benchmark/tasks/*.py
```

Hard tasks take 2-5 min each. Run in background for multi-task suites. Do NOT pipe output through `head` or similar — it breaks the pipe and causes timeouts.

**Analyze**:

```bash
python benchmark/analyze.py benchmark/results/benchmark_<timestamp>_<model>.jsonl
python benchmark/paired.py benchmark/results/benchmark_<timestamp>_<model>.jsonl

# Quick check of a results file:
jq -r '[.task, (.correct|tostring), (.total_cost_usd|tostring), (.tool_calls.tilth_search // 0 | tostring)] | join("\t")' benchmark/results/<file>.jsonl
```

Results written to `benchmark/results/benchmark_<timestamp>_<model>.jsonl`. Each line is JSON with: `task`, `mode`, `model`, `correct`, `total_cost_usd`, `num_turns`, `tool_calls` (map of tool name → count), `tool_sequence`, `tilth_version`, `duration_ms`, token counts.

Key metric: **cost per correct answer** = total_spend / correct_count. This is the expected cost under retry (geometric model: `avg_cost / accuracy`).

Task definitions are in `benchmark/tasks/*.py`. Each has `name`, `prompt`, `ground_truth` (required strings), `repo`, and difficulty tier. Hard tasks for testing instruction changes: `rg_search_dispatch`, `rg_trait_implementors`, `gin_servehttp_flow`.

## MCP instructions

Server instructions sent via MCP protocol live in `prompts/`:

- `prompts/mcp-base.md` — base instructions for all modes (wired in as `SERVER_INSTRUCTIONS`)
- `prompts/mcp-edit.md` — appended in edit mode (wired in as `EDIT_MODE_EXTRA`)

`src/mcp/mod.rs` embeds both at compile time via `include_str!`. `AGENTS.md` is the user-facing copy; regenerate it via `./scripts/regen-agents-md.sh` after any change so both surfaces stay in lockstep. The byte-lock tests in `src/mcp/mod.rs` (`server_instructions_byte_lock`, `edit_mode_extra_byte_lock`) flag accidental drift and must be updated alongside intentional prompt edits.

Changes to MCP instructions must be surgical — no bloat. Haiku is sensitive to:

- Instruction positioning (top-weighted — put important guidance first)
- Framing ("DO NOT" works better than "IMPORTANT:" for weaker models)
- Concrete examples (tool call patterns, not abstract descriptions)

Test instruction changes with haiku benchmarks on hard tasks (`rg_search_dispatch`, `rg_trait_implementors`, `gin_servehttp_flow`).
