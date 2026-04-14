# tilth

Rust MCP server + CLI for AST-aware code intelligence. Tree-sitter outlines, symbol search, callers/callees, file-level deps analysis. Replaces grep/cat/find for AI agents with structured, token-efficient output.

## Project structure

```
src/
  main.rs              CLI entry (clap). Dispatches to MCP, map, or single-query mode.
  lib.rs               Public API: classify query → read/search/glob → formatted output.
  mcp.rs               MCP server (JSON-RPC on stdio). SERVER_INSTRUCTIONS + EDIT_MODE_EXTRA.
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
    blast.rs           Blast radius — find callers of definitions touched by edits.
  index/
    symbol.rs          In-memory symbol index (built on first search, cached).
    bloom.rs           Bloom filter cache for fast "file contains symbol?" pre-check.
  cache.rs             OutlineCache — DashMap of path → (mtime, outline). Shared across tools.
  session.rs           MCP session state — tracks previously expanded definitions for dedup.
  edit.rs              Hash-anchored editing (tilth_edit). Hashline verification + atomic apply.
  install.rs           `tilth install <host>` — writes MCP config for 6 hosts.
  format.rs            Output formatting helpers.
  budget.rs            Token budget enforcement.
  map.rs               Codebase map generation (CLI only, disabled as MCP tool).
  types.rs             Shared types (QueryType, Lang, OutlineEntry, etc.).
  error.rs             Error types with exit codes.
npm/                   npm wrapper — postinstall downloads binary, run.js proxies to it.
benchmark/             Evaluation harness (see Benchmarks section below).
AGENTS.md              MCP tool usage instructions shipped to users (read by Claude Code).
```

## Languages supported

Rust, TypeScript, TSX, JavaScript, Python, Go, Java, C, C++, Ruby, PHP, C#, Swift.
Kotlin, Dockerfile, Make detected but have no tree-sitter grammar (outline returns None).

## Build, test, install

```bash
cargo build --release        # release build
cargo test                   # unit tests (in-source #[cfg(test)] modules)
cargo clippy -- -D warnings  # lint
cargo fmt --check            # format check
cargo install --path .       # install to ~/.cargo/bin/tilth
```

CI runs `fmt --check`, `clippy -D warnings`, `cargo test` on every push/PR.

## Version bumps

Update version in **both** `Cargo.toml` and `npm/package.json`. Tag with `v<version>` on main.

## Benchmarks

26 code navigation tasks across 4 repos (Express/JS, FastAPI/Python, Gin/Go, ripgrep/Rust). Each task runs headless `claude -p` with a question, checks answer against ground-truth strings.

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
python benchmark/compare_versions.py old.jsonl new.jsonl

# Quick check of a results file:
jq -r '[.task, (.correct|tostring), (.total_cost_usd|tostring), (.tool_calls.tilth_search // 0 | tostring)] | join("\t")' benchmark/results/<file>.jsonl
```

Results written to `benchmark/results/benchmark_<timestamp>_<model>.jsonl`. Each line is JSON with: `task`, `mode`, `model`, `correct`, `total_cost_usd`, `num_turns`, `tool_calls` (map of tool name → count), `tool_sequence`, `tilth_version`, `duration_ms`, token counts.

Key metric: **cost per correct answer** = total_spend / correct_count. This is the expected cost under retry (geometric model: `avg_cost / accuracy`).

Task definitions are in `benchmark/tasks/*.py`. Each has `name`, `prompt`, `ground_truth` (required strings), `repo`, and difficulty tier. Hard tasks for testing instruction changes: `rg_search_dispatch`, `rg_trait_implementors`, `gin_servehttp_flow`.

## MCP instructions

Server instructions sent via MCP protocol live in `src/mcp.rs`:
- `SERVER_INSTRUCTIONS` — base instructions for all modes
- `EDIT_MODE_EXTRA` — appended in edit mode (hashline format, edit workflow)

`AGENTS.md` is the user-facing copy read directly by Claude Code (not via MCP protocol). Both should stay in sync.

Changes to MCP instructions must be surgical — no bloat. Haiku is sensitive to:
- Instruction positioning (top-weighted — put important guidance first)
- Framing ("DO NOT" works better than "IMPORTANT:" for weaker models)
- Concrete examples (tool call patterns, not abstract descriptions)

Test instruction changes with haiku benchmarks on hard tasks (`rg_search_dispatch`, `rg_trait_implementors`, `gin_servehttp_flow`).
