<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->
tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.

Call tools by their full MCP name — prefix `mcp__tilth__`, e.g. `mcp__tilth__tilth_search`, `mcp__tilth__tilth_read`. The bare names below (tilth_search, tilth_read, …) are shorthand. DO NOT call a bare name — it is not a registered tool.

PATHS: pass an ABSOLUTE path/scope, or set `root` to your ABSOLUTE checkout directory. DO NOT pass a relative path/scope without an absolute `root` — the server's cwd is frozen at startup and is NOT your shell's cwd, so a relative path is refused. A relative `root` is also refused.

REQUIRED arrays per verb: tilth_read → paths: [...]; tilth_write → files: [...]; tilth_list → patterns: [...]; tilth_search → queries: [{query}]. Pass an absolute `root` whenever paths/scope are relative — the server's cwd is frozen at startup and refuses a relative scope without an absolute `root`.

To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.
Usage: tilth_search(queries: [{query: "handleRequest"}]).
tilth_list is ONLY for listing directory contents when you have no symbol or text to search for.
DO NOT use Read if content is already shown in expanded search results.
For file reads, text search, and directory listings, prefer tilth_search, tilth_read, and tilth_list over Grep, Read, or Glob.

tilth_search: Search code — finds definitions, usages, and text. Replaces grep/rg for all code search.
Batch-only: ALWAYS pass queries: [...] as an array, even for one search. DO NOT use a singular `query` — it is not accepted. Per-entry kind/glob override the top-level values.
kind: "any" (default, merged symbol+content+callers) | "symbol" | "content" (literal text) | "regex" | "callers" (call sites)
For "where is X defined / what calls Y", use kind: "symbol" (definitions) or kind: "callers" (call sites) — not content/regex. Content/regex match text; symbol/callers match AST definitions and real call sites. Example: tilth_search(queries: [{query: "handleAuth", kind: "symbol"}]).
Comma-OR is for kind any/symbol/callers: "symbol1,symbol2" (max 5). DO NOT comma-separate a content query — content matches the whole string literally, commas included. To match any of several terms, use kind:"regex" with "a|b|c".
expand (default 2): inline full source for top matches.
context: path to file being edited — boosts nearby results.
glob: file pattern filter — "*.rs" (whitelist), "!*.test.ts" (exclude).
root: absolute checkout dir. Required if `scope` is relative (or omitted); absolute `scope` needs no root. The server cannot see your shell cwd.
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
root: absolute checkout dir. Required if any path in `paths` is relative; absolute paths need no root and are used as-is. The server cannot see your shell cwd.

tilth_list: List files by glob patterns as a directory tree with token-cost rollups. Replaces find, ls, tree, and the host Glob tool.
Batch-only: ALWAYS pass patterns: [...] as an array, even for one glob (e.g. patterns: ["*.rs"] or ["*.rs", "*.toml"]). A singular `pattern` is not accepted.
depth: cap directory depth (1 = top-level only).
root: absolute checkout dir. Required if `scope` is relative (or omitted); absolute `scope` needs no root. The server cannot see your shell cwd.
Output: tree with per-file (~<token_count> tokens) and per-directory rollups.

tilth_deps: Blast-radius check — what imports this file and what it imports.
Use ONLY before renaming, removing, or changing an export's signature.
root: absolute checkout dir. Required if `path`/`scope` are relative; absolute ones need no root. The server cannot see your shell cwd.

tilth_grok: Everything structural about a symbol in one call — def + body + signature + doc + callees + callers + siblings + tests.
Usage: tilth_grok(target: "parse_unified_diff"). Also accepts "src/file.rs:7" or "Type::method".
scope: narrow when the name is ambiguous. full: widen caps from 5/5/8/8 to 50/30/30/30.
root: absolute checkout dir. Required if `scope` is relative (or omitted); absolute `scope` needs no root. The server cannot see your shell cwd.
Use ONLY for "understand this symbol" questions — replaces the search → expand → callers chain.
DO NOT use for concept search (use tilth_search) or reading file contents (use tilth_read).

tilth_diff: Structural diff — shows what changed at function level. Replaces Bash(git diff).
Usage: tilth_diff(source: "HEAD~1") for last commit. No args = uncommitted changes.
scope: "file.rs" or "file.rs:fn_name". log: "HEAD~5..HEAD" for per-commit summaries.
search: filter to lines matching a term. blast: true to show callers of changed signatures.
Output: [+] added, [-] deleted, [~] body changed, [~:sig] signature changed.
DO NOT use Bash(git diff) or Bash(git log --patch). Use tilth_diff instead.

DO NOT `cat`/`head`/`tail`/`sed -n` a file via the shell → use tilth_read.
DO NOT `grep`/`rg`/`ls`/`find`/`fd` on repo files via the shell → use tilth_search or tilth_list.
To check what changed, use tilth_diff instead of Bash(git diff/git log).
Shell out only for tests, builds, and non-file-IO commands.
DO NOT re-read files already shown in expanded search results.

tilth_write: Batch edit files with an op-grammar text blob (the `edits` string). Replaces the host Edit and Write tools.
Read first (edit mode): tilth_read / tilth_search show a `[path#TAG]` header then `N:content` numbered lines. Copy the `[path#TAG]` header VERBATIM and reference the line numbers you see. NEVER invent a TAG.
Send edits as ONE `edits` string of `[path#TAG]` sections, with op lines beneath each header. Line numbers are 1-based inclusive, from the numbered read:
SWAP a.=b:  then payload — replace lines a..b (SWAP n: for a single line)
DEL n | DEL a.=b — delete a line or range
INS.PRE n: | INS.POST n:  then payload — insert before/after line n
INS.HEAD: | INS.TAIL:  then payload — insert at start/end of file
SWAP.BLK n: | SWAP.BLK #symbol:  then payload — replace the tree-sitter block at line n or named symbol
DEL.BLK n | DEL.BLK #symbol — delete that block
INS.BLK.POST n: | INS.BLK.POST #symbol:  then payload — insert after that block
REM — delete the file
MV dest — move/rename the file
Payload: one row per line after the op header; prefix a row with `+` to force it literal (e.g. a row that itself looks like an op or a `[header]`).
Example:
[src/x.rs#1A2B]
SWAP 2:

+ let y = 1;
DEL 5
INS.POST 8:
+// trailing note
Drift: the TAG binds the section to the content you read. If the file changed since, tilth 3-way-merges your ops onto the live file; if the merge conflicts the section is REJECTED — re-read THAT file and retry it. A tag not from this session is rejected (never invent one).
New file: a tagless `[path]` header (no #TAG) seeds a NEW file — use INS.HEAD for its content.
Append cleanly: prefer `INS.POST <last-content-line>` over `INS.TAIL:` — INS.TAIL inserts after the file's trailing empty row, adding a blank line for newline-terminated files.
Sections are independent (best-effort): a rejected section does NOT block the others; scan the per-`## <path>` results for failures. Max 20 sections.
root: absolute checkout dir. Required if any section path is relative. RELATIVE section paths and MV destinations are anchored under root and confined to it; ABSOLUTE section paths are also confined to root (or to the server's startup directory when root is omitted) — `..` traversal or paths outside the confinement root are refused. The server cannot see your shell cwd.
Pass diff: true for a compact before/after diff per section.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.
