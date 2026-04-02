# tilth_diff — Design Document

## The problem

Agent does `git diff` on a 50-file PR → gets 3000+ lines of raw unified diff → burns 10k+ tokens reading it → still can't answer "what actually changed?" without multiple follow-up reads and searches.

## What tilth_diff does

One tool, progressive disclosure. Same pattern as tilth_read (full vs outline vs section) and tilth_search (overview → drill → expand).

```
tilth_diff()                                    # overview: file list + function-level markers
tilth_diff(scope: "auth.rs")                    # one file: all changes with structural context
tilth_diff(scope: "auth.rs:handleAuth")         # one function: before/after with line detail
tilth_diff(search: "error")                     # search within changed code only
tilth_diff(blast: true)                         # overview + downstream impact per symbol
tilth_diff(patch: "fix.patch")                  # parse a patch file
tilth_diff(a: "old.rs", b: "new.rs")            # file-to-file comparison
tilth_diff(log: "HEAD~5..HEAD")                 # per-commit structural summaries
tilth_diff(log: "main..HEAD", scope: "auth.rs") # history of one file on this branch
```

## Token budget comparison

| Approach                          | Tokens  | Round-trips |
|-----------------------------------|---------|-------------|
| `git diff` raw                    | 10,000+ | 1 (agent confused) |
| `git diff` + re-reads + searches  | 15,000+ | 5-8 |
| tilth_diff overview + drill       | 500-800 | 2 |

## MCP tool definition

```json
{
  "name": "tilth_diff",
  "description": "Structural diff — shows what changed at the function/class level. Replaces git diff for understanding changes.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "source": {
        "type": "string",
        "description": "Git diff source. uncommitted (default), staged, <commit>, <ref>..<ref>"
      },
      "a": { "type": "string", "description": "First file path (file-to-file diff)" },
      "b": { "type": "string", "description": "Second file path (file-to-file diff)" },
      "patch": { "type": "string", "description": "Path to a .patch or .diff file" },
      "scope": {
        "type": "string",
        "description": "Filter to file or function. 'auth.rs' or 'auth.rs:handleAuth'"
      },
      "search": {
        "type": "string",
        "description": "Search within changed lines only."
      },
      "blast": {
        "type": "boolean",
        "description": "Include blast radius (callers affected by signature changes)."
      },
      "expand": {
        "type": "number",
        "default": 0,
        "description": "Number of changed functions to show full before/after source."
      },
      "log": {
        "type": "string",
        "description": "Per-commit structural summaries. Commit range e.g. 'HEAD~5..HEAD', 'main..HEAD'."
      }
    }
  }
}
```

## Output formats

### Overview (default, no scope)

```
# Diff: uncommitted — 3 files, 2 functions modified, 1 added (~350 tokens)

## src/auth.rs (modified, 3 symbols)
  [~:sig]  fn handleAuth(req: Request) → (req: Request, ctx: Context)        L42
  [~]      fn validate_session                                               L88  (body, 44→51 lines)
  [+]      fn refresh_token(session: &Session) -> Result<Token>              L120 (new, 18 lines)

## src/routes/api.ts (modified, 1 symbol)
  [~:sig]  fn register_routes(app) → (app, opts)                             L15
  ⚠ 12 callers affected

## package-lock.json (generated, 482 lines changed — summarized)
```

Markers: `[+]` added, `[-]` deleted, `[~]` body changed, `[~:sig]` signature changed, `[→]` moved to/from another file, `[ ]` unchanged (shown for context).

Cross-file alerts: when the same symbol has `[~:sig]` in multiple files, a footer warns:
```
⚠ handleAuth signature changed in 3 files — callers likely need updates
```

### File detail (scope: "src/auth.rs")

