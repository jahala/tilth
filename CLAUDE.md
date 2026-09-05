# tilth

Rust MCP server + CLI for AST-aware code intelligence. Tree-sitter outlines, symbol search, callers/callees, file-level deps analysis. Replaces grep/cat/find for AI agents with structured, token-efficient output.

## Project structure

A Cargo workspace with two members. The root package is the `tilth` binary (CLI, MCP server, editor, installer, formatting); `crates/tilth-core` is the parsing substrate it links, published as its own crate so other tools can depend on the parser without the faces. Every `crate::lang::…`, `crate::types::…`, `crate::search::symbol::…` path in the binary still resolves through re-exports in `src/lib.rs`, `src/read/mod.rs`, and `src/search/mod.rs`.

```
crates/tilth-core/     The library. Its documented API is what src/lib.rs re-exports at the root; the modules are #[doc(hidden)] internals the binary uses.
  src/
    lib.rs             Crate docs, lint policy, the negotiated public surface (detect_file_type, get_outline_entries, is_test_file, test_entries, imports, callers, deps, TilthError).
    types.rs           Shared types (QueryType, Lang, FileType, OutlineEntry, Match, Edit, etc.), is_test_file(), estimate_tokens().
    error.rs           TilthError with exit codes.
    cache.rs           OutlineCache — DashMap of path → (mtime, outline / parsed tree). Shared across tools.
    lang/
      mod.rs           Shared language infrastructure: detect_file_type(), package_root().
      spec.rs          Per-language LangSpec table: extensions, grammar, queries, stdlib rule, definition ops.
      outline.rs       Tree-sitter outline extraction: outline_language(), walk_top_level(), get_outline_entries(), extract_import_source().
      treesitter.rs    Shared AST constants: DEFINITION_KINDS, extract_definition_name(), definition_weight().
      detection.rs     Generated file detection (lockfiles, .min.js) and binary detection.
      <lang>.rs        One file per language (rust, python, go, …) filling the spec table.
    read/
      mod.rs           Declares imports + outline.
      imports.rs       Import-line detection, external/local classification, related-file resolution.
      outline/
        mod.rs         generate() — dispatches to the right outline backend by file type, appends the truncation note when a cap is hit.
        code.rs        Outline string formatting for code files. Uses lang/outline for extraction.
        markdown.rs    Markdown heading-based outlines.
        structured.rs  JSON/YAML/TOML structured outlines.
        tabular.rs     CSV/TSV outline: headers + row count + first 5 / last 3 rows via memchr.
        fallback.rs    head_tail() / log_view() — unknown files and logs with no outline support.
        test_file.rs   describe/it/test structure: test_entries() as data, outline() rendered over it.
    index/
      bloom.rs         BloomFilterCache — per-file "file contains symbol?" pre-check.
    search/
      mod.rs           The shared walker (SKIP_DIRS, base_walk_builder, walker), file_metadata(), format_token_count().
      symbol.rs        AST-based symbol search (definitions first, then usages).
      callers.rs       Structural call-site detection (tree-sitter + memchr pre-filter); find_callers_batch().
      callees.rs       Callee extraction and resolution for expanded definitions.
      callee_query.rs  Per-language tree-sitter call-expression queries + compiled-Query cache, shared by callers.rs and callees.rs.
      siblings.rs      Sibling symbol surfacing in search results.
      scope.rs         Enclosing-scope lookup: nearest definition at a line, qualified by containing type/module.
      deps.rs          File-level dependency analysis (imports + dependents with symbols); analyze_deps().
      rank.rs          Result ranking (definition weight, basename boost, context proximity).
      facets.rs        Faceted result grouping (definitions, usages, implementations).
      bloom_walk.rs    Shared file-prefilter for relational queries (callers/callees/deps): size gate + bloom-filter pre-check before a full parse.
      blast.rs         Blast radius — find callers of definitions touched by edits.
  tests/api.rs         The acceptance test for the public surface, run from outside the crate.
src/                   The tilth binary.
  main.rs              CLI entry (clap). Dispatches to MCP, map, or single-query mode.
  lib.rs               Public API: classify query → read/search/glob → formatted output. Re-exports tilth-core's cache, error, index, lang, types.
  mcp/
    mod.rs             MCP server (JSON-RPC on stdio). Embeds SERVER_INSTRUCTIONS + EDIT_MODE_EXTRA via include_str! from prompts/.
    write.rs           tilth_write overwrite/append primitives — create-only guard, O_NOFOLLOW symlink refusal, atomic parent-dir creation.
    tools/
      mod.rs           Tool dispatch hub — path/scope resolution under the absolute-path discipline (anchor_path, resolve_scope), budget application.
      definitions.rs   JSON schema definitions for every MCP tool (tilth_search/read/list/deps/grok/diff/savings/write).
      search.rs        tool_search — symbol/content/regex/callers dispatch, multi-symbol support, scope-warning integration.
      read.rs          tool_read — smart file reads, batch/section/sections slicing, savings tracking.
      write.rs         tool_write — batch writes in hash/overwrite/append modes; hash mode delegates to edit::apply_batch.
      deps.rs          tool_deps — file-level dependency analysis (imports + dependents), bloom-filtered.
      diff.rs          tool_diff — structural diff dispatch (uncommitted/staged/ref/file-pair/patch/log).
      list.rs          tool_list — glob file listing, patterns batch, scope resolution, directory-tree rendering.
      grok.rs          tool_grok — one-call symbol bundle, default vs full caps.
      savings.rs       tool_savings — session token-savings summary vs a naive-read baseline.
      session.rs       tool_session — summary/reset actions for grok dedup + savings state.
  classify.rs          Query type detection (file path, glob, symbol, content, fallthrough).
  diff/
    mod.rs             Structural diff types, source resolution, orchestrator pipeline (diff()).
    parse.rs           Unified diff parser: git diff output → Vec<FileDiff>.
    matching.rs        Three-phase symbol matching: identity → structural hash → fuzzy similarity.
    overlay.rs         Per-file structural overlay: outline old/new, match symbols, attribute hunks.
    format.rs          Progressive-disclosure formatters: overview, file detail, function detail, log, conflicts.
  read/
    mod.rs             File reading with smart view (full vs outline based on token count). Re-exports tilth-core's read::{imports, outline}.
  search/
    mod.rs             Search orchestration and formatting. Re-exports the walker and the structural queries from tilth-core.
    content.rs         Literal text / regex search via ripgrep internals.
    grok.rs            One-call symbol bundle (def + body + callers + callees + siblings + tests).
    strip.rs           Cognitive load stripping (comments, blank lines in expanded code).
    truncate.rs        Smart truncation to fit budget constraints.
    alloc.rs           Value-based budget allocation — keeps the highest-value blocks when output exceeds budget, not just positional tail-cut.
    glob.rs            File glob search.
  session.rs           MCP session state — tracks previously expanded definitions for dedup.
  edit.rs              Hash-anchored editing (tilth_write hash mode). Hashline verification + atomic apply. Re-exports the Edit type from tilth-core.
  edit_parse_check.rs  Post-edit tree-sitter parse check — diffs pre/post ERROR/MISSING nodes so tilth_write reports only errors the edit introduced.
  install.rs           `tilth install <host>` — writes MCP config for the supported hosts.
  format.rs            Output formatting helpers.
  budget.rs            Token budget enforcement.
  map.rs               Codebase map generation (CLI only, disabled as MCP tool).
  overview.rs          Project fingerprint for MCP initialization (manifest, languages, modules, deps, git). Instant orientation without a tool call.
  timeout.rs           Per-request wall-clock timeout for sync tool calls — worker thread + bounded channel, tracks abandoned threads on expiry.
  util.rs              atomic_write_bytes() — shared by edit.rs and install.rs.
npm/                   npm wrapper — postinstall downloads binary, run.js proxies to it.
benchmark/             Evaluation harness (see Benchmarks section below).
prompts/               MCP server instruction source (mcp-base.md + mcp-edit.md). Embedded into the binary at compile time and regenerated into AGENTS.md.
AGENTS.md              User-facing copy of the MCP instructions. Generated from prompts/*.md via scripts/regen-agents-md.sh — do not edit directly.
docs/tend2/            The module map (tend2 loops): what this repo claims and how each claim is proven.
scripts/check/         Evidence scripts the loops cite (workspace green, test count, output parity, context cost, …).
```

