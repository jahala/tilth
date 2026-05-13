tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

## Core Principles

ALWAYS BATCH: tilth_read takes `paths: [...]`, tilth_files takes `patterns: [...]`, tilth_edit takes `files: [...]`. Batch all known reads, globs, and edits into the first call; each extra call costs a turn. Even for one item, use a one-element array: `paths: ["a.rs"]`, `patterns: ["*.rs"]`. Never call the same batch-capable tool twice in a row when one call could include all items.

Search first: To explore code, always call tilth_search before reaching for other tools. It finds definitions, usages, and file locations in one call.

DO NOT use Grep, Read, or Glob. Use tilth_search (grep), tilth_read (read), tilth_files (glob) instead.
DO NOT use Bash(grep/rg/cat/head/tail/find/ls). Use the tilth tools.
DO NOT re-read files already shown in expanded search results.

## Tools

tilth_search: Code search — finds definitions, usages, and text. Replaces grep/rg.
  Usage: tilth_search(query: "handleRequest")
  For multi-symbol lookup, separate with comma: "symbol1,symbol2" (max 5)
  kind: "symbol" (default, declarations only — no comment/string hits) | "any" (declarations + usages + comment/string mentions) | "content" (literal text in strings/comments) | "regex" (regex pattern over file content) | "callers" (call sites of a symbol)
  expand: (default 2) inline full source for top matches
  context: path to file being edited — boosts nearby results
  glob: file pattern filter — "*.rs" (whitelist), "!*.test.ts" (exclude)
  Output per match:
    ## <path>:<start>-<end> [definition|usage|impl]
    <outline context>
    <expanded source block>
    ── calls ──
    <name>  <path>:<start>-<end>  <signature>
    ── siblings ──
    <name>  <path>:<start>-<end>  <signature>
  Re-expanding a previously shown definition returns [shown earlier].

tilth_read: File reading with smart outlining. Replaces cat/head/tail.
  Usage: tilth_read(paths: ["a.rs", "b.rs"]) — always an array, max 20 files in one call.
  Before calling, collect every file you already know you need; include them all.
  For a single file: tilth_read(paths: ["a.rs"]). The singular `path` form is NOT accepted.
  Small files return full content. Large files return structural outline.
  section: "<start>-<end>" or "<heading text>" — only valid with a single-element paths array
  sections: array of ranges/headings for multiple slices — only valid with a single-element paths array
  Output modes:
    Full/section: <line_number> │ <content>
    Outline: [<start>-<end>]  <symbol name>

tilth_files: File glob search. Replaces find, ls, pwd.
  Usage: tilth_files(patterns: ["*.rs", "*.toml"]) — always an array, max 20 globs in one call.
  Before calling, collect every glob you already know you need; include them all.
  For a single glob: tilth_files(patterns: ["*.rs"]). The singular `pattern` form is NOT accepted.
  Output: <path>  (~<token_count> tokens)

tilth_deps: Blast-radius check before signature changes.
  Shows what imports this file and what it imports.
  Use ONLY before renaming, removing, or changing an export's signature.

tilth_diff: Structural diff at function level. Replaces Bash(git diff/git log --patch).
  Usage: tilth_diff(source: "HEAD~1") for last commit, or no args for uncommitted changes
  scope: "file.rs" or "file.rs:fn_name" to limit to a specific function
  log: "HEAD~5..HEAD" for per-commit summaries
  search: filter to lines matching a term
  blast: true to show callers of changed function signatures
  Output: [+] added, [-] deleted, [~] body changed, [~:sig] signature changed
