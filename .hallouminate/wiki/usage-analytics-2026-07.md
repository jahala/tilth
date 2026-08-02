# tilth MCP usage analytics — July 2026

Full error/usage analysis of tilth's MCP surface from one month of real agent
sessions (2026-07-02 → 2026-08-02), across three harnesses, benchmarked
against oh-my-pi's built-in tools to separate "table stakes" failure rates
from fixable ones. This page is the evidence base for the per-tool fix specs
(see Recommendations); re-derivation instructions are at the bottom.

## Data sources and caveats

- Database: `~/.claude/analytics/sessions.duckdb`, built by the
  `session-analytics` skill's `ingest.py` (1-hour TTL; `--force` to refresh).
- Harness coverage at time of analysis: claude (~312k entries), codex (~15k),
  opencode (~1.5k). oh-my-pi (`omp`, ~65k entries, 478 sessions from
  `~/.omp/agent/sessions/`) was added 2026-08-02 on dotfiles branch
  `omp-session-analytics` — until that merges and chezmoi syncs, the deployed
  ingest lacks it.
- Three ingest gotchas that skew naive queries:
  - The `mcp_calls` table only captures Claude-style `mcp__tilth__tilth_x`
    names. Codex logs bare `tilth_x`; omp logs single-separator
    `mcp__tilth_x`. Query `tool_uses` with `LIKE '%tilth%'` for cross-harness
    truth.
  - Codex tool results carry `is_error='false'` even for MCP errors — error
    text is inside `content`. Codex error rates must be text-sniffed.
  - Claude successes store `is_error` as NULL, not `'false'` — use
    `coalesce(is_error,'false')` in recovery/join queries.

## Call volume and flagged error rates

Claude harness (the only one with reliable flags, and ~90% of tilth volume):

| Tool | Calls | Errors | Rate |
|---|---|---|---|
| tilth_read | 4,740 | 106 | 2.2% |
| tilth_write | 3,356 | 246 | 7.3% |
| tilth_search | 2,158 | 35 | 1.6% |
| tilth_list | 331 | 3 | 0.9% |
| tilth_deps | 12 | 0 | 0% |
| tilth_diff | 7 | 4 | 57% |
| tilth_grok | 6 | 3 | 50% |

(`tilth_edit` ×61 and `tilth_files` ×17 also appear — retired tool names from
older builds, not the current surface.)

Codex: read 746, search 576, write 300, diff 194, list 103, deps 11, grok 8.
omp: search 82, read 75, write 30, list 19, diff 18. Notable: codex uses
tilth_diff 28× more than claude does — diff's near-zero claude usage is a
discoverability problem (deferred tool), not a value problem.

## tilth_write error taxonomy

246 errors, 107 sessions. Recent-21-day breakdown (~65 errors):

- **Op-shape guessing, ~74%.** Agents invent field names — `find` (the
  find/replace mental model), `anchor`, `new`, `text`, `start_end`, `lines` —
  and op names — `seed`, `replace_file`, `insert`, `replace_lines` — or omit
  `op` entirely. The raw serde error lists valid names but shows no example,
  so recovery costs a full retry round-trip.
- **Harness-level `InputValidationError`** (payload not parseable as JSON),
  ~14%. Raw input is not preserved for failed parses, so the
  large-payload-escaping hypothesis is unconfirmed.
- **Fabricated tags.** `tag: "auto"`, `"prior-search"`, `"PREPEND_MARKER"`,
  empty string, and `"#A0EA"` — that last copied the tag *with* the `#` from
  the `[path#TAG]` display format. Agents mint tags instead of doing an
  edit-mode read.
- Older builds contribute op-grammar-era errors (`missing required parameter:
  edits (op-grammar text blob…)` ×38) — ignore for current-surface decisions.

**Recovery:** 215/246 (87%) recover with a successful tilth_write within 5
minutes — the errors cost round-trips, not failures. But 75 error events were
followed by builtin Edit/Write use within 5 minutes: schema friction actively
drives agents off the tool.

**Cross-model contrast:** of 330 tilth_write calls under codex + omp
(GPT-5/kimi-family models), zero schema-shape errors (verified by
text-sniffing since codex flags are broken). The guessing is Claude-family
behavior specifically — which argues for corrective error text over schema
redesign.

## tilth_read / tilth_search errors

- `paths must be an array of strings` ×15 (`src/mcp/tools/read.rs:51`) —
  bare-string or object elements.
- `unknown read mode: edit` ×3 — agents reaching for the edit-mode read via a
  `mode` value.
- Regex parse failures on literal-looking queries ×4: `#[tool(`, `.step(`,
  `write_lane: Arc::new(Semaphore` — unclosed group/class errors where the
  agent plainly wanted literal search.
- `multi-symbol search limited to 5 queries (got 6)` ×2
  (`src/mcp/tools/search.rs:160`) — hard cap where a soft cap would do.
- Stale error strings advising `set "root"` — pre-cwd-rename binaries still
  installed in other projects; reinstall, not a code fix.
- Not-found errors where the `did you mean:` suggestion exists are observed
  working (one omp case recovered immediately).

## tilth_diff — the clearest cross-harness signal

Every observed diff failure mode, in all three harnesses:

