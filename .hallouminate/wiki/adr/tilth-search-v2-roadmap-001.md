---
status: trusted
last_verified: 2026-08-14
confidence: high
sources:
  - /home/paul/.local/share/cheese/paulnsorensen-tilth/specs/tilth-search-v2-roadmap.md
  - src/mcp/mod.rs:301-328
  - src/lib.rs:184-210
---
# Search v2 Public Discovery Topology

Tilth will converge on five public MCP verbs: `search`, `read`, `list`, `diff`, and conditional `write`. Public `grok` and `deps` disappear only after the measured search-v2 trial; their useful behavior moves behind search. `list` remains a separate browse verb.

## Context

Session analytics showed that models use search heavily but rarely select grok or deps, even when those capabilities would improve an exact-symbol answer. The current split makes the caller classify intent before Tilth has resolved the target. In contrast, list has distinct directory-tree, overview, budget, and token-rollup behavior and materially replaces host browsing commands.

Current MCP dispatch exposes search, deps, grok, list, read, diff, and optional write (`src/mcp/mod.rs:301-328`). The Rust library independently exposes `run_deps` and `run_grok` (`src/lib.rs:184-210`).

## Decision

- Fold bounded definition/signature/body context into unique exact-symbol/file search results.
- Always attempt verified dependency impact for unique exact symbol/file results.
- Emit typed continuations for expensive callers, callees, siblings, and tests.
- Keep list public and behaviorally stable.
- Remove only the MCP grok/deps registrations at graduation; retain the Rust library engines.

## Alternatives rejected

- **Keep all discovery verbs:** preserves schema tax and caller-side intent classification.
- **Delete grok/deps without replacement:** loses high-value context rather than improving adoption.
- **Fold list into read or search:** erases a coherent browse contract and harms batching/token rollups.

## Consequences

Search becomes the single semantic discovery entry point, while list remains the one structural browse entry point. Search orchestration grows, but the internal grok/dependency engines remain reusable modules rather than duplicated logic. Final removal is gated by the trial described in `adr/tilth-search-v2-roadmap-004.md`.
