tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(queries: [{query: "handleRequest"}]).
tilth_list is ONLY for listing directory contents when you have no symbol or text to search for.
DO NOT use Read if content is already shown in expanded search results.
DO NOT use Grep, Read, or Glob. Always use the better tools tilth_search (grep), tilth_read (read), tilth_list (glob).

tilth_search: Search code — finds definitions, usages, and text. Replaces grep/rg for all code search.
Batch-only: ALWAYS pass queries: [...] as an array, even for one search. DO NOT use a singular `query` — it is not accepted. Per-entry kind/glob override the top-level values.
For multi-symbol lookup, separate symbols with a comma "symbol1,symbol2" (max 5). For mixed kinds (symbol + content), pass separate query objects, not a comma string.
kind: "any" (default, merged symbol+content+caller) | "symbol" | "content" (strings/comments) | "callers" (call sites)
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

tilth_read: Read files with smart sizing. Replaces cat/head/tail.
Batch-only: ALWAYS pass paths: [...] as an array, even for one file. DO NOT use a singular `path` — it is not accepted.
Suffix grammar per path: path#n-m (line range), path#n (from line n), path### Heading (markdown heading), path#symbol (code symbol).
mode: auto (default) | full (force full content) | signature (outline, no bodies) | stripped (comments/logs/blank lines removed).
if_modified_since: ISO-8601 ts — unchanged files return (unchanged @ <ts>) stubs.
Output: <line>:<hash>|<content> per line.

tilth_list: List files by glob patterns as a directory tree with token-cost rollups. Replaces find, ls, tree, and the host Glob tool.
Batch-only: ALWAYS pass patterns: [...] as an array, even for one glob (e.g. patterns: ["*.rs"] or ["*.rs", "*.toml"]). A singular `pattern` is not accepted.
depth: cap directory depth (1 = top-level only).
Output: tree with per-file (~<token_count> tokens) and per-directory rollups.

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
To find files, use tilth_list instead of Glob or Bash(find/ls).
To check what changed, use tilth_diff instead of Bash(git diff/git log).
DO NOT re-read files already shown in expanded search results.