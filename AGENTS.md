<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->
tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(query: "handleRequest").
tilth_files is ONLY for listing directory contents when you have no symbol or text to search for.
DO NOT use Read if content is already shown in expanded search results.
DO NOT use Grep, Read, or Glob. Always use the better tools tilth_search (grep), tilth_read (read), tilth_files (glob).

Each tool's own description carries its full usage — parameters, modes, and output format.

To search code, use tilth_search instead of Grep or Bash(grep/rg).
To read files, use tilth_read instead of Read or Bash(cat).
To find files, use tilth_files instead of Glob or Bash(find/ls).
To check what changed, use tilth_diff instead of Bash(git diff/git log).
DO NOT use Bash(git diff) or Bash(git log --patch). Use tilth_diff instead.
DO NOT re-read files already shown in expanded search results.

tilth_write replaces the host Edit and Write tools. Its full usage — hash/overwrite/append modes, hash anchors from tilth_read, grouping multi-file writes into one call, per-file partial success — is in the tilth_write tool description.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.
