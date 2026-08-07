tilth — code intelligence MCP server. Replaces grep, cat, find, ls, and git diff with AST-aware, token-efficient equivalents.

Call tools by full MCP name: mcp__tilth__tilth_search, mcp__tilth__tilth_read, etc. DO NOT call bare names — they are not registered tools.

PATHS: pass `root` as the ABSOLUTE checkout directory with every relative path or scope. Absolute paths work without it; omitting path/scope searches the server's project. `..` traversal in relative paths is refused.

ROUTE BY QUESTION:
- Find or explore anything → tilth_search(query: "handleRequest", root: "/abs/checkout"). Use kind (symbol|content|regex|callers) when you know the shape; comma-separate up to 5 related symbols.
- Read a file or range → tilth_read(path: "src/x.rs", section: "45-89", root: "/abs/checkout"). Use paths for batches and sections for disjoint slices.
- Who uses this file / who imports it → tilth_deps(path: "src/cache.rs", root: "/abs/checkout"). One blast-radius call; do not assemble it from import greps.
- Understand ONE symbol deeply → tilth_grok(target: "parse_unified_diff", root: "/abs/checkout"). Replaces search → expand → callers.
- Research a subsystem from a natural-language prompt → tilth_scout(prompt: "parse a unified diff"). Use for candidate ranking; use grok for one known symbol.
- What changed → tilth_diff() for uncommitted work; tilth_diff(source: "HEAD~1") for a commit. DO NOT use Bash(git diff) or git log --patch.
- Browse structure with no query in mind → tilth_list(patterns: ["*.rs"]).

DO NOT cat/head/tail/sed repo files via shell → tilth_read.
DO NOT grep/rg/ls/find via shell → tilth_search / tilth_list.
Shell is for tests, builds, and non-file-IO commands only.
DO NOT re-read content already shown in expanded results.