```
# Diff: src/auth.rs — 3 symbols touched, +22/-8 lines

## fn handleAuth — signature changed (L42-93)
  BEFORE: fn handleAuth(req: Request) -> Response
  AFTER:  fn handleAuth(req: Request, ctx: Context) -> Response

  +48│ ctx.verify_permissions(req.user_id)?;
  +49│ let claims = ctx.extract_claims()?;
  ~67│ return Ok(response.with_context(claims))

## fn validate_session — body changed (L88-132, +7 lines)
  +95│ if token.is_expired() {
  +96│     return Err(SessionError::Expired);
  +97│ }

## fn refresh_token — new (L120-138, 18 lines)
  pub fn refresh_token(session: &Session) -> Result<Token> {
      let old = session.token()?;
      ...
  }
```

### Function detail (scope: "auth.rs:handleAuth", expand: 1)

Full before/after source of that one function, with line-level +/- markers. Same as `git diff` but scoped to one function and with structural header.

### Merge conflict view

When conflicts are detected (`<<<<<<<` markers in working tree files):

```
# Conflicts: 2 in src/auth.rs

## fn handleAuth (L42-48)
  OURS:
    ctx.verify(req.user_id)?;
  THEIRS:
    ctx.verify_all(req)?;

## fn validate_session (L95-103)
  OURS:
    if token.expired() { return Err(SessionError::Expired) }
  THEIRS:
    token.refresh_or_fail()?;
```

Conflict detection: scan working tree files for `<<<<<<<` markers. Extract both sides. Map each conflict's line range to the enclosing function via outline. Show both sides with structural context.

### Log mode (log: "HEAD~5..HEAD")

Per-commit structural summaries. Agent dropped into a codebase cold can understand what happened recently in one call.

```
# Log: HEAD~5..HEAD — 5 commits, 12 files, 23 functions touched

## abc1234 — "refactor auth to use context" (2h ago, @alice)
  src/auth.rs:      [~:sig] handleAuth, [+] refresh_token
  src/routes.rs:    [~] register_routes

## def5678 — "fix session timeout" (5h ago, @bob)
  src/auth.rs:      [~] validate_session

## 9876543 — "add rate limiter middleware" (1d ago, @alice)
  src/middleware.rs: [+] rate_limit, [+] RateLimitConfig
  src/routes.rs:    [~] register_routes
```

Scoped log: `tilth_diff(log: "main..HEAD", scope: "auth.rs")` filters to commits that touched that file. Shows only the relevant symbols per commit.

Implementation: `git log --format="%H %at %s %an" <range>`, then for each commit call the diff pipeline with `source: "<commit>^..<commit>"`. Compact format — one line per file, symbols comma-separated. ~30 lines.

## Data structures

```rust
// src/diff/mod.rs

/// What to diff.
pub enum DiffSource {
    GitUncommitted,
    GitStaged,
    GitRef(String),             // "abc123" or "main..HEAD"
    Files(PathBuf, PathBuf),    // file-to-file comparison
    Patch(PathBuf),             // .patch or .diff file
    Log(String),                // "HEAD~5..HEAD" — per-commit summaries
}

/// One file's changes.
pub struct FileDiff {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,  // for renames
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    pub is_generated: bool,
    pub is_binary: bool,
}

pub enum FileStatus { Added, Modified, Deleted, Renamed }

pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

pub enum DiffLineKind { Context, Added, Removed }

pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

/// Structural overlay — what changed at the function/class level.
pub struct SymbolChange {
    pub name: String,
    pub kind: OutlineKind,
    pub change: ChangeType,
    pub line: u32,              // line in new file (or old for deletions)
    pub old_sig: Option<String>,
    pub new_sig: Option<String>,
    pub size_delta: Option<(u32, u32)>,  // (old_lines, new_lines)
}

pub enum ChangeType {
    Added,
    Deleted,
    BodyChanged,
    SignatureChanged,
    Moved(PathBuf),             // moved to/from this file
}

/// A merge conflict region within a file.
pub struct Conflict {
    pub line: u32,
    pub ours: String,
    pub theirs: String,
    pub enclosing_fn: Option<String>,  // function name from outline
}
```

## Pipeline

