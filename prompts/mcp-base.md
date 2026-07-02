tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(query: "handleRequest").
Tracing a call chain or a symbol's callers/callees? tilth_grok(symbol) returns def + callers + callees + siblings in one call — best in Go, Rust, and other statically-typed languages.
Following a chain or comparing symbols? Pass them together: tilth_search("parse,decode,apply") (up to 5). Several parts of one file? tilth_read(sections=["10-40","80-110"]) in one call.
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