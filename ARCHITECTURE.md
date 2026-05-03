# Tilth — architectural review

This document walks a reader unfamiliar with the source through tilth's
subsystems, data flow, key types, and extension points. It is written
against the `choongng/tilth@dev` fork at commit `fd3de77` (17 commits
ahead of `origin/main`, which itself tracks upstream `jahala/tilth` at
v0.7.0). Where the fork has diverged from upstream architecturally, that
is called out inline; the [Fork delta](#fork-delta) section tabulates
every fork commit.

Tilth is a single Rust binary that exposes two surfaces: a CLI
(`tilth`) and an MCP server (`tilth --mcp`). Both speak to the same
core: a query classifier, a tree-sitter-driven search engine, a smart
file reader, and supporting subsystems for diff, edit, blast-radius
analysis, and codebase mapping. The whole project is ~16.5k lines of
Rust across roughly 40 files in `src/`, plus a Cargo workspace, an
`install.rs` that writes MCP-host configs, and a benchmark harness.

## Reading order

Read in this order to learn the codebase from scratch:

1. `src/types.rs` (~190 lines) — the type alphabet. `QueryType`, `Lang`,
   `FileType`, `ViewMode`, `Match`, `SearchResult`, `FacetTotals`,
   `OutlineEntry`, `OutlineKind`. Every other module speaks in these.
2. `src/lib.rs` (~360 lines) — the public API. Five entry points
   (`run`, `run_full`, `run_expanded`, `run_callers`, `run_deps`),
   one shared dispatcher (`run_inner`), and the cascade helpers
   `single_query_search` / `multi_word_concept_search`.
3. `src/classify.rs` (~150 lines) — query string → `QueryType` by
   byte-pattern matching, in priority order.
4. `src/main.rs` and `src/mcp.rs` — the two surfaces. Read `main.rs`
   first to see the clap shape; `mcp.rs` is the larger one but follows
   the same dispatch logic in JSON-RPC clothing.
5. `src/search/mod.rs` and `src/search/symbol.rs` — the search
   subsystem's spine. Then the rest of `src/search/` opportunistically.
6. `src/read/mod.rs` and `src/read/outline/` — the read subsystem.
7. Subsystems in any order: `src/lang/`, `src/index/`, `src/diff/`,
   `src/edit.rs`, `src/overview.rs`, `src/map.rs`, `src/install.rs`.

The single most useful function for orienting yourself is
`lib.rs::run_inner` — every search query funnels through it.

## Layout map

```
src/
├── lib.rs           Public API + dispatch (run / run_full / run_expanded /
│                    run_callers / run_deps)
├── main.rs          CLI binary (clap parser → lib calls or mcp::run)
├── mcp.rs           MCP server: JSON-RPC stdio loop, tool dispatch,
│                    per-request timeout / abandoned-thread tracking
├── classify.rs      Query string → QueryType (7-rule precedence ladder)
├── types.rs         Shared types: QueryType, Lang, FileType, ViewMode,
│                    Match, SearchResult, FacetTotals, OutlineEntry
├── error.rs         TilthError (NotFound, PermissionDenied,
│                    InvalidQuery, IoError, ParseError)
├── format.rs        Output headers, line numbering, hashlines
├── budget.rs        Token cap (truncate output to a token budget)
├── session.rs       Per-MCP-process counters: reads, searches, top
│                    queries, hot dirs, expanded-set dedup
├── cache.rs         OutlineCache: rendered outlines + parsed
│                    tree_sitter::Tree, both keyed by (path, mtime)
├── overview.rs      Project fingerprint emitted at MCP `initialize`
├── map.rs           tilth --map: structural tree of the codebase
├── install.rs       Writes MCP server entries into ~20 host configs
├── edit.rs          Hash-anchored line edits + EditResult diff preview
│
├── search/          Search engine (~7300 lines, 13 files)
│   ├── mod.rs           Walker, ignore policy, formatter, dispatch
│   ├── symbol.rs        Tree-sitter definition detection per language;
│   │                    markdown-heading defs; usage matching
│   ├── content.rs       Literal content search (ripgrep internals)
│   ├── rank.rs          Match scoring: 11 boosts/penalties + recency
│   ├── facets.rs        Group matches into definitions /
│   │                    implementations / tests / usages_local /
│   │                    usages_cross
│   ├── truncate.rs      Display cap policy (per-facet limits)
│   ├── strip.rs         Cognitive-load stripping in expanded source
│   ├── siblings.rs      Tree-sitter sibling extraction (with cached
│   │                    Query objects)
│   ├── callers.rs       Find call sites of a symbol (`tilth_search
│   │                    kind:callers`); enclosing-scope annotation
│   ├── callees.rs       Resolve function calls inside a definition
│   ├── deps.rs          Blast-radius analysis (`tilth_deps`)
│   ├── glob.rs          Glob query → file list (`tilth_files`)
│   └── blast.rs         Symbol-level blast radius
│
├── read/            Read engine (~1800 lines, 8 files)
│   ├── mod.rs           read_file decision tree, section reader,
│   │                    heading resolver, suggestion fallback
│   ├── imports.rs       Resolve "Related: file1, file2" hints
│   └── outline/
│       ├── mod.rs       Dispatch by FileType
│       ├── code.rs      Tree-sitter outline for code
│       ├── markdown.rs  ATX-heading outline with fence tracking
│       ├── structured.rs JSON/YAML/TOML "keys" view
│       ├── tabular.rs   CSV/TSV head/tail + column count
│       ├── test_file.rs Test-suite outline (describe/it grouping)
│       └── fallback.rs  Last-resort head/tail
│
├── lang/            Language detection + tree-sitter wrapper
│   ├── mod.rs           detect_file_type, package_root, FileType ↔ Lang
│   ├── detection.rs     Binary / generated / minified detection
│   ├── treesitter.rs    DEFINITION_KINDS, extract_definition_name
│   └── outline.rs       outline_language(Lang) → tree_sitter::Language;
│                        node_to_entry walker (~800 lines)
│
├── index/           Pre-computed indexes (currently allocated, not all
│                    used in the active code path)
│   ├── mod.rs
│   ├── symbol.rs        SymbolIndex: name → [SymbolLocation] DashMap
│   └── bloom.rs         Per-file Bloom filter for fast "does X contain Y?"
│                        (BloomFilterCache wraps the per-file filters)
│
└── diff/            Structural diff (~3600 lines, 5 files)
    ├── mod.rs           DiffSource resolution; diff() pipeline orchestrator
    ├── parse.rs         Parse unified diff text → FileDiff structs
    ├── overlay.rs       FileDiff + on-disk source → structural FileOverlay
    │                    (changed symbols with confidence scores);
    │                    cross-file move detection
    ├── matching.rs      Symbol identity matching across before/after
    └── format.rs        FileOverlay → human-readable output
```

## Two entry surfaces

Both surfaces produce the same string outputs from the same internals.
The CLI is a thin shell around `lib.rs`; the MCP server is a thicker
shell with its own concurrency model.

### CLI (`src/main.rs`)

A clap `derive`-style parser (`Cli` struct) accepts a free-form `query`
plus flags. The mode is determined by mutually-exclusive flags
(`--callers`, `--deps`, `--map`, `--mcp`, `--edit`, `--full`,
`--expand`, `--section`). Subcommands are limited to one entry —
`Install` — which delegates to `install::run`.

The default mode dispatches into `lib::run` (or `run_full`,
`run_expanded`, `run_callers`, `run_deps`, `map::generate`) based on
the active flags. The output is printed verbatim to stdout. JSON output
(`--json`) emits a serde-serialized `SearchResult`; otherwise the
human-readable formatter wins.

`main.rs` also handles two operational concerns the MCP path doesn't
touch: it detects terminal height via `TIOCGWINSZ` (fork commit
`4fa6832` replaced a stale 24-row fallback) and writes shell completions
(`--completions <shell>`) using `clap_complete`.

### MCP server (`src/mcp.rs`)

Invoked as `tilth --mcp` (or `tilth --mcp --edit` for edit mode). The
binary becomes a JSON-RPC server speaking newline-delimited messages
over stdio. The body is a hand-rolled loop, not a framework — about 1300
lines total.

Startup (`mcp::run`) resolves a project root in priority order:
explicit `--scope` argument > MCP `roots/list` response > nearest
`package_root(cwd)` > `cwd`. The chosen root becomes the process's
working directory, so every later tool call resolves relative paths
against it. This was added specifically to handle MCP hosts (Codex was
the documented case) that launch tilth with `cwd=/`.

The request loop (`mcp.rs:139-220`) parses each line as JSON, dispatches
on `method`, and writes the response. Two methods are special:

- `initialize` — emits `protocolVersion`, capabilities, `serverInfo`,
  and an `instructions` string. The instructions are the
  `SERVER_INSTRUCTIONS` constant (recently rewritten on `fd3de77` as a
  pre-flight gate naming the exact Bash commands and host tools to
  avoid, with concrete `<bad>→<good>` rewrites). When
  `TILTH_NO_OVERVIEW` is unset, `overview::fingerprint(cwd)` is
  prepended — a project summary built in <250ms (a stderr warning fires
  if it overruns).
- `tools/list` — returns the tool schemas. `tools/call` is the workhorse
  and goes through `handle_tool_call`.

Tool dispatch is routed by name through `dispatch_tool` to
`tool_read` / `tool_search` / `tool_files` / `tool_deps` / `tool_diff` /
`tool_session` / `tool_edit` (the last only in edit mode).
`tilth_map` is exposed but always returns "use tilth_search instead"
— a deliberate redirect introduced when the structural map proved less
useful than search for agent workflows.

Concurrency: each `tools/call` spawns a worker thread, communicates via
`mpsc::channel`, and waits with `recv_timeout`. The default per-tool
timeout is 90s (`TILTH_TIMEOUT` env override). On timeout, the response
goes back immediately with an `isError: true` payload; the worker thread
is **abandoned** (not joined, not killed) — this is the safest option in
Rust since cancelling a thread mid-tree-sitter-parse is unsound. A
process-wide `ABANDONED_THREADS` counter logs to stderr once
accumulation hits 3.

Edit mode (`--edit`) appends an `EDIT_MODE_EXTRA` instruction block
describing `tilth_edit` and unlocks the `tilth_edit` dispatch arm.

## Query pipeline

The whole search/read/glob/regex flow funnels through one function:
`lib.rs::run_inner`. It is the single most useful place to set a
breakpoint when learning the codebase.

```
        run / run_full / run_expanded
                    ↓
             ┌──────────────┐
             │  run_inner   │
             └──────┬───────┘
                    │
        classify(query, scope) → QueryType
                    │
            ┌───────┴───────┐
            │               │
   contains ',' &&     QueryType arms
   2..=5 idents             │
   (multi-symbol)           │
            │       ┌───────┼─────────┐
            │  FilePath   Glob    other QT
            │       │       │         │
            │   read_file  glob   use_expanded?
            │       │       │       │       │
            │       │       │      yes      no
            │       │       │       ↓        ↓
            │       │       │   run_query_   run_query_
            │       │       │   expanded     basic
            │       │       │       │        │
            └───────┴───────┴───────┴────────┴────► budget::apply
```

`use_expanded` is `expand > 0 && QueryType is search-like`. Its purpose
is to gate the inline-source paths from the outline-only paths: read
queries (`FilePath`, `Glob`) ignore `--expand` because their output
shape is already determined.

The two dispatch helpers (`run_query_basic`, `run_query_expanded`) match
on the `QueryType` arms. They look almost identical, but with two
non-obvious twists:

- **Single-word concept fallthrough.** `Concept(text)` (when
  `text.contains(' ')` is false) drops into `single_query_search` in the
  basic path: try `search_symbol_raw`; if it returned definitions
  (concept queries prefer real defs), accept; otherwise try
  `search_content_raw`; otherwise — if the original symbol search had
  any usages even without definitions — fall back to those; otherwise
  `NotFound` with a similar-file suggestion. The `Fallthrough` arm
  (path-like-but-didn't-resolve queries) shares the same cascade with
  `prefer_definitions: false` so any symbol match is accepted.
- **Multi-word concept relaxation.** `Concept(text)` with whitespace
  goes through `multi_word_concept_search`. First tries an exact-phrase
  content search; on no hits, escapes each word and runs a relaxed
  regex (`a.*b|b.*a` for two words; full OR for 3+). Then ranking's
  `multi_word_boost` does the actual relevance lifting.

The expanded path skips the cascades — `Concept` and `Fallthrough` go
straight to `search_symbol_expanded`. The comment explains why: the
expanded variant already carries the inline source so the
definitions-vs-content distinction doesn't earn its keep.

Multi-symbol search (`search_multi_symbol_expanded`) sits at the top of
`run_inner`, before the `QueryType` dispatch. It activates when the
query contains a comma, every comma-separated piece is an identifier
(per `classify::is_identifier`), and the count is 2 to 5. Six or more
identifiers are an explicit `InvalidQuery` error. Below 2 is "just look
up the one symbol." This explicit gate prevents collisions with regex
(`/foo,bar/`) and brace globs (`*.{rs,ts}`).

Token budgeting (`budget::apply`) is a final filter applied to whatever
string came out of the dispatch. It walks the output and snips off
content past the budget; the entry-format header is preserved so the
caller knows truncation happened.

## Classification (`src/classify.rs`)

Tilth's "what kind of query is this" decision is a 7-rule byte-pattern
ladder, in priority order:

1. **Slash-wrapped regex** — `/pattern/` with regex metacharacters
   inside. Must come before glob detection because `[`, `{`, `*`
   overlap. The metachar guard means `/src/` isn't misclassified as
   regex when it's actually a path.
2. **Glob** — contains `*`, `?`, `{`, or `[` and no spaces. The
   no-spaces guard distinguishes real globs from content like
   `import { X }`.
3. **Path** — starts with `./` or `../`, or contains `/` without
   spaces. If the resolved path exists, returns `FilePath`; otherwise
   `Fallthrough` so the symbol/content cascades still get a shot.
4. **Dotfile** — starts with `.`; check the filesystem for an exact
   match (catches `.gitignore` etc.).
5. **Pure numeric** — all ASCII digits → `Content` search (HTTP codes,
   error numbers).
6. **Bare filename** — has an extension or is a known
   extensionless-name (README, Makefile, etc.); check the filesystem.
7. **Identifier vs concept** — starts with letter/underscore/$/@, no
   whitespace. `looks_like_exact_symbol` (case + length heuristics)
   splits `Symbol` from `Concept`.
8. **Multi-word phrase** — 2-4 simple words → `Concept`.

When nothing matches, `Fallthrough` is the catch-all. Classification is
deliberately syntactic — no regex engine, no fuzzy matching — so adding
a query type means adding a `QueryType` enum arm and one rule. The
ordering encodes the precedence policy ("globs before paths," "regex
first if metachars present," "filesystem only checked when the shape
suggests a file").

## Search subsystem (`src/search/`)

The search subsystem is the largest area of code and the thing most fork
work has touched. Roughly 7300 lines across 13 files.

### Walker and ignore policy (`mod.rs`)

`walker(scope, glob)` builds an `ignore::WalkParallel` with tilth's
opinionated defaults:

- **Always-skipped directories** (`SKIP_DIRS`): build artifacts,
  dependency caches, VCS internals — `.git`, `node_modules`, `target`,
  `dist`, `build`, `__pycache__`, `vendor`, `.next`, `.venv`,
  `.mypy_cache`, etc. ~30 entries. Enforced even with
  `TILTH_NO_IGNORE=1`.
- **Per-repo `.gitignore`** is honored by default
  (`apply_ignore_settings`).
- **`.tilthignore`** — gitignore syntax — is layered on top.
  `!path` lines re-include files that `.gitignore` would have excluded.
- **Global gitignore** and `.git/info/exclude` are deliberately
  **never** consulted; those tend to list per-engineer files (agent
  state, editor scratch) that grep-style queries should still find.
- **Symlinks are followed** (`follow_links(true)`). This is the
  upstream `feat: follow symlinks in all file walkers (#46)` change
  and creates a known duplicate-match problem when a path is reachable
  via multiple symlink chains — see [Open
  threads](#open-architectural-threads).
- **Threads**: `TILTH_THREADS` env or `available_parallelism / 2`
  clamped to `[2, 6]`.

The glob filter, when present, is applied via `OverrideBuilder` —
gitignore-style with whitelist, negation (`!`), and brace expansion.

### Symbol search (`symbol.rs`)

The biggest file in the subsystem (~1280 lines). Three layers stacked:

1. **`search()`** — top-level entry. Walks files via `walker`, parses
   each via `OutlineCache::get_or_parse` (cap: 500 KB), dispatches to
   the right per-language definition extractor, runs a usage scan,
   merges definitions + usages, ranks, facets (when >5 matches), and
   computes `FacetTotals` from the pre-cap merged set. Returns a
   `SearchResult`.
2. **Per-language definition extraction**:
   - `find_defs_treesitter` walks the tree-sitter tree and picks
     definitions whose name field equals the query. Uses
     `DEFINITION_KINDS` from `lang/treesitter.rs` (about 30 node-kind
     strings spanning every supported grammar).
   - `find_defs_heuristic_buf` is a regex-free fallback for languages
     without grammars (Dockerfile, Make).
   - `find_defs_markdown_buf` handles ATX headings as
     `def_weight = 30` definitions, with fence-state tracking
     (fork commit `8882d72`) to skip ```` ``` ```` and `~~~` blocks.
3. **Usage matching** (`find_usages`) — line-level identifier
   matching, with word-boundary checks to avoid substring false hits.
   Each usage is annotated with its enclosing definition (fork commit
   `7cad3f1`, `walk_to_enclosing_definition`). When fork commit
   `c8ae866` lands the `stratum_for_display` sort, the renderer shows
   real code defs (weight ≥60) ahead of doc-heading defs (weight 30)
   when the display cap is binding.

The markdown branch deserves a callout because it interacts with two
seemingly-similar things in different files. `read/outline/markdown.rs`
also tracks fence state for outlines; `find_defs_markdown_buf` does
the same job for the search path. The two implementations share the
`parse_atx_heading` parser but maintain their own pre-pass loops. See
[Open threads](#open-architectural-threads).

### Content / regex search (`content.rs`, regex paths)

Both layers use the ripgrep crates (`grep-regex`, `grep-searcher`).
Content search escapes the query into a regex literal and runs a
case-sensitive search by default; regex search passes through verbatim
with the `/pattern/` wrapper stripped. Both paths return a
`SearchResult` with `definitions = 0` and all matches in `usages`. They
do not populate `FacetTotals` (the `Default` zero-value is correct here
because the renderer suppresses facet headers on zero totals).

### Ranking (`rank.rs`)

`sort(matches, query, scope, context)` runs a single-pass scoring loop
using 11 boosts/penalties:

- `basename_boost` — files whose basename matches the query rank
  higher.
- `definition_name_boost` — exact-vs-fuzzy match between `def_name` and
  query.
- `query_intent_boost` — heuristics like "looks like a test query."
- `exported_api_boost` — public/exported defs outrank internals.
- `multi_word_boost` — for relaxed multi-word queries, reward matches
  containing more of the words.
- Penalties: `fixture_penalty` (test fixtures), `non_code_penalty`
  (markdown for code-shaped queries), `incidental_text_penalty` (the
  word appearing in a comment but not as a symbol).
- `scope_proximity` and `context_proximity` — closer-to-the-edited-file
  matches outrank distant ones (the MCP `context` argument feeds this).
- `recency` — recently-modified files outrank stale ones.

### Faceting and truncation (`facets.rs`, `truncate.rs`, `strip.rs`)

`facet_matches` partitions a `Vec<Match>` into five groups:
`definitions`, `implementations`, `tests`, `usages_local`,
`usages_cross`. "Local" means same package root as the primary
definition (`lang::package_root`). The faceted output shows facet
headers and per-facet limits.

`truncate.rs` enforces those limits. Two recent fork changes shape what
the user sees:

- `ac21c9b` adds `FacetTotals` to `SearchResult` so the renderer can
  print `displayed/total` headers (`Definitions (10/24)`).
- `cc1525a` adds the per-facet hidden-count tail
  (`... and 14 more definitions. Narrow with scope.`).

`strip.rs` ("cognitive load stripping") prunes line-level noise from
expanded source bodies — logging statements, redundant comments,
consecutive blank lines — to fit more useful content in the same
budget. Per-language rules in `StripLang` (Rust, Python, Go, JS/TS,
Java/Kotlin/C#, C/C++).

### Relational queries (`callers.rs`, `callees.rs`, `siblings.rs`, `deps.rs`, `blast.rs`)

These produce derived views from the same walker + tree-sitter
machinery:

- **Callers** (`callers.rs`, ~1070 lines, the second-largest file in
  the subsystem). `find_callers` and `search_callers_expanded` resolve
  call sites of a symbol. Two implementations: a single-symbol
  tree-sitter path and a batch path that processes multiple symbols in
  one walk. Each caller is annotated with its `EnclosingScope`
  (fork commit `7cad3f1`) so the user sees `[usage in function foo]`
  instead of just a line number. `no_callers_message` produces the
  helpful "no callers found, here are similar names" output (also a
  fork-side polish, see Session 51).
- **Callees** (`callees.rs`) — resolve outgoing calls inside a
  definition body. Drives the `── calls ──` footer under each
  expanded match.
- **Siblings** (`siblings.rs`) — extract the surrounding outline
  context (the entries immediately before and after the matched
  definition). Compiles tree-sitter `Query` objects lazily and caches
  them in a process-wide `LazyLock<Mutex<HashMap>>`. The cache key
  uses the query string's pointer address (`&'static str`) so distinct
  queries against the same language stay separated.
- **Deps** (`deps.rs`, ~580 lines) — `analyze_deps` is what
  `tilth_deps` runs. Returns a `DepsResult` with `Uses` (local +
  external) and `Used by`. `format_deps` does the human output. The
  external-dep stdlib heuristic (`is_stdlib`) per-language; module-path
  validation (`is_valid_module_path`) avoids treating relative paths as
  packages.
- **Blast** (`blast.rs`, ~290 lines) — symbol-level blast radius for
  diff workflows; called by `tilth_diff --blast`.

### Glob (`glob.rs`)

Thin wrapper that builds a walker with a single override pattern and
emits `path  (~N tokens)` lines, sorted by relevance. The token
estimate comes from `estimate_tokens(byte_len)` — `bytes / 4` ceiling
division.

## Read subsystem (`src/read/`)

`read::read_file(path, section, full, cache, edit_mode)` is the
gateway. The decision tree, in order:

1. `metadata` errors → typed `TilthError::NotFound` /
   `PermissionDenied` / `IoError` with a `suggest_similar` fallback
   (Levenshtein on directory siblings).
2. **Directory** → `list_directory` (sorted entries, file types).
3. **Empty file** → header with `ViewMode::Empty`.
4. **`section: Some(...)`** → `read_section`. Either a line range
   (`"45-89"`) or a heading address (`"## Architecture"`,
   resolved by `resolve_heading` via fence-aware ATX scan). Returns
   the verbatim slice regardless of file size.
5. **mmap + binary check** — `is_binary` looks for null bytes in the
   first 512 bytes via `memchr`. Binary files emit just a header.
6. **Generated** — by name (`Cargo.lock`, `package-lock.json`, etc.) or
   content marker (`@generated`, `DO NOT EDIT`, `auto-generated`,
   etc.).
7. **Minified** — `.min.<ext>` convention or, for files >100KB, a
   newline-density heuristic.
8. **Full mode + oversized** — guard for `--full` on huge files. Caps
   at `TILTH_FULL_SIZE_CAP` (default in `full_read_size_cap()`); above
   the cap, returns the outline with a banner explaining how to
   override.
9. **Below the token threshold or `--full`** → return full content
   (with `format::hashlines` if `edit_mode`, plain `number_lines`
   otherwise).
10. **Otherwise** → `outline::generate` per `FileType`, cached in
    `OutlineCache::get_or_compute`.

The `outline/` submodule dispatches on `FileType`:

- `code.rs` — calls `lang::outline::walk_top_level` with the parsed
  tree-sitter tree, formats `OutlineEntry` instances as `[start-end]
  symbol_name signature`.
- `markdown.rs` — ATX headings outline with fence-state tracking.
- `structured.rs` — JSON/YAML/TOML "keys" view (top-level keys + their
  shapes).
- `tabular.rs` — CSV/TSV header + first/last few rows.
- `test_file.rs` — describe/it grouping for test files.
- `fallback.rs` — last-resort head/tail when nothing else applies.

Two helpers feed the MCP layer:

- `would_outline(path)` lets MCP decide whether to append related-file
  hints (`"Related: foo.ts, bar.ts"`) — only meaningful when the user
  is going to see an outline.
- `imports::resolve_related_files` does the related-file resolution
  for the same hint.

## Language layer (`src/lang/`)

`Lang` and `FileType` are carried through the type system so downstream
code never re-detects file kind. Adding a language is mechanical — the
compiler tells you every site to update:

- Add an arm to the `Lang` enum (`types.rs`).
- Add an extension match to `lang::detect_file_type`.
- Add a tree-sitter binding to `lang::outline::outline_language` (or
  return `None` to skip outlines).
- Optionally add per-language strip rules in `search/strip.rs`.

The set today is 18 languages: Rust, TypeScript, TSX, JavaScript,
Python, Go, Java, Scala, C, C++, Ruby, PHP, Swift, Kotlin, C#, Elixir,
Dockerfile, Make. Tree-sitter grammars exist for all but Dockerfile and
Make.

`lang::detection` is the heuristic layer:

- `is_binary(buf)` — null byte in first 512 bytes (single SIMD
  `memchr` pass).
- `is_generated_by_name`, `is_generated_by_content` — lockfile names
  and `@generated`/`DO NOT EDIT`-style markers (12 variants).
- `is_minified_by_name`, `is_minified_by_content` — naming convention
  + density check.

`lang::treesitter::DEFINITION_KINDS` is the canonical list of
tree-sitter node kinds tilth treats as definitions across all
languages. It's a flat `&[&str]` slice — additions are line-noise
edits.

`lang::package_root(path)` walks up looking for known manifests
(`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, etc.) and is
used by both ranking (`scope_proximity`) and faceting
(`is_same_package`). It's also what the MCP server uses at startup to
resolve a project root from cwd.

## Caches and indexes

Tilth keeps four pieces of state across calls. Lifetimes are
deliberately scoped — only `OutlineCache`, `Session`, `SymbolIndex`,
and `BloomFilterCache` survive between MCP requests.

### `OutlineCache` (`cache.rs`)

Two `DashMap` tables, both keyed by `(PathBuf, SystemTime)`:

- `entries`: rendered outline strings (`Arc<str>`).
- `parsed`: parsed tree-sitter trees (`Arc<ParsedFile>`,
  carrying `Arc<String>` content, `tree_sitter::Tree`, and `Lang`).

Entries are computed lazily via `get_or_compute` and `get_or_parse`.
Both use the entry-API to avoid TOCTOU races. There is no eviction —
`mtime` change on the keyed file means the old entry is never hit
again, and idle entries simply sit there. For long-running MCP
processes this is an O(touched files) cache; the trade-off is that a
file repeatedly modified will accumulate stale entries until the
process exits. In practice the working set of active files is small
enough that this hasn't been a concern.

The 500 KB ceiling on `get_or_parse` keeps tree-sitter parses from
blowing up memory on generated TypeScript bundles or vendored
single-file libraries. Above the cap, `get_or_parse` returns `None` and
callers fall back to text-based heuristics.

### `Session` (`session.rs`)

Per-MCP-process counters for the `tilth_session` tool. Tracks read
counts, search counts, top queries, hot directories (each indexed
file's parent), and an `expanded` set keyed by `path:line` so re-expanding
a previously shown definition can return `[shown earlier]` instead of
duplicating the source body.

Concurrency: `AtomicUsize` counters; `Mutex<HashMap/HashSet>` for the
maps. Poison handling is uniform via
`unwrap_or_else(PoisonError::into_inner)` — tilth deliberately doesn't
try to handle a poisoned mutex specially; the data is best-effort
telemetry.

### `SymbolIndex` (`index/symbol.rs`)

Pre-computed symbol-to-file mapping. `DashMap<Arc<str>, Vec<SymbolLocation>>`
plus a `DashMap<PathBuf, SystemTime>` for tracking what's been indexed.
`build()` parses every code file in scope using tree-sitter and rayon
in parallel, populating the maps.

**Status**: allocated in both `lib.rs` and `mcp.rs`, threaded through
the search-expanded entry points, and explicitly unused —
`search/mod.rs:194` does `let _ = index;` with the comment "Index is
available but not yet used for search fast-path. Build will be
triggered when the lookup path is wired in." This is a planned but
inert subsystem; today every symbol search still does a full walk.

### `BloomFilterCache` (`index/bloom.rs`)

Per-file Bloom filters that answer "does file X contain symbol Y?" in
constant time. Used by `callers.rs` and `callees.rs` to skip files
that can't possibly contain a referenced identifier, before the
expensive tree-sitter parse. Identifier extraction is a byte-level
state machine — fast enough to run on every cache miss without needing
a tree-sitter pass.

This is actively used in the relational-query paths (it's why
`tilth_search kind:callers` doesn't take O(N files) tree-sitter parses
on every query).

## Diff and edit subsystems

These two are separate from the search/read pipeline but share the same
walker and tree-sitter infrastructure. Both are large enough to be
peers of the search subsystem but stay quiet because they sit behind a
single tool each.

### Diff (`src/diff/`)

`tilth_diff` (and the `tilth --diff` CLI shape) ends up in
`diff::diff`. The pipeline:

1. **Resolve source** (`resolve_source`) — `DiffSource` is a
   six-armed enum: `GitUncommitted` (default), `GitStaged`, `GitRef`
   (any rev or rev range), `Files(a, b)` (file-to-file), `Patch(path)`
   (read a `.patch` file), `Log(range)` (per-commit summaries).
2. **`run_git_diff`** shells out to `git diff` with the right args
   (or reads the patch file).
3. **`parse::parse_unified_diff`** turns the raw text into
   `Vec<FileDiff>` — header, hunks, lines.
4. **`overlay::compute_overlay`** combines each `FileDiff` with the
   on-disk source to produce a `FileOverlay` — a structural view of
   changed *symbols* with confidence scores
   (`MatchConfidence::High/Medium/Low/None`).
5. **`overlay::cross_file_matching`** detects symbols moved between
   files.
6. **`overlay::signature_warnings`** flags signature changes that
   might break callers.
7. **Search filter** (`filter_by_search`) — narrows to symbols/files
   matching a substring, if `--search` was passed.
8. **Blast radius** (`compute_blast`) — for each signature-changed
   symbol, list of impacted files, gated on `--blast`.
9. **`format::format_diff`** — `[+]`/`[-]`/`[~]`/`[~:sig]` line
   prefixes; budgeted via `budget::apply`.

`diff_log(range, scope, budget)` is a separate pipeline for `--log
HEAD~5..HEAD` summaries — runs `git log --pretty + git show` per commit
and emits a structural change list per commit.

### Edit (`src/edit.rs`)

Hash-anchored line edits, ~300 lines. The `Edit` struct carries a
start anchor (`<line>:<hash>`), an optional end anchor (range edit), and
a content string (empty = delete). `apply_edits` reads the file, hashes
each line, validates that supplied anchors match, and rewrites the
file with the substituted content. The hash mismatch case is the
primary safety: if the file changed since `tilth_read`, the agent gets
`Mismatch` back and is forced to re-read.

`EditResult` is `Ok` / `Mismatch { expected, actual, line }`. The
formatter (`format_diffs`) produces a compact before/after diff
summary.

The edit tool surfaces a callee-callers warning when a function
signature changes — `apply_edits` calls into `search::callers` to
list possibly-affected sites. This is opt-in (`diff: true` arg) but
designed to make the agent re-check before chaining edits.

## MCP server (`src/mcp.rs`)

(Briefly above, in detail here.)

The MCP server is a single hand-rolled JSON-RPC loop reading
newline-delimited JSON from stdin and writing to stdout. Three concerns
beyond the surface API are worth understanding:

**Roots discovery.** After answering `initialize`, if the client
declared `roots` capability and `--scope` wasn't explicit, the server
sends a `roots/list` request to the client. The client's response (if
any) becomes the new working directory. This is what makes tilth work
correctly with hosts that launch it from a non-project directory.

**Per-request thread model.** Every `tools/call` is run on a fresh
`std::thread::spawn`-ed worker. The main loop waits with
`mpsc::recv_timeout` (90s default). On timeout, the response goes
back; the worker is **abandoned**, not joined. There's a counter and
a stderr warning at 3 abandoned threads. The reason this design is
necessary: aborting a thread mid-tree-sitter-parse is unsafe, so
"forget about it" is the only correct option short of a full async
rewrite.

**Tool definitions.** `tool_definitions(edit_mode)` returns the schemas
emitted at `tools/list`. These are the canonical source for argument
shapes; the in-process `dispatch_tool` and the per-tool functions
(`tool_search`, `tool_read`, etc.) parse them by `serde_json::Value`
lookups rather than typed structs.

**Instructions injection.** The `SERVER_INSTRUCTIONS` constant is the
strategic guidance every host gets at `initialize`. Fork commit
`fd3de77` rewrote it as a pre-flight gate after observing that
agents kept reaching for `Bash(grep/cat/find)` despite the older
"DO NOT use Grep/Read/Glob" rule. The new shape names exact bad
commands and provides `<bad>→<good>` rewrites. Edit mode appends an
`EDIT_MODE_EXTRA` block with the `tilth_edit` instructions.
`overview::fingerprint(cwd)` is prepended unless `TILTH_NO_OVERVIEW` is
set — about a 100-line summary of language counts, manifests, hot
files, and git context.

## Cross-cutting modules

A handful of small files cut across every subsystem.

- **`format.rs`** — `file_header` / `binary_header` / `search_header`
  produce the `# path (N lines, ~Mk tokens) [outline]`-style banners.
  `number_lines(content, start)` and `hashlines(content, start)` are
  the line-numbering / hash-anchored variants.
- **`budget.rs`** — `apply(output, budget)` truncates a finished
  output string to a token budget. Used by every entry point and tool.
- **`error.rs`** — `TilthError` enum (`NotFound`, `PermissionDenied`,
  `InvalidQuery`, `IoError`, `ParseError`) with `Display` impls that
  format human-readable messages and include suggestions where
  relevant.
- **`types.rs`** — already covered above; the type alphabet that every
  module speaks. `Match` / `SearchResult` / `FacetTotals` / `Lang` /
  `FileType` / `ViewMode` / `OutlineEntry` / `OutlineKind` are the
  ones that span many modules.

## Auxiliary modules

Three modules sit outside the search/read flow but are still
user-facing.

- **`overview.rs`** — `fingerprint(root)` walks files (depth 2),
  detects primary language, parses the project manifest (`Cargo.toml`,
  `package.json`, `go.mod`, `pyproject.toml`), reads `git` context
  (`branch`, `uncommitted`, `recent commits`), and emits a few
  hot-file lines. Wrapped in `catch_unwind` and a 250ms wall-clock
  budget; warn-on-overrun via stderr. Output is the project summary
  that the MCP `initialize` response prepends to `SERVER_INSTRUCTIONS`.
- **`map.rs`** — `tilth --map` generates a structural codebase tree:
  per-directory token estimates, top-level symbols per file, depth
  control. The `dispatch_tool` arm explicitly disables this for MCP
  ("use tilth_search instead").
- **`install.rs`** — `tilth install <host>` writes tilth's MCP server
  entry into a host's config file. Supports about 20 hosts:
  claude-code, cursor, windsurf, vscode, claude-desktop, opencode,
  gemini, codex, amp, droid, antigravity, zed, copilot-cli, augment,
  kiro, kilo-code, cline, roo-code, trae, qwen-code, crush, pi. Each
  host has its own config-file location and JSON/TOML format quirks;
  `resolve_host` returns a `HostInfo` that drives the right writer.

## Fork delta

The fork (`choongng/tilth@dev`, 17 commits ahead of `origin/main`,
which is at upstream v0.7.0) groups into five themes. Listed
oldest-first.

| Commit    | Theme                | Summary |
|-----------|----------------------|---------|
| `7e4d963` | Agent usability      | Respect `.gitignore` by default; add `.tilthignore` and `--no-ignore` escape. |
| `16212fc` | Agent usability      | Make `--full` imply expand-all on search queries; clarify `--full`/`--expand` help. |
| `4fa6832` | Agent usability      | Detect terminal height via `TIOCGWINSZ`; stop paging on stale 24-row fallback. |
| `d621828` | Agent usability      | Don't tag quoted-code mentions as definitions; recognise markdown headings. |
| `d8c9617` | Agent usability      | Ensure CLI output ends with a newline. |
| `e706d99` | Agent usability      | Thread `.gitignore` + `.tilthignore` guidance through `search`/`deps` tool descriptions. |
| `87b0b94` | Agent usability      | Drop `OutlineKind::Export` doubling; recurse into wrapped declaration. |
| `7cad3f1` | Enclosing scope      | Annotate usages with their enclosing scope via tree-sitter — `[usage in function foo]` instead of bare line numbers (touches `search/callers.rs`, `search/symbol.rs`). |
| `02e9a51` | Markdown sections    | Expand markdown-heading definitions to the section body (precursor to `e1785a6`). |
| `7d76b16` | Heading hierarchy    | Fix inverted markdown heading levels in search output. |
| `3e10ab6` | Search output        | Render grouped-usage entries as H3 to match single-match path. |
| `8882d72` | Search output        | Track fence state in markdown heading detection (`find_defs_markdown_buf`). |
| `c8ae866` | Search output        | Prefer code definitions over doc-heading defs when filling display cap (`stratum_for_display`). |
| `ac21c9b` | Search output        | Show `displayed/total` in facet headings via `FacetTotals`. |
| `cc1525a` | Search output        | Emit per-facet hidden-count tail; drop global tail on facet path. |
| `99c4a3d` | Search output        | Inline section body for markdown-heading defs in default preview. |
| `fd3de77` | Server instructions  | Tighten `SERVER_INSTRUCTIONS` into a pre-flight gate. |

The seven "agent usability" patches are the oldest layer and cover
everything from default ignore behaviour to terminal sizing to output
trailing-newline polish — they predated the search-output project that
landed Sessions 51-52. The `feat/usage-enclosing-scope-ast`
(`7cad3f1`) and `feat/markdown-section-span-expansion` (`02e9a51` →
`7d76b16` → six search-output follow-ups) trees are the two
substantial features. `fd3de77` is the latest commit, a small but
focused MCP-instructions rewrite.

None of these have an obvious upstream blocker; they are
fork-divergent because of bandwidth / drift, not architectural
disagreement. The most upstream-ready cluster is the
`feat/markdown-section-span-expansion` line — small, additive, with
tests, and not entangled with other unmerged work. The
`feat/usage-enclosing-scope-ast` adds a public-ish API
(`EnclosingScope`, `enclosing_definition_at`) and would benefit from a
short upstream design discussion.

## Extension points

Concrete "if you wanted to change X, edit Y":

- **Add a new language.** `Lang` arm in `types.rs` → extension match in
  `lang::detect_file_type` → tree-sitter binding in
  `lang::outline::outline_language`. The compiler will tell you what
  else needs touching (the `match` over `Lang` in
  `read/outline/code.rs::format_signature`, etc.). Optionally
  per-language stripping rules in `search/strip.rs::detect_lang`.
- **Add a new query type.** `QueryType` arm in `types.rs` → rule in
  `classify::classify` (place by precedence) → match arms in
  `lib::run_query_basic` and `lib::run_query_expanded` → handler in
  `search/`.
- **Add a new MCP tool.** Schema in `mcp::tool_definitions(edit_mode)`
  → dispatch arm in `mcp::dispatch_tool` → `tool_*` function near the
  others. If the tool needs cross-call state, add it to the
  `Session`/`OutlineCache` argument list and propagate.
- **Add a new search facet.** `FacetTotals` field in `types.rs` →
  `FacetedResult` field + partition rule in `search/facets.rs` →
  per-facet limit in `search/truncate.rs` → renderer entry in
  `search/mod.rs::format_search_result`.
- **Add a new outline strategy** (e.g. for a non-code structured
  format). `FileType` arm in `types.rs` → extension match in
  `lang::detect_file_type` → outline file in `read/outline/` →
  dispatch arm in `read/outline/mod.rs`.
- **Add a new edit policy** (e.g. structured-edit instead of
  hash-anchored). New `Edit*` struct in `edit.rs`; new MCP tool
  schema. The current `apply_edits` is small enough that adding a
  variant rather than abstracting is the right move.
- **Add an MCP host** for `tilth install`. New `HostInfo` arm in
  `install::resolve_host`; choose the writer (`write_json_config` or
  `write_toml_config`) that matches the host's config format.

## Open architectural threads

Material gaps the review surfaced. None are blocking; all are worth
tracking.

- **Canonical-path symlink dedup.** Upstream's
  `feat: follow symlinks in all file walkers (#46)` set `follow_links(true)`
  on every `WalkBuilder` to make monorepos / symlinked-dependency
  layouts work. The `ignore` crate's cycle detection only catches
  loops, not duplicate visits to the same inode — so a file reachable
  via two paths (e.g. `.agents/RECENT.md` and `.claude/RECENT.md`
  through a `.claude → .agents/` symlink) matches twice and the
  post-search dedup misses (the *paths* differ). Workspace fix in
  this repo: `.claude/` in `.tilthignore`. A more general fix would
  be a canonical-path dedup at the `Walk` consumer or at
  `format_search_result`. Worth a small upstream PR once thought
  through; today it's a known workaround.

- **`SymbolIndex` allocated but unused.** Both `lib.rs` and `mcp.rs`
  build a `SymbolIndex`, thread it through to
  `search::search_symbol_expanded`, and that function explicitly
  discards it (`let _ = index;`). The build path is finished and
  parallel; the lookup path was never wired in. Either delete the
  inert plumbing (clean) or finish the wiring (right answer if the
  performance is meaningful — it would shift the per-query cost from
  O(files) to O(matches), and a symbol-heavy MCP session can easily
  walk 5k+ files per call today). This is the single largest design
  cleanup available short of a full async rewrite.

- **Fence-tracking duplication.** Two markdown ATX scanners maintain
  their own pre-pass loops that toggle an `in_fence` flag on
  ```` ``` ```` / `~~~` delimiters: `read/outline/markdown.rs:10-32`
  (outline path) and `search/symbol.rs:603-660` (search path). They
  share the `parse_atx_heading` helper but the fence loops are
  copy-pasted. The MEMORY.md note says "two copies stay below the
  abstraction threshold; introduce a shared helper only if a third
  caller appears." That threshold is roughly correct — the cost of
  the abstraction is one tiny shared module — but the next time
  fence-state has to handle a new edge case (indented fences, info
  strings, lazy continuations) the duplication will bite.

- **MCP per-request thread model.** Every `tools/call` spawns a fresh
  thread; on timeout the thread is abandoned. This is correct given
  Rust's lack of safe thread cancellation, but it means a
  pathological query (regex on a giant log file, `tilth_deps` on a
  symbol with thousands of usages) leaks until process exit. The
  abandoned-thread counter exists but only warns at 3; nothing
  aborts the host process when leakage is excessive. Switching to
  `tokio` with cooperative cancellation points inside the search
  loop would fix this but is an invasive rewrite. Bounding the
  walker (max files visited per call) is the lower-cost option and
  would also prevent the underlying problem.

- **No `OutlineCache` eviction.** Long-running MCP processes
  accumulate cache entries indefinitely (one per `(path, mtime)`
  pair touched). For practical workloads the working set stays
  small enough that this hasn't surfaced, but a session that visits
  thousands of distinct files would grow the cache without bound.
  An LRU layer or an mtime-staleness sweeper at request boundaries
  would be a small, targeted fix.

- **`tilth_map` disabled at the MCP boundary but kept in CLI.** The
  MCP dispatch arm explicitly returns "use tilth_search instead,"
  which suggests either the CLI should stop advertising it as a
  feature too, or the MCP boundary should re-enable it once the
  shape is right. Today the asymmetry leaves dead code in
  `dispatch_tool` and confused users at the CLI level.
