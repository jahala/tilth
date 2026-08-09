---
slug: tilth-api-adoption
status: approved
created: 2026-08-09
confidence: high
gates_overridden: []
agent_introduced_scope: [replace_text, "Op::TextSwap", resolve_text_swap, TextUnmatched, TextAmbiguous, create_file, "create_file exists/tag-present rejection rules", "overview.rs::fingerprint() reuse", "5-rep haiku validation protocol"]
entity_referent_bindings:
  - {noun: replace_text, verdict: NEW ENTITY, referent: "src/edit/parser.rs (new Op variant)", citation: "dossier §4", note: "substring semantics (2b)"}
  - {noun: create_file, verdict: NEW ENTITY, referent: "src/edit/apply.rs FileOp family (new sibling)", citation: "production census", note: "tag-less sections only"}
  - {noun: FileOp, verdict: bound, referent: "src/edit/apply.rs:37", citation: "explorer digest"}
  - {noun: LineOp, verdict: bound, referent: "src/edit/apply.rs:92", citation: "explorer digest"}
  - {noun: lower_ops, verdict: bound, referent: "src/edit/apply.rs:110", citation: "code read"}
  - {noun: anchor_lines, verdict: bound, referent: "src/edit/apply.rs:189", citation: "code read"}
  - {noun: overview fingerprint, verdict: bound, referent: "src/overview.rs fingerprint()", citation: "decomposer correction", note: "NOT map.rs (full-tree rollup)"}
---

# tilth API adoption — replace_text, create_file, list overview

## Problem

Agents route around tilth's API where it fights their trained shapes or lacks an
affordance, and instruction bans alone decay. Evidence: haiku Bash leakage in 4/10 fork
benchmark runs vs 0/10 upstream (all leaks = repo orientation via find/ls, plus one
heredoc scratch-file spree); sonnet5 host-Edit fallback in fork-arm edit tasks; 23
production tilth_write calls died inventing tag-less ops (seed/write/create) for file
creation.

## Goals

- Give models their trained find/replace edit shape inside the tag/snapshot contract.
- Name file creation so agents stop guessing op names.
- Answer "show me the shape of this repo" so native find/ls loses its motive.
- Keep the --mcp --edit surface ≤ 13,779 chars.

## Non-goals

- **No op pruning.** Block ops and the insert family stay — production census: block ops
  used across 11 projects (2026-07-09 → 2026-08-02, 12 confirmed successes), inserts
  1,662 uses. (Reverses the benchmark-census recommendation; see ADR-002.)
- No batch-posture change (hold batch-only arrays + JIT nudge).
- No scratch-file/report-authoring affordance (answer-inline is correct; the instruction
  ban covers it). **Dropped explicitly at follow-up disposition.**
- No version bump (fork law: 0.8.4).
- No change to tag/snapshot/recovery machinery.

## Deferred follow-ups

- **tilth-api-adoption-F001** — Op-kind benchmark telemetry (record per-op-kind counts in
  results JSONL like `batch_sizes`, so replace_text adoption / line-op atrophy is measurable).
  - Destination: github_issue
  - State: created
  - Reference: https://github.com/paulnsorensen/tilth/issues/173
- **tilth-api-adoption-F002** — Block-op teaching line (one description sentence explaining
  block ops; competes for cap headroom, hence deferred).
  - Destination: github_issue
  - State: created
  - Reference: https://github.com/paulnsorensen/tilth/issues/174
- **tilth-api-adoption-F003** — `replace_all` escape hatch for replace_text (multi-site
  rename design note).
  - Destination: github_issue
  - State: created
  - Reference: https://github.com/paulnsorensen/tilth/issues/175

## Approach

Three additive API changes + a budget reconciliation, designed haiku-first under a hard
13,779-char surface cap:

