tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(query: "handleRequest").
tilth_files is ONLY for listing directory contents when you have no symbol or text to search for.
DO NOT use Read if content is already shown in expanded search results.
DO NOT use Grep, Read, or Glob. Always use the better tools tilth_search (grep), tilth_read (read), tilth_files (glob).

tilth_search: Search code — finds definitions, usages, and text. Replaces grep/rg for all code search.
For multi-symbol lookup, separate each with a comma "symbol1,symbol2" (max 5).
kind: "symbol" (default) | "content" (strings/comments) | "callers" (call sites)
expand (default 2): inline full source for top matches.
context: path to file being edited — boosts nearby results.
glob: file pattern filter — "*.rs" (whitelist), "!*.test.ts" (exclude).
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
Batch-only: ALWAYS pass paths: [...] as an array, even for one file. DO NOT use a singular `path`.
Small files → full content. Large files → structural outline.
For one file you may also pass sections (array of "<start>-<end>" or "<heading text>" ranges; single-element array for one range) or full.
Output:
<line_number> │ <content>                  ← full/sections mode
[<start>-<end>]  <symbol name>             ← outline mode

tilth_files: Find files by glob pattern. Replaces find, ls, pwd, and the host Glob tool.
patterns: run multiple globs in one call (e.g. patterns: ["*.rs", "*.toml"]).
Output: <path>  (~<token_count> tokens).

tilth_deps: Blast-radius check — what imports this file and what it imports.
Use ONLY before renaming, removing, or changing an export's signature.

tilth_grok: Everything structural about a symbol in one call — def + body + signature + doc + callees + callers + siblings + tests.
Usage: tilth_grok(target: "parse_unified_diff"). Also accepts "src/file.rs:7" or "Type::method".
scope: narrow when the name is ambiguous. full: widen caps from 5/5/8/8 to 50/30/30/30.
Use ONLY for "understand this symbol" questions — replaces the search → expand → callers chain.
DO NOT use for concept search (use tilth_search) or reading file contents (use tilth_read).

tilth_diff: Structural diff — shows what changed at function level. Replaces Bash(git diff).
Usage: tilth_diff(source: "HEAD~1") for last commit. No args = uncommitted changes.
scope: "file.rs" or "file.rs:fn_name". log: "HEAD~5..HEAD" for per-commit summaries.
search: filter to lines matching a term. blast: true to show callers of changed signatures.
Output: [+] added, [-] deleted, [~] body changed, [~:sig] signature changed.
DO NOT use Bash(git diff) or Bash(git log --patch). Use tilth_diff instead.

To search code, use tilth_search instead of Grep or Bash(grep/rg).
To read files, use tilth_read instead of Read or Bash(cat).
To find files, use tilth_files instead of Glob or Bash(find/ls).
To check what changed, use tilth_diff instead of Bash(git diff/git log).
DO NOT re-read files already shown in expanded search results.