- **Directory passed as `file`** (rejected `not found in diff` at
  `src/diff/mod.rs:310,319`): claude — `src`, `.github`, `src/fanout`,
  `src/wheypoint`; codex — `profiles`, `chezmoi`, `crates`,
  `.hallouminate/wiki`; omp — one agent burned **six consecutive calls**
  trying `agent-profile`, `agent-profile/`, `agent-profile/tests`,
  trailing-slash and absolute variants. Scoping a diff by directory is the
  natural agent move; the tool only accepts exact file paths.
- Nonexistent file path with no closest-match suggestion
  (`workflows/CTwErrorSolver1.json` ×2).
- `git log failed: fatal: ambiguous argument 'main..HEAD'` — the range is
  caller-supplied (`src/main.rs:123`, MCP `log` param), so this is the agent
  guessing `main` in a repo without it; fix is a teaching error naming the
  repo's actual default branch, not a code-side default.

## tilth_grok — adoption problem, not bug problem

14 calls lifetime (8 codex, 6 claude, 0 omp). Codex uses it exactly as
designed (`target` + `scope` + `full` + `budget`); claude omitted the
required `target` in 3 of its 6 calls. One codex target-resolution miss
(`not found: _prune_legacy_inline_hooks`) had no closest-match suggestion.
Claude-side agents appear to read "grok" as "explain this area" (target-less).

## Is 7.3% table stakes? The oh-my-pi comparison

oh-my-pi's own built-in tools, from its 33k tool results (clean per-result
`isError` flag):

| Tool | Harness | Calls | Error rate |
|---|---|---|---|
| tilth_write | claude | 3,356 | 7.3% |
| Claude builtin Write | claude | 1,323 | 5.8% |
| omp builtin write | omp | 1,338 | 5.7% |
| omp builtin edit | omp | 1,793 | 5.6% |
| Claude builtin Edit | claude | 2,318 | 3.5% |
| tilth_write | codex+omp | 330 | ~0% |

Read-family parity: tilth_read 2.2% vs omp read 1.2% vs Claude Read 1.7%.
Search parity: tilth_search 1.6% vs omp grep 0.7%.

**The verdict is in the composition, not the rate.** omp's edit errors are
intrinsic editing failures — sampled: "anchor line 119 is already targeted by
another hunk", edits anchoring to since-changed lines — the stale-anchor
class that tilth's whole-file-tag + 3-way-merge model (see
[[edit-anchor-design]]) is built to absorb. tilth_write's errors are ~74%
interface confusion. So ~5–6% is the observed floor for write tools, tilth's
overage above it is self-inflicted and fixable, and post-fix tilth could
plausibly land *below* the floor because it already absorbs the error class
that keeps other tools at it.

## Ranked recommendations

1. **Example-bearing write-op errors** — intercept the serde error and append
   one minimal valid op (`{"op":"replace","start":12,"end":14,"content":"…"}`)
   plus "ops are line-addressed, not find/replace". Serde aliases only where
   unambiguous (`text`→`content` on append). Kills the dominant error class
   and the builtin-fallback defections.
2. **Diff directory-prefix scoping** — treat a directory `file` argument as a
   prefix filter over the file-level overview. Best-evidenced fix of the
   batch (three harnesses). Add closest-match did-you-mean for unmatched
   paths, and a teaching error on unresolvable ref ranges naming the repo's
   actual default branch.
3. **Regex→literal search fallback** — on pattern compile failure, rerun as
   literal content search with a "treated as literal" note.
4. **Grok ergonomics** — accept `scope`-only calls (grok the scope's main
   exports) or make the missing-`target` error show a complete example call;
   closest-match for unresolved targets.
5. **Input coercion trio** — coerce bare-string `paths`; soft-cap multi-symbol
   queries (run first 5 + note) instead of erroring; on `mode: "edit"` point
   at the edit-mode read flow.
6. **Tag error polish** — strip a leading `#` before validating; error says
   "run an edit-mode tilth_read of <path> to mint a tag".

Analytics-side (dotfiles repo, not tilth): omp adapter PR; fix `mcp_calls`
to include codex/omp naming schemes; fix codex MCP `is_error` capture.

## Re-deriving these numbers

```sql
-- cross-harness tilth volume + flagged errors
SELECT tu.harness, tu.tool_name, count(*) AS calls,
  sum(CASE WHEN tr.is_error='true' THEN 1 ELSE 0 END) AS errors
FROM tool_uses tu JOIN tool_results tr ON tu.tool_use_id = tr.tool_use_id
WHERE tu.tool_name LIKE '%tilth%'
GROUP BY 1,2 ORDER BY 1,3 DESC;

-- write error taxonomy
SELECT substr(tr.content,1,120) AS err, count(*) AS cnt
FROM mcp_calls mc JOIN tool_results tr ON mc.tool_use_id = tr.tool_use_id
WHERE mc.tool_name='mcp__tilth__tilth_write' AND tr.is_error='true'
GROUP BY 1 ORDER BY 2 DESC;
```

Codex tilth errors require text-sniffing `content` for `not found` /
`missing required` / `parse error` patterns. omp queries work normally once
the `omp-session-analytics` dotfiles branch is merged and re-ingested.
