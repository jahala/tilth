---
name: tilth
description: "Smart code navigation using tilth CLI for reading, outlining, searching, and drilling into codebases. Use this skill whenever you need to read source code files, find symbol definitions or usages, search for patterns in a codebase, outline large files, trace call chains, or navigate code structure. Activate when the user asks to explore, understand, navigate, search, or read code in any repository. Prefer tilth over raw grep/cat/read for code comprehension tasks — it gives structural awareness in one call."
---

# Tilth — Smart Code Reading CLI

tilth combines `ripgrep`, `tree-sitter`, and `cat` into a single tool that understands code structure. It reduces tool-call overhead by giving you outlines, definitions, and usages in one shot instead of the typical glob-read-grep-read-again cycle.

**Binary location:** `~/.cargo/bin/tilth` (already in PATH via `~/.bashrc`)

Run tilth via Execute with:
```bash
tilth <args>
```

## Core Commands

### 1. Read a file (smart view)

```bash
tilth <path>
```

- Small files (< ~6000 tokens): prints full content with line numbers
- Large files: prints a structural outline with line ranges, function/class signatures
- Binary files: prints `[skipped]` with mime type
- Generated files (lockfiles, .min.js): prints `[generated]`

### 2. Drill into a section

```bash
tilth <path> --section 45-89          # exact line range
tilth <path> --section "## Foo"       # markdown heading
```

Use this after seeing an outline to get the exact code you need. The outline gives you line ranges — pass them to `--section`.

### 3. Force full output

```bash
tilth <path> --full
```

Overrides smart view. Use sparingly on large files.

### 4. Search for symbols (definitions + usages)

```bash
tilth <symbol> --scope <dir>
```

Tree-sitter finds where symbols are **defined**, not just where strings appear. Each match shows the surrounding file structure so you know context without a second read.

Expanded definitions include a **callee footer** (`── calls ──`) listing resolved callees with file, line range, and signature — follow call chains without separate searches.

Example:
```bash
tilth handleAuth --scope src/
```

### 5. Content search (text/regex)

```bash
tilth "TODO: fix" --scope <dir>       # text search
tilth "/<regex>/" --scope <dir>       # regex search
```

### 6. Glob files

```bash
tilth "*.test.ts" --scope <dir>
```

### 7. Expand search matches with inline source

```bash
tilth <symbol> --scope <dir> --expand       # expand top 2 matches (default)
tilth <symbol> --scope <dir> --expand=5     # expand top 5 matches
```

Shows inline source for the top N search matches. Useful to see code without a separate read.

### 8. Find all callers of a symbol

```bash
tilth <symbol> --callers --scope <dir>
```

Lists every call site that calls the symbol, with surrounding context. Use this to trace who calls a function.

### 9. Analyze file dependencies (blast radius)

```bash
tilth <file> --deps
```

Shows what the file imports (external deps) and what other files depend on it (dependents). Useful for understanding impact before changing a file.

### 10. Codebase map

```bash
tilth --map --scope <dir>
```

Generates a structural skeleton of the codebase. Useful for getting an overview, but use sparingly — on large codebases it can be verbose.

### 11. Budget control

```bash
tilth <path> --budget 2000
```

Limits response to ~N tokens, reducing detail to fit.

## Workflow Patterns

### Understanding a new codebase
1. `tilth --map --scope .` — get the skeleton
2. `tilth <interesting-file>` — outline the key files
3. `tilth <file> --section <range>` — drill into specific functions

### Finding where something is defined and used
1. `tilth <symbol> --scope .` — definitions first, then usages
2. Follow the callee footer to trace call chains

### Tracing callers / usages of a function
1. `tilth <fn-name> --scope .` — search results show both definitions and usages across the codebase
2. Use regex search for more specific patterns: `tilth "/def.*my_func/" --scope .`

### Reading a large file efficiently
1. `tilth <path>` — get the outline
2. `tilth <path> --section <line-range>` — read only what you need

## Supported Languages (tree-sitter)

Rust, TypeScript, TSX, JavaScript, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift.

For unsupported languages, tilth still works for file reading and content search — you just won't get structural outlines or definition detection.

## Key Advantages Over grep/cat/read

- **One call instead of many:** outline + definitions + usages in a single invocation
- **Structure-aware:** tree-sitter finds definitions, not just text matches
- **Token-efficient:** smart view only shows what matters; large files get outlined
- **Call chain tracing:** callee footers on expanded definitions let you follow code flow
- **Context included:** each match shows surrounding file structure
