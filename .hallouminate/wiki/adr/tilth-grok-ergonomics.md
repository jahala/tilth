# ADR: tilth-grok-ergonomics — curd 2 inclusion and shape

Date: 2026-08-02. Status: accepted (two-key handshake).
Spec: ~/.local/share/cheese/paulnsorensen-tilth/specs/tilth-grok-ergonomics.md

## Context

Grok (tool 5 of the July-2026 analytics fixes) had an approved "minimal
ergonomics" direction but a gated design fork: what candidate set should
back a did-you-mean on unresolved plain-name targets, given no enumerable
symbol index exists (BloomFilterCache is membership-only; fuzzy_path scores
file paths, not symbols)? Options on the table: scope-bounded outline walk,
existing-paths-only (= status quo), repo-wide walk, or dropping the curd.

## Decision

Include curd 2 as a scope-bounded outline walk. The deciding rule was
Paul's: "if we still have fuzzy matching in our fork, do it; otherwise
curd 1 only." The fork has it — nucleo-matcher in-tree at
src/read/fuzzy_path.rs, already implementing the suggest-only pattern
(subsequence scoring, hard cap with logged truncation, top-3). Curd 2
reuses that pattern over outline definition names instead of paths.

Weighed against: dropping curd 2 (14 grok calls lifetime; a miss costs one
fallback tilth_search since grok resolution IS search_symbol_raw), and a
zero-computation teaching-hint fallback. The existing-machinery condition
made reuse cheap enough to justify a real suggestion.

## Shape decisions

- New module src/search/fuzzy_symbol.rs mirroring fuzzy_path's structure,
  rather than generalizing fuzzy_path.rs — keeps the path scorer's
  path-tuned config (match_paths) and the symbol scorer (Config::DEFAULT)
  from sharing a signature they'd immediately diverge on.
- Suggest-only, never auto-resolve (standing posture, 2026-07-04).
- Qualified-owner miss path (owner_suggestion) untouched.
- Curd 1 error text approved verbatim, including the redirect line
  ("for area-level exploration use tilth_search or tilth_list") targeting
  the observed target-less misuse (3 of claude's 6 calls).

## Consequences

- 2 curds / 1 wave, file-disjoint; cookable in parallel.
- prompts/*.md untouched; the adoption problem (grok at 14 calls lifetime,
  deps at ~19:1 manual-vs-tool) moves to a separate instructions spec —
  logged as spec follow-ups with the 2026-08-02 DB evidence.
