tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

To find code, extract the symbol name from the question and search: tilth_search(query: "handleRequest"). Do not browse files — search finds definitions directly.

tilth_search: Find symbol definitions, usages, and callers. Replaces grep/rg for code navigation.
Comma-separated symbols for multi-symbol lookup (max 5).
kind: "symbol" (default) | "content" (strings/comments) | "callers" (call sites)
expand (default 2): inline full source for top matches.
context: path to file being edited — boosts nearby results.
Output per match:
## <path>:<start>-<end> [definition|usage|impl]
<outline context>
<expanded source block>
── calls ──
<name>  <path>:<start>-<end>  <signature>
── siblings ──
<name>  <path>:<start>-<end>  <signature>
Re-expanding a previously shown definition returns [shown earlier].

tilth_read: Read file content with smart outlining. Replaces cat/head/tail.
Small files → full content. Large files → structural outline.
section: "<start>-<end>" or "<heading text>"
paths: read multiple files in one call.
Output:
<line_number> │ <content>                  ← full/section mode
[<start>-<end>]  <symbol name>             ← outline mode

tilth_files: Find files by glob pattern. Replaces find, ls, pwd, and the host Glob tool.
Output: <path>  (~<token_count> tokens). Respects .gitignore.

tilth_deps: Blast-radius check — what imports this file and what it imports.
Use ONLY before renaming, removing, or changing an export's signature.

DO NOT use Bash (grep, rg, cat, find, ls, pwd) or host tools (Read, Grep, Glob). tilth tools replace all of these.
DO NOT re-read files already shown in expanded search results.