1. **`replace_text` op (substring, host-Edit semantics).** New `Op::TextSwap { old, new }`
   in `src/edit/parser.rs`; new arm in `lower_ops` (`src/edit/apply.rs`) following the
   `Op::Block` precedent — resolved against the `text` param (snapshot text on every
   canonical path) via `resolve_text_swap(text, old, new) -> Result<(u32, u32,
   Vec<String>), ApplyError>`: locate `old` (exactly once), expand to covering whole-line
   span, substitute, emit `LineOp::Swap`. `ApplyError` gains `TextUnmatched { preview }`
   and `TextAmbiguous { count }`. Requires a tag. Schema: 12th `oneOf` branch (~153 chars)
   + description mention (~+25). Seen-lines gate, overlap/bounds, both recovery
   strategies, snapshots, mismatch classification untouched (lowers before any gate runs).
2. **`create_file` op.** FileOp sibling of `delete_file`/`move_file`. Wire:
   `{"op": "create_file", "content": "..."}` in a tag-less section. Errors: file exists
   (point at tagged read + replace); tag present. Existing omit-tag+append path stays
   valid. Schema ~120 chars.
3. **`tilth_list` patternless overview.** A patternless call switches from today's
   full-tree rollup to the `overview.rs::fingerprint()`-style project overview (dirs, hot
   files, manifests, git line — same format as the injected `[tilth]` header) scoped to
   `cwd`. Description gains "omit `patterns` for a project overview" (~+35 chars).
4. **Surface-budget reconciliation.** Total `--mcp --edit` surface (initialize
   instructions + tools/list JSON) ≤ 13,779 chars, measured by piping
   initialize+tools/list JSON-RPC into the built binary. Adds funded by the in-flight
   codex-PR trims; overflow → trim deeper (tilth_write schema property descriptions,
   description compression). The cap never yields; the overview never yields. Update
   byte-lock tests; regenerate AGENTS.md via `./scripts/regen-agents-md.sh`.

## Decisions

- Substring (2b) over whole-line (2a) matching — trained-shape alignment is the goal; both
  fail closed (ADR-001).
- Prune reversed on production evidence — benchmark zero-use measured task shape, not op
  value (ADR-002).
- Hard 13,779 cap, trim-deeper overflow rule, overview never yields (ADR-003).
- `replace_text` uniqueness required; no `replace_all` today (follow-up F003).
- _Minor decisions:_ op census computed inline via jq/DuckDB rather than sub-agent spawns;
  codex PR isolated in a scratchpad worktree; `delete_file`/`move_file` never in any prune
  list; haiku-first validation with sonnet5 as regression check.

## Acceptance

- WHEN a tilth_write section with a valid tag contains `{"op": "replace_text", "old": O,
  "new": N}` and O occurs exactly once in the tagged snapshot THE SYSTEM SHALL replace it
  via the lowered `LineOp::Swap` and report the section as applied.
- WHEN O occurs zero times THE SYSTEM SHALL reject only that section with `TextUnmatched`
  and a re-read hint.
- WHEN O occurs more than once THE SYSTEM SHALL reject only that section with
  `TextAmbiguous { count }` and an add-context hint.
- WHEN a `replace_text` op appears in a tag-less section THE SYSTEM SHALL reject the section.
- WHEN a tag-less section contains `{"op": "create_file", "content": C}` and the file does
  not exist THE SYSTEM SHALL create it with content C.
- WHEN `create_file` targets an existing file, or appears in a tagged section, THE SYSTEM
  SHALL reject the section with an error naming the correct path (tagged read + replace).
- WHEN `tilth_list` is called without `patterns` THE SYSTEM SHALL return the
  fingerprint-format project overview scoped to `cwd` instead of the full tree.
- WHEN the release binary serves `--mcp --edit` THE SYSTEM SHALL present a combined
  initialize+tools/list surface ≤ 13,779 chars.
- [prose-fallback] Existing edit-machinery tests (seen-lines gate, overlap, recovery,
  byte-locks after baseline refresh) all pass unmodified except refreshed baselines.

## Interface sketches

```
// wire
{"op": "replace_text", "old": "lines.push(l);", "new": "lines.push(l as u32);"}
{"op": "create_file", "content": "..."}            // tag-less section only
tilth_list(cwd)                                     // no patterns → overview

// src/edit/parser.rs
Op::TextSwap { old: String, new: String }

// src/edit/apply.rs
fn resolve_text_swap(text: &str, old: &str, new: &str)
    -> Result<(u32, u32, Vec<String>), ApplyError>
ApplyError::TextUnmatched { preview: String }
ApplyError::TextAmbiguous { count: usize }
// create_file joins FileOp alongside Rem/Mv

// schema (serialized, compact)
{"required":["op","old","new"],"additionalProperties":false,"properties":{"op":{"const":"replace_text"},"old":{"type":"string"},"new":{"type":"string"}}}
```

