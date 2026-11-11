# tilth

Rust MCP server + CLI for AST-aware code intelligence. Tree-sitter outlines, symbol search, callers/callees, file-level deps analysis. Replaces grep/cat/find for AI agents with structured, token-efficient output.

## Project structure

A Cargo workspace of two crates. The root package is the `tilth` binary: CLI, MCP server, and the rendering of every tool's output. `crates/tilth-core` is the parsing substrate as a library: it returns data, and the binary renders it. Other crates depend on `tilth-core` too, so its root re-exports are a public API (`crates/tilth-core/tests/api.rs` pins them).

```
crates/tilth-core/src/
  lib.rs               Public API: the re-exports at the crate root. Every module is #[doc(hidden)] and shared with the binary.
  lang/
    mod.rs             Shared language infrastructure: detect_file_type(), package_root().
    spec.rs            Per-language spec table (grammar, definition kinds, callee/sibling queries, stdlib rule, lexer flags).
    outline.rs         Tree-sitter outline extraction: outline_language(), walk_top_level(), get_outline_entries().
    treesitter.rs      Shared AST constants: DEFINITION_KINDS, extract_definition_name(), definition_weight().
    detection.rs       Generated file detection (lockfiles, .min.js) and binary detection.
    <lang>.rs          One spec per language. c.rs, cpp.rs and go.rs also hold test fixtures, shared with the binary's tests through the `test-fixtures` feature.
  search/
    walk.rs            The shared directory walker: SKIP_DIRS, .tilthignore, the gitignore switch, glob overrides.
    callers.rs         Structural call-site detection (tree-sitter + memchr pre-filter): find_callers_batch(), CallerMatch.
    callees.rs         Callee extraction and resolution for expanded definitions.
    callee_query.rs    Per-language tree-sitter call-expression queries + compiled-Query cache, shared by callers.rs and callees.rs.
    scope.rs           Enclosing-scope lookup: nearest definition at a line, qualified by containing type/module.
    deps.rs            File-level dependency analysis: analyze_deps() → DepsResult.
    bloom_walk.rs      Shared file-prefilter for relational queries (callers/callees/deps): size gate + bloom-filter pre-check before a full parse.
  index/
    bloom.rs           Bloom filter cache for fast "file contains symbol?" pre-check.
  imports.rs           Imports as data, one Import per module a statement names: extract_imports(). Also is_import_line(), is_external(), resolve_related_files_with_content().
  test_structure.rs    describe/it/test structure of a test file as data: test_entries().
  cache.rs             OutlineCache — DashMap of path → (mtime, outline) and parsed trees. Shared across tools.
  types.rs             Shared types: Lang, FileType, OutlineEntry, OutlineKind.
  error.rs             TilthError, with exit codes.
src/
  main.rs              CLI entry (clap). Dispatches to MCP, map, or single-query mode.
  lib.rs               Public API: classify query → read/search/glob → formatted output.
  mcp/
    mod.rs             MCP server (JSON-RPC on stdio). Embeds SERVER_INSTRUCTIONS + EDIT_MODE_EXTRA via include_str! from prompts/.
    write.rs           tilth_write overwrite/append primitives — create-only guard, O_NOFOLLOW symlink refusal, atomic parent-dir creation.
    tools/
      mod.rs           Tool dispatch hub — path/scope resolution under the absolute-path discipline (anchor_path, resolve_scope), budget application.
      definitions.rs   JSON schema definitions for every MCP tool (tilth_search/read/list/deps/grok/diff/write).
      search.rs        tool_search — symbol/content/regex/callers dispatch, multi-symbol support, scope-warning integration.
      read.rs          tool_read — smart file reads, batch/section/sections slicing.
      write.rs         tool_write — batch writes in hash/overwrite/append modes; hash mode delegates to edit::apply_batch.
      deps.rs          tool_deps — file-level dependency analysis (imports + dependents), bloom-filtered.
      diff.rs          tool_diff — structural diff dispatch (uncommitted/staged/ref/file-pair/patch/log).
      list.rs          tool_list — glob file listing as a directory tree, pattern/patterns batch, scope resolution.
      grok.rs          tool_grok — one-call symbol bundle, default vs full caps.
      session.rs       tool_session — summary/reset actions for grok dedup state.
  classify.rs          Query type detection (file path, glob, symbol, content, fallthrough).
  diff/
    mod.rs             Structural diff types, source resolution, orchestrator pipeline (diff()).
    parse.rs           Unified diff parser: git diff output → Vec<FileDiff>.
    matching.rs        Three-phase symbol matching: identity → structural hash → fuzzy similarity.
    overlay.rs         Per-file structural overlay: outline old/new, match symbols, attribute hunks.
    format.rs          Progressive-disclosure formatters: overview, file detail, function detail, log, conflicts.
  read/
    mod.rs             File reading with smart view (full vs outline based on token count).
    outline/
      mod.rs           generate() — dispatches to the right outline backend by file type, appends the truncation note when a cap is hit.
      code.rs          Outline string formatting for code files. Uses lang/outline for extraction.
      markdown.rs      Markdown heading-based outlines.
      structured.rs    JSON/YAML/TOML structured outlines.
      tabular.rs       CSV/TSV outline: headers + row count + first 5 / last 3 rows via memchr.
      fallback.rs      head_tail() / log_view() — unknown files and logs with no outline support.
      test_file.rs     Test-suite outline, rendered over tilth_core::test_entries().
  search/
    mod.rs             Search orchestration. Symbol, content, regex, callers search types.
    symbol.rs          AST-based symbol search (definitions first, then usages).
    content.rs         Literal text / regex search via ripgrep internals.
    callers.rs         Caller search output: ranks tilth_core's call sites, renders buckets and the 2nd-hop impact block.
    siblings.rs        Sibling symbol surfacing in search results.
    grok.rs            One-call symbol bundle (def + body + callers + callees + siblings + tests).
    deps.rs            tilth_deps output: renders tilth_core's DepsResult with budget truncation.
    rank.rs            Result ranking (definition weight, basename boost, context proximity).
    facets.rs          Faceted result grouping (definitions, usages, implementations).
    strip.rs           Cognitive load stripping (comments, blank lines in expanded code).
    truncate.rs        Smart truncation to fit budget constraints.
    alloc.rs           Value-based budget allocation — keeps the highest-value blocks when output exceeds budget, not just positional tail-cut.
    glob.rs            File glob search.
    blast.rs           Blast radius — find callers of definitions touched by edits.
  session.rs           MCP session state — tracks previously expanded definitions for dedup.
  edit.rs              Hash-anchored editing (tilth_write hash mode). Hashline verification + atomic apply.
  edit_parse_check.rs  Post-edit tree-sitter parse check — diffs pre/post ERROR/MISSING nodes so tilth_write reports only errors the edit introduced.
  install.rs           `tilth install <host>` — writes MCP config for 23 hosts.
  format.rs            Output formatting helpers.
  budget.rs            Token budget enforcement.
  map.rs               Codebase map generation (CLI only, disabled as MCP tool).
  overview.rs          Project fingerprint for MCP initialization (manifest, languages, modules, deps, git). Instant orientation without a tool call.
  timeout.rs           Per-request wall-clock timeout for sync tool calls — worker thread + bounded channel, tracks abandoned threads on expiry.
  util.rs              atomic_write_bytes() — shared by edit.rs and install.rs.
  diag.rs              The one stderr sink: emit(). clippy.toml forbids std::io::stderr elsewhere.
  types.rs             Binary-only types (QueryType, ViewMode, Match, SearchResult, FacetTotals) and estimate_tokens().
npm/                   npm wrapper — postinstall downloads binary, run.js proxies to it.
scripts/fixtures/      setup_repos.py clones four real-world repos at pinned commits (README examples, speed table, parity checks).
prompts/               MCP server instruction source (mcp-base.md + mcp-edit.md). Embedded into the binary at compile time and regenerated into AGENTS.md.
AGENTS.md              User-facing copy of the MCP instructions. Generated from prompts/*.md via scripts/regen-agents-md.sh — do not edit directly.
```

