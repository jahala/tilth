# Post-edit tree-sitter parse check

## Problem

`tilth_edit` writes whatever the model asks for. If the edit produces a syntactically broken file (unbalanced braces, missing semicolons, unterminated strings), tilth says nothing — the model finds out later when it runs `cargo check` / `tsc` / etc.

Adding a tree-sitter parse pass after the write lets us surface "your edit broke the syntax at line N" in the same response, using grammars tilth already ships.

## Scope

In:
- Parse the post-edit file with the same grammar `outline_language()` returns.
- Diff against a pre-edit parse so pre-existing errors stay silent — we only flag what this edit introduced.
- Report new `ERROR` and `MISSING` nodes as a `── parse ──` block in `tilth_edit` output.

Out:
- Type checking. No `cargo check`, no `tsc --noEmit`.
- Linting / formatting.
- JSON/YAML/TOML structural validation (different code path; out of scope for v1).
- Per-edit attribution (which edit in a batch caused which error). v2 candidate.
- Rollback on broken parse. Edits always apply; we only warn.

## Design

### Hook point

`src/edit.rs::apply_edits`, between Phase 3 (`fs::write`) and Phase 4 (build diff/context). Both `content` (pre-edit) and `output` (post-edit) are already in scope at that point.

### Algorithm

```text
lang     = detect_file_type(path)
grammar  = outline_language(lang)        // Option<Language>
if grammar is None:    return None       // no grammar, skip silently
pre      = parse(content, grammar)
post     = parse(output,  grammar)
new_errs = multiset_subtract(pre, post)  // key = (ErrorKind, detail)
if new_errs is empty:  return None
return ParseReport { new_errs, total_post: post.len() }
```

`errors(tree)` walks the tree and collects nodes where `is_error()` or `is_missing()` is true. The multiset key is `(ErrorKind, detail)` — `ErrorKind` is the local `Error|Missing` enum and `detail` is the trimmed/truncated error-node text (or, for `MISSING`, the expected node kind). Positions are intentionally *not* part of the key: an edit that shifts lines must not surface pre-existing errors below the edit site as "new".

A node is reported as:
- `ERROR` if `is_error()` is true
- `MISSING` if `is_missing()` is true (these come with `kind()` describing what's missing, e.g. `;`)

### Output format

Append to `EditResult::Applied` before the blast radius block:

```text
── parse ──
:42 ERROR unexpected '}'
:55 MISSING ';'
```

Format details:
- One line per error: `:<line> <ERROR|MISSING> <detail>` (or `:<line> <ERROR|MISSING>` when `detail` is empty).
- `<detail>` for `ERROR`: the trimmed text of the error node, truncated to 40 chars with an ellipsis (`…`).
- `<detail>` for `MISSING`: the node `kind()` (which is what's expected, e.g. `;`, `)`, `expression`).
- Cap at **10 listed errors**. If more, append `... and N more (X new, Y total)` where `X` is the new-error count and `Y` is the total post-edit error count.
- Omit the block entirely if no new errors.

### Failure semantics

The write has already happened. The parse report is informational — it does not change `EditResult` discriminant or the file on disk.

Rationale: multi-file edits can have legitimate intermediate broken states (rename across files: first file references new name not yet introduced in second file). Rollback would also hide what the model attempted, making debugging harder. Hash anchors make re-editing cheap.

### Open-by-default

No flag, no env var, no per-call parameter for v1. The cost is sub-millisecond and the value is high. Re-evaluate after benchmarks if cost-per-correct-answer regresses.

### Bloom cache

Untouched. Parse check is read-only on the just-written file. `apply_batch` handles cache invalidation upstream of this.

## Implementation notes

### New module

`src/edit_parse_check.rs` — sibling of `src/edit.rs`, not a child. Keeping `edit.rs` flat (no `edit/mod.rs` promotion) avoided unnecessary churn since the new logic stands alone.

Public surface:

```rust
pub struct ParseReport {
    pub new_errors: Vec<ParseError>,
    pub total_post: usize,
}

pub struct ParseError {
    pub line: usize,        // 1-indexed
    pub col: usize,         // 0-indexed (internal sort key, never rendered)
    pub kind: ErrorKind,    // Error | Missing
    pub detail: String,     // <= 40 chars
}

pub fn check(path: &Path, before: &str, after: &str) -> Option<ParseReport>;
pub fn format_report(report: &ParseReport) -> String;
```

`check` returns `None` when:
- Language has no tree-sitter grammar (`outline_language` returns `None`)
- No new errors introduced

### Wire into `apply_edits`

In `EditResult::Applied`, add an optional parse block. Threaded through to `render_applied` and inserted into the final output between the context block and the blast radius.

```rust
EditResult::Applied {
    diff: String,
    context: String,
    parse: Option<String>,    // new — formatted ── parse ── block or None
}
```

### Diffing errors

Pre- and post-parse error lists are sorted by `(line, col)` for stable output.
Diff is a multiset subtraction keyed by `(ErrorKind, detail)` — no positions:
count occurrences in `pre`, walk `post` in order, skip the first N matches per
key, and collect the rest as new. This stays robust when an edit shifts line
numbers; a positional key would false-flag pre-existing errors below the edit
site.

If pre has errors the edit *fixed* (rare but possible), do not surface them in v1 — keeps output focused on regressions.

### Acceptance criteria

1. **Clean edit, clean file** → no `── parse ──` block. (Most common case.)
2. **Clean edit, pre-broken file** → no `── parse ──` block (pre-existing errors silent).
3. **Edit introduces syntax error** → `── parse ──` block with the new error, line-anchored.
4. **Edit introduces 15 syntax errors** → top 10 listed, `... and 5 more (15 new, 15 total)`.
5. **File in a language without a tree-sitter grammar (Dockerfile, Make)** → no `── parse ──` block, no crash.
6. **Multi-file batch where one file goes broken, others stay clean** → only the broken file's section shows `── parse ──`.
7. **Edit that fixes a pre-existing error** → no `── parse ──` block (v1 behavior — we don't report fixes).

### Test plan

In-source `#[cfg(test)] mod tests` in `src/edit_parse_check.rs`:

- `clean_edit_returns_none` — edit a Rust file, valid before, valid after.
- `introduced_error_reported` — edit removes a closing brace.
- `preexisting_error_silent` — file starts broken, edit unrelated lines, no report.
- `missing_node_reported` — edit removes a semicolon in Java/C.
- `error_cap_at_ten` — edit produces 15 errors, output truncated with count.
- `no_grammar_returns_none` — synthetic `.dockerfile` edit, no panic, no report.
- `error_detail_truncation` — error node with 200-char text gets cut to 40.
- `error_sort_order_stable` — multiple errors return in `(line, col)` order regardless of tree walk order.

Plus one integration test in `src/edit.rs` tests module:

- `apply_edits_includes_parse_block_on_break` — full pipeline, verify the returned `Applied.parse` is `Some(_)` and contains expected line numbers.

## Effort

~150 LOC + tests. One PR. Grammar plumbing already in place; this is wiring + a diff + a formatter.

## Out of scope (v2 candidates)

- Per-edit attribution: map each error to the edit range that contains it.
- Report fixes (errors that disappeared) alongside regressions.
- Structured parse for non-tree-sitter languages (JSON via `serde_json`, YAML via `serde_yaml`, TOML via `toml`).
- Opt-out flag (only if benchmarks justify it).