## Risks

- July census transferability: strong SWAP-dominance evidence comes from the blob grammar
  era (n=132); JSON-grammar n is small. Mitigated by F001 telemetry.
- Budget math depends on the in-flight codex-PR trims landing; if they don't, the
  trim-deeper pass grows.
- Line-op atrophy post-replace_text is unmeasured (F001 measures it).
- `resolve_text_swap` substring semantics must handle multi-line `old` spanning partial
  first/last lines — covered by required unit tests.

## Open questions

- [TBD] Whether the codex PR (instruction ban repositioning + trims) lands before cook —
  wave 4 assumes its trims; if absent, wave 4 trims deeper.

## Quality gates

- `cargo fmt --check`: clean
- `cargo clippy -- -D warnings`: clean
- `cargo test`: green (incl. new tests named in Curds)
- surface measurement script: ≤ 13,779 chars
- `./scripts/regen-agents-md.sh && git diff --exit-code AGENTS.md`: clean after regen commit

## Curds

curds:
  - id: replace-text-op
    title: Add replace_text substring-swap op (parser + apply + schema)
    files: [src/edit/parser.rs, src/edit/apply.rs, src/mcp/tools/definitions.rs]
    depends_on: []
    verify: "cargo test -p tilth resolve_text_swap_unique_match_substitutes resolve_text_swap_zero_matches_errors resolve_text_swap_multi_match_errors resolve_text_swap_mid_line_span parses_replace_text_op tilth_write_schema_includes_replace_text_branch -- --exact"
  - id: create-file-op
    title: Add create_file FileOp sibling to delete_file/move_file (parser + apply + schema)
    files: [src/edit/parser.rs, src/edit/apply.rs, src/mcp/tools/definitions.rs]
    depends_on: [replace-text-op]
    verify: "cargo test -p tilth create_file_new_file_succeeds create_file_existing_file_errors create_file_tag_present_errors tilth_write_schema_includes_create_file_branch -- --exact"
  - id: list-overview
    title: tilth_list patternless call returns project overview instead of full tree
    files: [src/mcp/tools/list.rs, src/overview.rs, src/mcp/tools/definitions.rs]
    depends_on: [create-file-op]
    verify: "cargo test -p tilth omitted_patterns_returns_project_overview -- --exact (replaces omitted_patterns_defaults_to_full_tree); manual: tilth_list(cwd) patternless emits fingerprint-format overview"
  - id: surface-budget-reconciliation
    title: Reconcile total --mcp --edit surface to ≤13,779 chars and refresh byte-lock tests/AGENTS.md
    files: [src/mcp/tools/definitions.rs, src/mcp/mod.rs, prompts/mcp-base.md, prompts/mcp-edit.md, AGENTS.md]
    depends_on: [replace-text-op, create-file-op, list-overview]
    verify: "cargo test -p tilth server_instructions_byte_lock edit_mode_instructions_byte_lock -- --exact (new baselines); surface measurement ≤13779; ./scripts/regen-agents-md.sh && git diff --exit-code AGENTS.md"
  - id: benchmark-validation
    title: Haiku fork-vs-upstream benchmark confirming leakage elimination without cost/correctness regression
    files: [benchmark/tasks/base.py]
    depends_on: [surface-budget-reconciliation]
    verify: "tasks rg_search_dispatch, rg_trait_implementors, gin_servehttp_flow + gin/fastapi multi-edit family, 5 reps, fork vs upstream; native-tool leakage == 0 in fork arm; cost/correct non-regressive"

waves:
  - [replace-text-op]
  - [create-file-op]
  - [list-overview]
  - [surface-budget-reconciliation]
  - [benchmark-validation]

Serialization rationale: src/mcp/tools/definitions.rs is shared by all four implementation
curds (single serde_json::json! literal); parallel edits would silently collide.