```
1. Resolve source
   GitUncommitted → `git diff --no-color -U3`
   GitStaged      → `git diff --cached --no-color -U3`
   GitRef(r)      → `git diff --no-color -U3 {r}` (or `{r}^..{r}` for single commit)
   Files(a, b)    → `diff -u a b`
   Patch(p)       → read file, parse as unified diff

2. Parse unified diff → Vec<FileDiff>
   Custom parser (~120 lines). Handles: ---, +++, @@ headers, +/-/space lines.
   Detects: binary files, renames (diff --find-renames output).
   Flags: is_generated via is_generated_by_name().

3. Structural overlay (per code file)
   For each modified code file:
     a. Get old content: `git show HEAD:path` (or `git show ref^:path`)
     b. Get new content: read working tree (or `git show ref:path`)
     c. old_outline = walk_top_level(old_content)
     d. new_outline = walk_top_level(new_content)
     e. Match by name:
        - name in new but not old → Added
        - name in old but not new → Deleted
        - name in both, signatures differ → SignatureChanged
        - name in both, any hunk overlaps line range → BodyChanged
        - name in both, no hunk overlap → Unchanged (show for context)
     f. For SignatureChanged: store old_sig and new_sig for display

4. Hunk-to-function attribution
   Slice hunks by function boundaries. Each function gets only
   the diff lines that fall within its line range.
   Show the actual +/- lines grouped by function in file detail mode.

5. Cross-file move detection
   After computing all overlays: collect all Deleted symbols and all Added symbols
   across all files. If same name appears in both sets → replace the [-] and [+]
   entries with [→ target_file] and [→ source_file] respectively.
   Pure name matching (~15 lines). No body comparison.

6. Cross-file signature change grouping
   After building overview: scan for symbol names with [~:sig] in multiple files.
   Emit footer: "⚠ handleAuth signature changed in 3 files — callers likely need updates"
   HashMap<name, count> pass over formatted entries (~10 lines).

7. Format output
   Overview: file list + symbol markers. Reuse format::search_header pattern.
   File detail: per-symbol changes with attributed diff lines.
   Function detail: full before/after with +/- markers.
   Budget enforcement via budget::apply().

8. Blast radius (when blast: true)
   For SignatureChanged symbols, call blast_radius logic.
   Show ⚠ warning with caller count.

9. Search within diff (when search is set)
   Filter DiffLine entries to those matching search term.
   Show structural context (which function contains the match).

10. Conflict detection (when working tree has conflict markers)
    Scan files for <<<<<<< markers.
    Extract ours/theirs blocks.
    Map each conflict to enclosing function via outline.
    Show structured conflict view.

11. Log mode (when log is set)
    `git log --format="%H %at %s %an" <range>` → list of commits.
    For each commit: run steps 1-6 with source = "<commit>^..<commit>".
    Compact format: one line per file, symbols comma-separated.
    If scope is set, filter to commits that touched the scoped file/function.
```

## What we reuse (no duplication)

| Existing code | Used for |
|---|---|
| `walk_top_level()` | Outline both old and new versions |
| `outline_language()` | Get tree-sitter grammar per language |
| `detect_file_type()` | Determine if file is code (has grammar) |
| `is_generated_by_name()` | Skip lockfiles/generated files |
| `OutlineCache` | Cache outlines for old and new |
| `format::file_header()` | Consistent header format (adapt for diff) |
| `format::number_lines()` | Line-numbered excerpts in drill-down |
| `rel()` | Relative paths in output |
| `estimate_tokens()` | Token count in header |
| `budget::apply()` | Truncate large output |
| `blast_radius()` / `touched_symbols()` | Downstream impact (adapt: takes Edit, we have Hunk) |
| `is_test_file()` | Separate test changes in output |

## What we add

| New code | Size estimate | Purpose |
|---|---|---|
| `src/diff/mod.rs` | ~120 lines | Types, source resolution, orchestration, log mode loop |
| `src/diff/parse.rs` | ~150 lines | Unified diff parser + conflict marker parser |
| `src/diff/overlay.rs` | ~140 lines | Map hunks to outline entries, signature comparison, move detection, cross-file grouping |
| `src/diff/format.rs` | ~230 lines | Overview, file detail, function detail, conflict, log formatters |
| MCP wiring in `mcp.rs` | ~50 lines | Tool dispatch |
| CLI wiring in `main.rs` | ~20 lines | --diff flag |
| Tests | ~170 lines | Parser tests, overlay tests, move detection tests |
| Total | ~880 lines | |

