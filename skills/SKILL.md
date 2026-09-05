---
name: tilth
description: Use tilth to read, search, and navigate code structurally (outlines, definitions, callers, deps, diff) instead of grep, cat, find, ls, or git diff.
---

# tilth — code intelligence for agents

Tree-sitter + ripgrep + smart file reading in one binary, as a CLI (`tilth …`) and as an MCP server (`tilth --mcp`, tools named `tilth_*`). Replaces `grep`, `cat`, `find`, `ls`, and `git diff` with AST-aware equivalents across 14 languages (Rust, TS/TSX, JS, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift, Elixir). Search before reading — one call returns definitions, usages, and callee footers.

DO NOT use `grep`, `rg`, `cat`, `head`, `tail`, `find`, `ls` — use `tilth` instead.
DO NOT re-read files whose content is already shown in expanded search results.

## As an MCP server

When tilth is connected as an MCP server the tools are `tilth_search`, `tilth_read`, `tilth_list`, `tilth_deps`, `tilth_grok`, `tilth_diff`, and, in edit mode, `tilth_write`. The server's own `instructions` block is a short pointer; this is the guide it points at.

tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

PATHS: DO NOT pass a relative path or scope without also setting root (absolute) — the server cannot see your shell cwd, so bare relative paths are refused. Absolute paths always work; omitting path/scope searches the project the server was launched in. DO NOT pass a file as scope — scope is a directory; to search one file, set glob to that file's path.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(query: "handleRequest").
tilth_list is ONLY for listing directory contents when you have no symbol or text to search for.
DO NOT use Read if content is already shown in expanded search results.
DO NOT use Grep, Read, or Glob. Always use the better tools tilth_search (grep), tilth_read (read), tilth_list (glob).

Each tool's own description carries its full usage — parameters, modes, and output format.

To search code, use tilth_search instead of Grep or Bash(grep/rg).
To read files, use tilth_read instead of Read or Bash(cat).
To find files, use tilth_list instead of Glob or Bash(find/ls).
To check what changed, use tilth_diff instead of Bash(git diff/git log).
DO NOT use Bash(git diff) or Bash(git log --patch). Use tilth_diff instead.
DO NOT re-read files already shown in expanded search results.

### Edit mode (`tilth --mcp --edit`)


tilth_write replaces the host Edit and Write tools. Its full usage — hash/overwrite/append modes, hash anchors from tilth_read, grouping multi-file writes into one call, per-file partial success — is in the tilth_write tool description.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.

## As a CLI

Run via Bash: `tilth <args>`. Search before reading — `tilth <symbol> --scope .` returns definitions, usages, and callee footers in one call.

## Read

```bash
tilth <path>                      # smart view: full if small, outline if large
tilth <path> --section 45-89      # exact line range
tilth <path> --section "## Foo"   # markdown heading (suggests fuzzy matches on miss)
tilth <path> --full               # force full content (file paths)
```

Outline format: `[<start>-<end>]  <symbol>`. Full/section format: `<line> │ <content>`. Binary files print `[skipped]`; lockfiles, minified bundles, generated code print `[generated]`.

## Search

```bash
tilth <symbol> --scope <dir>                # definitions + usages
tilth "Foo,Bar,Baz" --scope <dir>           # multi-symbol (max 5)
tilth <symbol> --expand                     # inline source for top 2 matches
tilth <symbol> --expand=5                   # inline source for top 5
tilth <symbol> --full                       # expand every match (capped at 50)
tilth <symbol> --callers --scope <dir>      # call sites (structural, not text)
tilth "TODO: fix" --scope <dir>             # content search (literal text)
tilth "/regex/" --scope <dir>               # regex search
tilth <symbol> --glob "*.rs" --scope <dir>  # file pattern filter
```

`--full` semantics depend on query type:
- File path → return whole file (bypass smart-view outline).
- Symbol / text / regex → expand every match (capped at 50). Explicit `--expand=N` wins.
- Glob → no-op.

Symbol search also surfaces **markdown headings as soft definitions** — `tilth StreamingResponse --scope docs/` finds `## StreamingResponse` headings ranked between code defs (60-80) and usages (0). Section body inlines automatically in the default preview (capped at 40 lines; pass `--expand` for the rest).

Output per match:
```
## <path>:<start>-<end> [definition|usage|impl]
<outline context>
<expanded source block>
── calls ──
  <callee>  <path>:<start>-<end>  <signature>
── siblings ──
  <related>  <path>:<start>-<end>  <signature>
```

`--callers` finds direct, by-name call sites. If it returns 0 matches but the symbol exists, the call is likely indirect (trait/interface dispatch, reflection, route registration, callback) — fall back to `tilth <symbol> --scope .` to see references.

## Files

```bash
tilth "*.test.ts" --scope <dir>   # glob (respects .gitignore)
tilth --map --scope <dir>         # codebase skeleton with directory token rollups
```

## Deps (blast radius)

```bash
tilth <file> --deps               # what it imports + what depends on it
```

Use only before renaming, removing, or changing an export's signature.

## Diff (structural)

```bash
tilth diff                        # uncommitted changes
tilth diff HEAD~1                 # vs prior commit
tilth diff main..feat             # branch comparison
tilth diff --log HEAD~5..HEAD     # per-commit symbol summaries
tilth diff --blast                # warn on signature-changed exports
tilth diff --expand 3             # inline source for top 3 changed symbols
```

Function-level change detection — `[+]` added, `[-]` removed, `[~]` modified, `[~:sig]` signature changed. Replaces `git diff` for symbol-level review.

## Budget

```bash
tilth <args> --budget 2000        # cap response at ~N tokens
```

Use when an outline or search returns more than you need.
