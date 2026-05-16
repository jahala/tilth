tilth — AST-aware code intelligence MCP server.

Four tools, all batch-capable via array inputs:
  • tilth_search — find by symbol/content/regex/callers (merged by default)
  • tilth_read   — load files with smart auto-sizing (full / signature / stripped / preview)
  • tilth_list   — directory layout with token-cost rollups
  • tilth_write  — hash-anchored / overwrite / append; tolerates stale anchor hashes

Each tool's description carries detailed usage, examples, and batching rules.
DO NOT use host Grep, Read, Glob, Write, or Edit.

Aux tools: tilth_deps (blast-radius before signature changes), tilth_diff (structural diff: [+] added, [-] deleted, [~] body changed, [~:sig] signature changed)