## Languages supported

Rust, TypeScript, TSX, JavaScript, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift, Kotlin, Elixir, Bash.
Dockerfile, Make detected but have no tree-sitter grammar (outline returns None).

## Build, test, install

```bash
cargo build --release        # release build
cargo test                   # unit tests (in-source #[cfg(test)] modules)
cargo clippy --all-targets -- -D warnings  # lint (incl. tests)
cargo fmt --check            # format check
cargo install --path .       # install to ~/.cargo/bin/tilth
```

CI runs `fmt --check`, `clippy --all-targets -D warnings`, `cargo test` on every push/PR.

## Version bumps

Update version in **both** `Cargo.toml` and `npm/package.json`. Tag with `v<version>` on main.

Releases publish **two npm names** from the same `npm/` wrapper: the canonical unscoped `tilth` and the org anchor `@plotplot/tilth` (the `publish-npm` job renames the artifact and republishes with `--access public`). Both names have an OIDC trusted publisher on npmjs.com (`jahala/tilth` + `release.yml`), so releases need no token. `@plotplot/tilth` was bootstrapped with a one-time manual publish — npm cannot configure trusted publishing for a package that does not exist yet.

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

What the MCP server says at `initialize` is a pointer under 120 tokens, in `prompts/`:

- `prompts/mcp-base.md` — served in every mode (`SERVER_INSTRUCTIONS`): the native-tool prohibition, the root rule, and where the guide lives
- `prompts/mcp-edit.md` — appended in edit mode (`EDIT_MODE_EXTRA`): `tilth_write` replaces the host editor

The guide itself is `skills/SKILL.md`: a one-sentence description and a body that carries every instruction the served block used to carry, plus the CLI usage. The project fingerprint is no longer prepended at `initialize` (`tilth overview` still prints it); `--no-overview` is accepted and inert.

`src/mcp/mod.rs` embeds both prompt files at compile time via `include_str!`. `AGENTS.md` is the user-facing copy; regenerate it via `./scripts/regen-agents-md.sh` after any change so both surfaces stay in lockstep. The byte-lock tests in `src/mcp/mod.rs` (`server_instructions_byte_lock`, `edit_mode_extra_byte_lock`, `served_instructions_are_under_the_token_bar_in_both_modes`) flag drift and must be updated alongside intentional prompt edits. `scripts/check/context-cost.sh` measures what a live server serves; `scripts/check/skill-body.sh` checks the skill still carries every line the v0.10.1 block did.

Changes to the pointer and the skill must be surgical — no bloat. Haiku is sensitive to:

- Instruction positioning (top-weighted — put important guidance first)
- Framing ("DO NOT" works better than "IMPORTANT:" for weaker models)
- Concrete examples (tool call patterns, not abstract descriptions)

Instruction changes are measured by a copeca A/B (the skill-versus-MCP brief is `docs/ab/skill-vs-mcp-2026-09.md`), not by the in-repo benchmark.