## Languages supported

Rust, TypeScript, TSX, JavaScript, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift, Kotlin, Elixir, Bash.
Dockerfile, Make detected but have no tree-sitter grammar (outline returns None).

## Build, test, install

```bash
cargo build --release        # release build of the tilth binary
cargo test --workspace       # unit tests of both crates + tilth-core's tests/api.rs
cargo clippy --workspace --all-targets -- -D warnings  # lint (incl. tests)
cargo fmt --all --check      # format check
cargo install --path .       # install to ~/.cargo/bin/tilth
cargo publish --dry-run -p tilth-core  # the library packages on its own
python3 scripts/check/output_parity.py --base OLD --new NEW --repos <fixture repos>  # byte-for-byte output parity of two tilth builds
```

The toolchain is pinned in `rust-toolchain.toml`, and CI and the release build install it from there. Clippy's lints change between releases, so bumping it is a deliberate PR that re-runs clippy.

Lints live in `[workspace.lints]` in the root `Cargo.toml` (pedantic, a nursery subset, and restrictions on unwrap, expect, print and unsafe). `clippy.toml` exempts `#[test]` functions and `#[cfg(test)]` modules from the unwrap, expect, print and dbg lints, and nothing else: a helper function in `tests/*.rs` needs its own `#[expect]`. An exception is an `#[expect(lint, reason = "...")]` on the item or statement it excuses, never an `allow`: `allow_attributes` fails the build. A temporary exception's reason names its tracking issue, as in `reason = "split by responsibility in #315"`. A permanent one starts with `by design: `. `tests/expect_reasons.rs` fails on any other reason. Results go to stdout through `write_stdout` (CLI) or the MCP transport's writer, and diagnostics through `diag::emit`; `clippy.toml` forbids `std::io::stdout` and `std::io::stderr` everywhere else.

