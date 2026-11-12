---
name: tilth
description: Use when exploring a codebase, finding where a symbol is defined or used, tracing its callers or callees, reading a source file as an outline, checking what depends on a file before changing it, or reviewing a diff by function. Covers 17 languages.
---

# tilth — code intelligence CLI

Tree-sitter + ripgrep + smart file reading in one binary. Replaces `grep`, `cat`, `find`, `ls` with AST-aware equivalents across 17 languages (Rust, TypeScript, TSX, JavaScript, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift, Kotlin, Elixir, Bash).

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

The shape of the query picks the mode:

| Query | Mode | Example |
|---|---|---|
| One identifier | Symbol search: definitions, then usages | `tilth WeekService` |
| Several words | Literal text search | `tilth "study start date"` |
| Contains `*` | File listing | `tilth "*.swift"` |
| `/pattern/` or `/pattern/flags` | Regex search | `tilth "/week/i"` |
| An existing file path | Read the file | `tilth src/main.rs` |

Symbol search matches the identifier exactly, including case.

```bash
tilth <symbol> --scope <dir>                # definitions + usages
tilth "Foo,Bar,Baz" --scope <dir>           # multi-symbol (max 5)
tilth <symbol> --expand                     # inline source for top 2 matches
tilth <symbol> --expand=5                   # inline source for top 5
tilth <symbol> --full                       # up to 100 matches, source for the top 50
tilth <symbol> --full --expand=0            # up to 100 matches, no inline source
tilth <symbol> --callers --scope <dir>      # call sites (structural, not text)
tilth "TODO: fix" --scope <dir>             # content search (literal text)
tilth "/regex/" --scope <dir>               # regex search
tilth <symbol> --glob "*.rs" --scope <dir>  # file pattern filter
```

`--full` semantics depend on query type:
- File path → return whole file (bypass smart-view outline).
- Symbol / text / regex → raise the match cap from 10 to 100 and inline source for the top 50 matches. `--expand=N` sets the inline-source count only, so `--full --expand=0` still lists up to 100 matches. Multi-symbol (`"Foo,Bar"`) is the exception: it always expands at least one match per symbol.
- Glob → no-op.

Symbol search also surfaces **markdown headings as soft definitions** — `tilth StreamingResponse --scope docs/` finds `## StreamingResponse` headings ranked between code defs (60-80) and usages (0). Section body inlines automatically in the default preview (capped at 40 lines; pass `--expand` for the rest).

Output per match:
```
## <path>:<start>-<end> [definition|usage|impl]
<outline context>
<expanded source block>
-- calls --
  <callee>  <path>:<start>-<end>  <signature>
-- siblings --
  <related>  <path>:<start>-<end>  <signature>
```

`--callers` finds direct, by-name call sites. If it returns 0 matches but the symbol exists, the call is likely indirect (trait/interface dispatch, reflection, route registration, callback) — fall back to `tilth <symbol> --scope .` to see references.

## Files

```bash
tilth "*.test.ts" --scope <dir>   # glob files (.tilthignore honored)
tilth --respect-gitignore <symbol> --scope <dir>  # opt into .gitignore/.ignore
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
tilth diff --expand 3             # changed lines of the first 3 changed symbols
```

Function-level change detection — `[+]` added, `[-]` removed, `[~]` modified, `[~:sig]` signature changed. Replaces `git diff` for symbol-level review.

## Over MCP

When tilth runs as an MCP server (`tilth --mcp`), the tools are `tilth_search`, `tilth_read`, `tilth_list`, `tilth_deps`, `tilth_grok`, `tilth_diff`, and `tilth_write` in edit mode. Each tool's description carries its parameters. The rules below are the ones that are easy to get wrong.

Paths and scope:

- The server cannot see your shell's working directory. Pass absolute paths, or set `root` to the absolute project root whenever a `path` or `scope` is relative.
- `scope` is an existing directory, never a file. To search one file, set `scope` to its directory and `glob` to its name, or call `tilth_read` if you already know the file.
- Omitting `scope` searches `root` when it is set, and otherwise the directory the server was started in. In a worktree, set `root` so the search runs in the right checkout.
- `context` names the file you are working on. It boosts nearby results and does not narrow the search.

Search:

- `kind: "symbol"` (the default) for identifiers, `"content"` for literal text such as error messages or config keys, `"regex"` only when you need a pattern, and `"callers"` for call sites.
- Do not mix kinds in one query. Search a class name as a symbol and `__all__` as content, in two calls.
- Comma-separated queries are for up to five symbol names in one symbol search.
- `glob` narrows by file: `"**/*.py"`, `"models.py"`, or `"!**/*_test.py"` to exclude.
- Use `tilth_list` only to list files when there is no symbol or text to search for.
- Do not re-read source already shown in an expanded search result.

Other tools:

- `tilth_grok` when the task is to understand one symbol: it returns its definition, callers, callees, siblings and tests together.
- `tilth_deps` before changing a signature, renaming or removing an export, or changing behaviour that callers rely on.
- `tilth_write` replaces the host's Edit and Write tools. Group writes to several files into one call. `mode: "hash"` takes `edits`, each with a `start` anchor from `tilth_read`, an optional `end`, and `content`. `"overwrite"` and `"append"` take `content`. A multi-file call can partly succeed, so read each file's result.

## Budget

```bash
tilth <args> --budget 2000        # cap response at ~N tokens
```

Use when an outline or search returns more than you need.
