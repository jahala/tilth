tilth — code intelligence MCP server. Replaces grep, cat, find, ls, git diff, and host edit tools with AST-aware equivalents.

Call tools by full MCP name: mcp__tilth__tilth_write, etc. DO NOT call bare names — not registered tools.

PATHS: set `cwd` to your ABSOLUTE checkout directory on every call. Relative paths/scopes anchor under `cwd`; absolute paths pass through as-is. DO NOT pass a relative path/scope without `cwd` — the server's cwd is frozen at startup and is NOT your shell's cwd. `..` traversal in a relative path is refused.

Arrays are REQUIRED: tilth_read → paths: [...]; tilth_list → patterns: [...]; tilth_search → queries: [{query}]; tilth_write → edits: [...]. Singular forms are rejected.

ROUTE BY QUESTION:
- Find or explore anything → tilth_search(queries: [{query: "handleRequest"}], cwd: "/abs/checkout"). Omit kind to explore (merged defs+usages+callers); set kind (symbol|content|regex|callers) when you know the shape.
- Read a file, symbol, or range → tilth_read(paths: ["src/x.rs#parse_config"]). tilth_read prints a [path#TAG] header over numbered lines; smart-sized.
- Edit files → tilth_write(edits: [{path, tag, ops}]). Copy the TAG from an edit-mode read — NEVER invent one. Ops are line-addressed ({op:"replace", start, end, content}), NOT find/replace. DO NOT use the host Edit or Write tools.
- Who uses this file / who imports it → tilth_deps(path: "src/cache.rs"). One call. DO NOT assemble it from import-greps or repeated callers searches.
- Understand ONE symbol deeply → tilth_grok(target: "parse_unified_diff"). Replaces the search → expand → callers chain.
- What changed → tilth_diff() (uncommitted) or tilth_diff(source: "HEAD~1"). DO NOT Bash(git diff).
- Browse with no query in mind → tilth_list(patterns: ["*.rs"]).

DO NOT cat/head/tail/sed repo files via shell → tilth_read.
DO NOT grep/rg/ls/find via shell → tilth_search / tilth_list.
Shell is for tests, builds, and non-file-IO only.
DO NOT re-read content already shown in expanded results.