---
status: trusted
last_verified: 2026-08-14
confidence: high
sources:
  - /home/paul/.local/share/cheese/paulnsorensen-tilth/specs/tilth-search-v2-roadmap.md
---
# Search and Dependency Discovery Domain Model

**Search v2 request** — An ordered batch of 1–10 `{query, path?}` objects with an optional shared positive budget; the server owns routing.
_Avoid_: kinded query, mode-selected query
_Code_: NEW ENTITY (`src/mcp/tools/definitions.rs`)

**Search result envelope** — A JSON-text response with top-level `results`, `hints`, and `diagnostics`, preserving request order and per-query completeness.
_Avoid_: prose response, mixed Markdown envelope
_Code_: NEW ENTITY (`src/mcp/tools/search.rs`)

**Client profile** — Normalized MCP `initialize.params.clientInfo.name` used as one persistent-cache isolation key.
_Avoid_: session identity, process identity
_Code_: NEW ENTITY (MCP initialize boundary in `src/mcp/mod.rs`)

**Worktree key** — Canonical Git top-level plus absolute Git dir identifying one independent checkout.
_Avoid_: path-only key, git-common-dir key
_Code_: NEW ENTITY (scope resolution near `src/mcp/tools/mod.rs`)

**Dependency index** — Private per-client, per-worktree redb-derived state containing per-file outgoing edges and reverse indexes.
_Avoid_: shared dependency database, source of truth
_Code_: NEW ENTITY (`src/index/dependency.rs`)

**Verified-only partial** — A core search result plus only dependency facts proven fresh before the internal sub-deadline; stale edges are omitted.
_Avoid_: stale-with-warning
_Code_: NEW ENTITY (`src/index/dependency.rs`)

**Graduation manifest** — A prepared per-harness record that freezes every matched v1/v2 threshold and real-call sample floor before trial advertisement.
_Avoid_: calendar sunset, best-effort comparison
_Code_: NEW ENTITY (`benchmark/experiments/tilth-search-v2-graduation.json`)

**Search continuation** — A typed follow-up for bounded expensive graph work such as callers, callees, siblings, or tests.
_Avoid_: unconditional expansion, public mode flag
_Code_: NEW ENTITY (`src/mcp/tools/search.rs`)