## Dependencies

**None new.** Git is universally available. Unified diff format is simple enough to parse ourselves. File-to-file diffs use `diff -u` (POSIX, always available).

## Edge cases

| Case | Handling |
|---|---|
| Binary file | `(binary, {size})` — no structural analysis |
| Generated/lockfile | `(generated, N lines changed — summarized)` |
| Non-code file (md, json) | Use existing structured outline. No AST overlay. |
| Tree-sitter parse failure | Fall back to hunk-level summary (no function markers) |
| Renamed file | Show old→new path, compare outlines if same language |
| New file (all added) | All outline entries marked `[+]` |
| Deleted file (all removed) | All outline entries marked `[-]` |
| Very large diff (>100 files) | Budget truncation. Show top files by change size. |
| Merge conflicts | Structured conflict view with function context |
| Empty diff | "No changes." |
| Not a git repo | Error unless file-to-file or patch mode |
| Dirty submodules | Skip (show as "submodule changed") |
| Function moved (same name) | Detected as `[→]` via cross-file name matching. Renamed functions still show as `[-]` + `[+]` (no similarity comparison). |
| Hunk spans multiple functions | Split hunk at function boundaries, attribute lines to each |

## Feature decisions

### DOING — Phase 1 (core)

| Feature | Why |
|---|---|
| **Unified diff parser** | Foundation. ~120 lines, no deps. |
| **Structural overlay** (outline mapping) | Core value. Maps line changes to functions. Reuses walk_top_level. |
| **Overview format** (file list + `[+]/[~]/[-]/[~:sig]` markers) | The 200-token answer to "what changed?" |
| **File detail format** (per-function changes with diff lines) | Drill-down without re-reading the file. |
| **Hunk-to-function attribution** | Show actual +/- lines grouped by function, not raw hunks. |
| **Signature diffing** | Show BEFORE/AFTER signatures for `[~:sig]`. Agent needs this to update callers. |
| **Git sources: uncommitted, staged, ref** | The three daily-use modes. |
| **File-to-file mode** (`a`, `b` params) | Simple, `diff -u` does the work. Same pipeline after. |
| **Patch file input** (`patch` param) | rnett explicitly requested. Read file, feed to same parser. |
| **Scope: file-level** (`scope: "auth.rs"`) | Filter to one file. Trivial — just filter Vec<FileDiff>. |
| **Generated/lockfile detection** | Reuse is_generated_by_name(). Show "(generated, N lines)" instead of structural analysis. |
| **Binary file detection** | Skip structural analysis. Show "(binary, {size})". |
| **Budget enforcement** | Reuse budget::apply(). Prevent token explosion on huge diffs. |
| **Unchanged context symbols** | Show `[ ]` markers for unchanged functions in file detail. Agent sees what's around the changes. |
| **Function move detection** (cross-file, by name) | ~15 lines. Collect deleted+added names across files, match → `[→]` marker. Prevents agent confusion on refactors. |
| **Cross-file signature change grouping** | ~10 lines. HashMap counter over `[~:sig]` names. Footer warns "handleAuth changed in 3 files". Prevents agent from missing coordinated changes. |
| **MCP tool + CLI flag** | Wire it in. |

### DOING — Phase 2 (intelligence)

| Feature | Why |
|---|---|
| **Blast radius integration** | For `[~:sig]` changes, show caller count and locations. Reuse existing blast_radius() logic. Agent knows what else to update. |
| **Search within diff** (`search` param) | Filter diff lines to matches. Show which function they're in. Agent finds specific changes without reading everything. |
| **Merge conflict detection + view** | rnett explicitly requested. Scan for `<<<<<<<`, extract both sides, map to enclosing function. High value — conflict resolution is extremely token-expensive without this. |
| **Scope: function-level** (`scope: "auth.rs:handleAuth"`) | Drill into one function. Full before/after with +/- markers. |
| **Expand parameter** | Inline full source for top N changed functions. Same pattern as tilth_search. |
| **Log mode** (`log` param) | Per-commit structural summaries. Agent dropped cold into a codebase can understand recent history in one call. ~30 lines — loop over `git log`, diff pipeline per commit, compact format. Pairs with `scope` for function-level history. |

