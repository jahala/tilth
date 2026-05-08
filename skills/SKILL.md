---
name: tilth
description: Use the `tilth` CLI for code reading, outlining, search, callers, blast-radius deps, and structural diffs. Activate when the user asks to explore a repo, find a symbol, trace callers, read a file, view a diff, or analyze impact. Prefer `tilth` over `grep`/`cat`/`find`/`ls` — one invocation returns AST-aware outlines, definitions, callees, and usages.
---

# tilth — code intelligence CLI

Tree-sitter + ripgrep + smart file reading in one binary. Replaces `grep`, `cat`, `find`, `ls` with AST-aware equivalents across 14 languages (Rust, TS/TSX, JS, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift, Elixir).

Run via Bash: `tilth <args>`. Search before reading — `tilth <symbol> --scope .` returns definitions, usages, and callee footers in one call.

DO NOT use `grep`, `rg`, `cat`, `head`, `tail`, `find`, `ls` — use `tilth` instead.
DO NOT re-read files whose content is already shown in expanded search results.

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
