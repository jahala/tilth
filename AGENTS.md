<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->
tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

PATHS: pass an ABSOLUTE path/scope, or set `root` to your ABSOLUTE checkout directory. DO NOT pass a relative path/scope without an absolute `root` — the server's cwd is frozen at startup and is NOT your shell's cwd, so a relative path is refused. A relative `root` is also refused.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(query: "handleRequest").
tilth_files is ONLY for listing directory contents when you have no symbol or text to search for.
DO NOT use Read if content is already shown in expanded search results.
For file reads, text search, and directory listings, prefer tilth_search, tilth_read, and tilth_files over Grep, Read, or Glob.

Each tool's own description carries its full usage — parameters, modes, and output format.

For code search, prefer tilth_search over Grep or Bash(grep/rg).
For file reads, prefer tilth_read over Read or Bash(cat).
For file listings, prefer tilth_files over Glob or Bash(find/ls).
To check what changed, use tilth_diff instead of Bash(git diff/git log).
DO NOT use Bash(git diff) or Bash(git log --patch). Use tilth_diff instead.
DO NOT re-read files already shown in expanded search results.

tilth_write replaces the host Edit and Write tools. Its full usage — hash/overwrite/append modes, hash anchors from tilth_read, grouping multi-file writes into one call, per-file partial success — is in the tilth_write tool description.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.