### NOT DOING (and why)

| Feature | Why not |
|---|---|
| **AST-level diffing** (Dijkstra graph, difftastic-style) | Massive complexity (~2000 lines). Name-based outline matching covers 95% of cases. If a function was completely rewritten, showing "body changed" + the diff lines is sufficient. The agent doesn't need optimal edit distance. |
| **Lockfile summarization** (parse "express 4.18→4.19") | Per-format parsing code (package-lock.json, Cargo.lock, go.sum, yarn.lock, etc.). Tedious, fragile, low value. "(generated, N lines changed)" is sufficient. Agent can drill down if it cares. |
| **Semantic change classification** ("added error handling", "changed return type") | Requires per-language tree-sitter query patterns. Large surface area. The signature diff + body diff lines already convey this — the agent can read them. |
| **AST-aware hunk splitting** (split git hunks at AST boundaries) | Requires parsing + re-diffing at AST node level. The hunk-to-function attribution (slicing by line range) achieves 90% of this with 10% of the effort. |
| **Inline before/after full source** (expand with +/- markers spliced in) | Complex formatting — interleaving diff hunks with unchanged code. The expand parameter can show the full new source; combined with the diff lines in file detail mode, this is sufficient. |
| **Slider heuristics** (move hunks to visually intuitive positions) | Only relevant if we compute our own diffs. We use git's diff output which already has git's slider logic. |
| **imara-diff integration** (line-level diff engine) | Only needed for file-to-file mode, where `diff -u` works fine. Adding a dep for marginal gain. Reconsider if file-to-file becomes a major use case. |
| **git2/gix integration** (programmatic git access) | Adds C deps (git2) or large dep tree (gix). `process::Command` is simpler, git is always available, output format is stable. |
| **Stdin/pipe mode** (pipe arbitrary diff into tilth) | Niche. Patch file input covers the main use case. Stdin adds complexity (no file to reference for outlines). Reconsider if requested. |
| **git blame integration** | Completely different feature. Not a diff. |
| **Word-level diff within lines** (highlight specific changed tokens) | Adds visual noise in text output. Useful for humans in a terminal, not for agents reading text. |
| **Diff statistics** (insertions/deletions per file) | Already shown in overview header. Git's `--stat` output is available if agent wants raw numbers. |
| **Custom diff algorithm selection** (Myers vs Patience vs Histogram) | We use git's default. No reason to expose algorithm choice to agents. |
| **Submodule diffs** | Complex, rare. Show "(submodule changed)" and move on. |
| **Multi-way diff** (3-way merge base comparison) | Complex. Conflict view covers the practical need. |

## Implementation order

**Phase 1 — ship value fast (~650 lines)**
1. `src/diff/parse.rs` — unified diff parser + conflict marker parser
2. `src/diff/overlay.rs` — map hunks to outlines, signature comparison, hunk attribution, move detection, cross-file sig grouping
3. `src/diff/format.rs` — overview + file detail + conflict view formatters (incl. `[→]` marker, cross-file warnings)
4. `src/diff/mod.rs` — source resolution, orchestration, DiffSource enum
5. MCP wiring in `mcp.rs` — tool dispatch
6. CLI wiring in `main.rs` — --diff flag
7. Tests — parser, overlay, move detection, format

**Phase 2 — intelligence (~230 lines)**
1. Blast radius integration — adapt touched_symbols to work with hunks
2. Search within diff — filter + structural context
3. Scope: function-level drill-down
4. Expand parameter — inline full source for changed functions
5. Log mode — per-commit structural summaries, scoped log filtering
