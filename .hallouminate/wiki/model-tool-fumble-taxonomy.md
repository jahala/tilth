# Sonnet 5 tool-fumble taxonomy (2026-08-08)

Catalogue of tool-call failure classes found by forensics over
`benchmark/results/streams/20260808_030838/` (script `fumbles.py`), during the
sonnet5 tilth investigation. Ranked roughly by frequency/impact. Source:
`.cheese/notes/tilth-pr196-sonnet5-audit.md`.

## 1. Missing `root`/`cwd` on relative paths — biggest class

`<certain>` 30 error+retry trips, 25 of them on upstream `tilth_read` alone.
The server cannot see the shell's live cwd (it's a long-lived stdio process,
spawned once); benchmark cells run headless with no Claude Code hook to
inject `cwd`, so every relative-path call the model makes without an explicit
`cwd`/`root` fails once and gets retried. See
[MCP cwd / workspace-root binding](mcp-cwd-root-binding.md) for the general design
history — this is that design's cost surfacing in a hookless client.

`<certain>` Design option raised (not yet decided, touches fork-law posture
from #113): default an omitted `cwd` to the server process's startup
directory instead of a hard teaching refusal, for hookless contexts
specifically. Not implemented.

## 2. Grep-style/regex/full-signature queries break `tilth_search`'s grammar

`<certain>` 29 empty `tilth_search` results catalogued. Sonnet 5 writes
grep-style queries the search grammar rejects or misses:

- Escaped regex: `func (c \*Context) reset`, `Depends\(`
- Full Go method signatures instead of bare symbol names
- Comma-OR over full signatures: `func (c *Context) Set,func (c *Context) Get`

Each miss costs a trip and, on the fork arm, often triggers a native
fallback (see [MCP cost model](mcp-cost-model-sonnet5.md)). Product fix
candidates (untasked): server-side query normalization (strip receiver
syntax, auto-retry as regex) or explicit query-grammar guidance in the
search tool description.

## 3. Literal tab bytes in JSON tool inputs

`<certain>` Two distinct sub-failures, verified against raw bytes:

- **`tilth_write` deaths**: literal tab bytes inside JSON content strings
  when the model writes tab-indented Go — "Invalid control character" kills
  the call in Claude Code's JSON parser *before* it reaches the server. Only
  an instruction-level mitigation is possible (server never sees the
  malformed call).
- **Native `Read` deaths** (fork arm, different fumble): the model writes
  line ranges into the `offset` field as a pair, e.g.
  `"offset": 195, 212` — trailing/double commas, not tabs.

## 4. `tilth_grok` couldn't resolve Go type declarations — FIXED

`<certain>` `tilth_grok` reported "not found: HandlersChain" ×5 — it could
not resolve `type HandlersChain []HandlerFunc`, the symbol behind all 6
`gin_context_next` grader failures at the time.

Root cause: Go `type_declaration` names live on the inner `type_spec` node;
`extract_definition_name` never descended into it, so **all** Go type
definitions (structs, func types, aliases) were silently dropped from
outlines, symbol search, and grok — not just this one symbol.

**Fixed in commit `9b17e8e`**: descend into `type_spec`, plus regression
tests. Verified `HandlersChain` groks and `type Engine struct` now appears
as a definition on the gin fixture. 897 tests / clippy / fmt green.

## 5. Upstream line:hash anchor mangling — fork is structurally immune

`<certain>` On upstream (line:hash anchor model), the model invents or
mangles anchors (`'189:'`, `'107:?'`) despite instructions, plus at least one
legitimate hash-mismatch from real drift.

This class **cannot occur on the fork**: the whole-file-tag edit model
(post-#99) uses integer start/end line numbers plus a 4-hex tag, no per-line
hash anchors; drift is instead handled by the seen-lines gate and 3-way
merge recovery. See
[Edit-anchor design: per-line hash vs whole-file tag](edit-anchor-design.md) for
the design history.

## 6. Native `Edit` whitespace mismatches

`<certain>` `old_string` whitespace mismatches ×2 — generic model fumble, not
tilth-specific.

## Related

- `.cheese/notes/tilth-pr196-sonnet5-audit.md`
- [MCP cwd / workspace-root binding](mcp-cwd-root-binding.md)
- [Edit-anchor design](edit-anchor-design.md)
- [MCP cost model: why tilth costs more per correct answer](mcp-cost-model-sonnet5.md)
- Fix commit `9b17e8e` (Go type_declaration name resolution)
