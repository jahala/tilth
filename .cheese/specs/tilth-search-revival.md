# tilth_search revival — scope

## Problem

The `#35` upstream merge (`2254b20`) gutted the `tilth_search` wire layer, the
same way it reverted read to singular and killed `tilth_list`. The current
`tool_search` is a 4-arg `(args, cache, session, bloom)` singular-`query`
dispatcher. A later P0 PR (`#38`, `2cf0411`) is **innocent** — it never removed
the batch form; it added comma-split multi-target *on top of* the
already-reduced singular query. So everything below is a `2254b20` casualty.

The 11 dropped `tool_search` integration tests in pre-merge `3801a4c` are the
behavioral spec for what to restore.

## What was stripped (11 tests → 6 feature clusters)

| # | Feature | Tests | Layer | Deps on main |
|---|---------|-------|-------|--------------|
| 1 | Batch `queries: [{query, glob?, kind?}]` array form (per-entry kind/glob override, 10-entry cap, stray-field tolerance) | `queries_array_form`, `queries_empty_errors`, `queries_missing_query_field_errors`, `queries_over_limit_rejected`, `tolerates_stray_context_field` | **wire** | none — pure dispatch |
| 2 | `if_modified_since` redaction of unchanged result sections | `if_modified_since_redacts_unchanged_bodies` | **wire** | `iso::file_changed_since`, `iso::unchanged_stub` ✅ present |
| 3 | Cache-token JSON first line (`{"if_modified_since": "<iso>"}`) | `first_line_is_parseable_cache_token_json` | **wire** | `iso::with_header`, `iso::parse_iso_utc` ✅ present |
| 4 | Merged default (no `kind` → faceted `## symbol results` / `## content results` / `## caller results`) | `default_merges_symbol_content_and_callers` | **wire + engine reconcile** | calls gone `_expanded_mode` fns; `SymbolMode::Strict` **gone** |
| 5 | Edit-mode hashline rendering in expanded source (`1:xxx\|…` anchors, suppress `│` gutter) | `expand_emits_hashlines_in_edit_mode` | **engine (deep)** | engine lost the `edit_mode`→hashline switch; trailing bool is now `full` |
| 6 | Diagnostic header fields + glob-zero hint (`Files searched: N`, `Content hits: N`, `Files matched glob: N`, `glob matched no files`, `no content matches`) | `glob_excludes_everything_emits_glob_hint`, `zero_matches_emits_empty_header_with_kind_hint` | **engine (header)** | none of these strings exist on main |

## Layer analysis (the effort driver)

Pre-merge wire called engine entry points that **no longer exist**:
`search_symbol_expanded_mode(.., SymbolMode::Strict, edit_mode)` and
`search_content_expanded_mode(.., edit_mode)`. The current engine replaced them
with `search_symbol_expanded(.., context, glob, full)` /
`search_content_expanded(.., context, glob, full)` — the trailing bool is
**`full`, not `edit_mode`**, and `SymbolMode` is gone. So:

- Clusters **1, 2, 3** are wire-only with intact deps → low risk, restore
  `search.rs` dispatcher + redaction helpers + `with_header` call.
- Cluster **4** needs the merged-default rewritten against the current
  `_expanded` engine API, and a decision on `SymbolMode::Strict` (restore the
  enum, or accept current `symbol::search` defaults).
- Cluster **5** is the deep one: re-thread an `edit_mode` flag through
  `tool_search` (4→5 args), the `_expanded` engine fns, and
  `format_search_result` (`search/mod.rs:913`) so expanded source renders
  hashline anchors under edit mode and the gutter otherwise. The hashline
  machinery (`format::hashlines`) exists (read uses it) — the work is plumbing
  the flag and the branch in the result formatter.
- Cluster **6** is engine header enrichment: the search result header must emit
  `Files searched` / `Content hits` / `Files matched glob` counts and the
  glob-zero / no-content hints. Reconcile with `#38`'s existing `0 matches`
  hint.

## #38 reconciliation — clean, layered

The array form (cluster 1) and `#38`'s comma-split multi-target are **orthogonal
layers**, not a conflict:

- **Array form** = outer batch: each `queries[i]` is an independent query with
  its own `kind`/`glob`, dispatched through `tool_search_single`.
- **Comma-split** = inner: within a single `kind=symbol`/`kind=callers` query,
  `"foo,bar"` fans out to multiple targets (≤5, deduped — `#38`'s work).

Restore the outer array dispatch as a wrapper; keep `#38`'s comma-split,
default budget cap, caller dedup, and zero-match hint as the inner per-entry
behavior. Nothing `#38` did gets reverted.

## Recommended split — two PRs

**PR-A (wire layer, low risk):** clusters 1, 2, 3 + the merged-default skeleton
(cluster 4) wired to current engine fns with `full=false`, `edit_mode` threaded
but the engine still gutter-only. Restores 5–6 of the 11 tests. Self-contained,
no engine-format changes, minimal benchmark impact.

**PR-B (engine, higher risk):** clusters 5 and 6 — `edit_mode` hashline
rendering in `format_search_result` and the diagnostic/glob header fields.
Restores the remaining tests. **Changes the default search output surface**, so
it must be validated with the haiku benchmark suite (`rg_search_dispatch`,
`rg_trait_implementors`, `gin_servehttp_flow`) per the MCP-instruction-change
rule — faceted merged-default + header fields materially change what the model
sees.

## Knock-on changes

- `tool_search` signature 4→5 args (`edit_mode`). Update the dispatcher
  (`mcp/mod.rs:311`) to pass real `edit_mode` (already in scope, used by
  `tool_read`), and the 2 existing `search.rs` tests that call `tool_search`
  with 4 args.
- Re-advertise the `queries` array + per-entry `kind`/`glob` in
  `tool_definitions` (`tilth_search` schema) — currently singular-only.
- The excluded `tool_definitions_expose_v2_surface_only` test asserts singular
  `query` is *hidden*; if the array form becomes the advertised surface, that
  test's intent returns — decide whether to restore it.

## Open questions

1. `SymbolMode::Strict` — restore the enum, or accept current `symbol::search`
   default matching for the merged default?
2. Should singular `query` stay advertised alongside `queries`, or become a
   hidden back-compat alias (pre-merge v2 hid it)?
3. PR-B changes default output — acceptable to gate on a haiku benchmark run
   before merge?

## Estimate

- PR-A: ~250–350 LOC (mostly restored wire + 5–6 tests), low risk.
- PR-B: ~150–250 LOC across `search/mod.rs` formatter + header, **plus** a
  benchmark gate, medium-high risk (output-surface change).