CI runs `fmt --all --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` and the tilth-core publish dry-run on every push/PR.

## Version bumps

`tilth` and `tilth-core` share one version. Update it in `Cargo.toml` (the package version and both `tilth-core` dependency lines), `crates/tilth-core/Cargo.toml` and `npm/package.json`. Tag with `v<version>` on main. `release.yml` refuses a tag that any of these disagree with, and publishes `tilth-core` to crates.io before `tilth`, because the binary depends on it by version.

Releases publish **two npm names** from the same `npm/` wrapper: the canonical unscoped `tilth` and the org anchor `@plotplot/tilth` (the `publish-npm` job renames the artifact and republishes with `--access public`). Both names have an OIDC trusted publisher on npmjs.com (`jahala/tilth` + `release.yml`), so releases need no token. `@plotplot/tilth` was bootstrapped with a one-time manual publish — npm cannot configure trusted publishing for a package that does not exist yet.

## MCP instructions

Server instructions sent via MCP protocol live in `prompts/`:

- `prompts/mcp-base.md` — base instructions for all modes (wired in as `SERVER_INSTRUCTIONS`)
- `prompts/mcp-edit.md` — appended in edit mode (wired in as `EDIT_MODE_EXTRA`)

`src/mcp/mod.rs` embeds both at compile time via `include_str!`. `AGENTS.md` is the user-facing copy; regenerate it via `./scripts/regen-agents-md.sh` after any change so both surfaces stay in lockstep. The byte-lock tests in `src/mcp/mod.rs` (`server_instructions_byte_lock`, `edit_mode_extra_byte_lock`) flag accidental drift and must be updated alongside intentional prompt edits.

Changes to MCP instructions must be surgical — no bloat. Haiku is sensitive to:

- Instruction positioning (top-weighted — put important guidance first)
- Framing ("DO NOT" works better than "IMPORTANT:" for weaker models)
- Concrete examples (tool call patterns, not abstract descriptions)

Validate instruction changes with a copeca A/B before shipping them. The in-repo benchmark harness was retired in September 2026 and is preserved under the `benchmark-archive` tag. tilth publishes no cost, accuracy or turn figures.
