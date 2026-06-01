---
slug: grok-qualifier-disambiguation
source: mold-handshake
intent: Make grok's qualified-target resolution honor the qualifier so `Alpha::dispatch` resolves to the `dispatch` owned by `Alpha`, not whichever same-named definition ranks first.
blast_radius: medium
inputs: A grok target spec carrying a qualifier — `Type::method`, `Type.method`, or `a::b::method` — where the trailing segment matches multiple definitions across different owners.
outputs: The definition whose owning type/container matches the qualifier; or a NotFound with a suggestion naming the real owners when none match.
verification: `cargo test` — new owner-resolution tests pass (including TypeScript class-method and `a::b::method` nested-module cases), the strengthened ambiguity test pins the resolved owner, and a new `symbol.rs` test confirms deeply-nested definitions are detected; existing symbol-search tests still pass (no ranking regression); `cargo clippy -- -D warnings`; `cargo fmt --check`.
---

## Contract

When grok resolves a qualified target (`Type::method`, `Type.method`, or `a::b::method`) by stripping to its trailing segment, it must use the qualifier to select *which* same-named definition to return — the one whose owning type/container matches the qualifier — instead of unconditionally taking the top-ranked definition.

Today `resolve_by_name` (`src/search/grok.rs:86-102`) strips a qualified target to its bare trailing segment (`name.rsplit([':', '.']).next()`) and calls `resolve_def_by_query`, which returns `definitions.first()` (`grok.rs:107-122`). The qualifier (`Alpha`) is discarded, so with both `Alpha::dispatch` and `Beta::dispatch` in scope the result depends on ranking order, not the qualifier. PR #61's ambiguity test (`resolve_qualified_target_ambiguous_segment_reports_other_defs`, `grok.rs:1044`) asserts only `other_def_count == 1` and never *which* owner resolved, so it passes regardless of the bug.

The fix derives each candidate definition's owner from the tree-sitter outline (or, for Go, from the method receiver) at resolve time, filters candidates to those whose owner matches the qualifier, and applies the decision tree below. The owner-resolution logic lives inside `src/search/grok.rs` (plus one small Go receiver-type query helper that may reuse `src/search/siblings.rs`'s `with_query` infrastructure). `Match` and `src/types.rs` are not touched — the owner is derived from the outline at resolve time, keeping the blast radius off the 31-importer `types.rs` hot file.

### Scope expansion (v2 — added after cook-phase halt)

The original spec fenced the change to `grok.rs` and listed "Changing `search_symbol_raw` behavior" as a non-goal. During implementation that fence proved incompatible with two acceptance criteria: TypeScript class methods (AST depth 4 under `export`) and 2-level nested-module functions (`a::b::method`, depth 5) are classified as **usages, not definitions** by `walk_for_definitions` in `src/search/symbol.rs`, because of an arbitrary `depth > 3` recursion cap (`symbol.rs:335`, present since the initial commit, no rationale comment). Capped-out definitions never enter grok's candidate set, so there is nothing for grok to owner-match. By explicit user decision the scope is widened to fix that cap so deeply-nested definitions become discoverable. This change affects every symbol search, not just grok — the accepted blast radius. Only the recursion **depth bound** changes; the ranking weights and candidate-selection algorithm are untouched.

### Scope boundary

In scope: qualifier-aware resolution for the languages whose outline nests methods under a named container (Rust, TypeScript/TSX, JavaScript, Python, Java, C++, Ruby, and the other class-nesting languages) **plus Go** via a new receiver-type finder; **plus removing the `depth > 3` cap in `walk_for_definitions` (`src/search/symbol.rs`)** so deeply-nested method definitions are detected as definitions (v2 scope expansion). Out of scope: unqualified (bare) resolution, which is unchanged; Elixir `defimpl` owner-matching (its outline parent is the protocol, not the implementing type, so it degrades to the no-owner-match path); any change to `Match`/`types.rs`; any change to ranking weights or the candidate-selection algorithm in `search_symbol_raw` (only the recursion depth bound moves).

## Grounding (settled during /mold)

- **Rust impl parent name is the concrete type, prefixed.** `node_to_entry` emits an `impl_item` parent with `name = format!("impl {type}")` from the `"type"` field (`src/lang/outline.rs:188-189`). For `impl Trait for Foo { fn m }` the parent is `"impl Foo"` (the `"trait"` field is ignored). So owner-matching against a `Foo` qualifier requires stripping the leading `impl` prefix and any `<…>` generics, then comparing. `<certain>`
- **TS / Python / Java / C++ / Ruby** nest methods under the bare class/namespace name (`find_child_text(node, "name", …)`, `src/lang/outline.rs:154-159`). No prefix to strip. `<certain>`
- **Go does not nest** — `func (f *Foo) Bar()` is a flat top-level entry named `"Bar"`; the receiver type lives in the AST, not the outline tree. The existing `extract_go_receiver_name` (`src/search/siblings.rs:218-245`) is **not reusable**: it captures the receiver *variable name* (`f`) via the `name:` field, not the *type* (`Foo`), and only reads the first method in the file. Go therefore needs a new per-method receiver-*type* query. `<certain>`
- **Elixir `defimpl Protocol, for: Type`** names the outline parent for the protocol, not the implementing type (`src/lang/treesitter.rs:239-251`), so `Type::method` cannot owner-match; it degrades to the no-owner-match path. `<certain>`
- **No ancestor/parent finder exists.** `find_entry_at_line` and `find_by_start_line` (`grok.rs:200-224`) return only the leaf entry. `collect_siblings` (`grok.rs:762-794`) finds a parent but only one level deep over top-level entries — a recursive parent-name finder must be added. `<certain>`
- **`walk_for_definitions` caps recursion at `depth > 3`** (`src/search/symbol.rs:335`), incrementing `depth` once per AST level from the root (depth 0). A Python class method (`module → class_definition → block → function_definition`) and a Rust impl method land at depth 3 and are detected — which is why those criteria already pass. A TypeScript `export class` method (`program → export_statement → class_declaration → class_body → method_definition`) lands at depth 4, and a doubly-nested Rust module fn at depth 5 — both exceed the cap, so `search_symbol_raw` returns them as usages, not definitions. The cap is arbitrary (initial-commit, no rationale) and removing it is the v2 fix. `<certain>` (git blame + cook-phase CLI verification)

## Design

All new/changed symbols live in `src/search/grok.rs` unless noted.

- `fn split_qualified(name: &str) -> (Option<&str>, &str)` — split on the last `::`/`.` separator into (immediate-owner qualifier, bare name). `"Alpha::dispatch"` → `(Some("Alpha"), "dispatch")`; `"a::b::method"` → `(Some("b"), "method")` (the immediate owner is the **last** segment of the prefix); `"dispatch"` → `(None, "dispatch")`.
- `fn find_parent_name(entries: &[OutlineEntry], start_line: u32) -> Option<&str>` — recursive walk of the outline tree returning the `name` of the entry whose `children` contain an entry at `start_line`; `None` when the target is top-level (free function, Go method) or not found.
- `fn owner_matches(container_name: &str, qualifier: &str) -> bool` — normalize the container name (strip a leading `impl` prefix, strip any `<…>` generic arguments, take the trailing `::`/`.` segment) and compare case-sensitively to `qualifier`.
- **New Go receiver-type helper** — a tree-sitter query over the candidate's file capturing the `method_declaration` receiver `type:` field (handling `(pointer_type (type_identifier))` → strip the `*`), scoped to the method whose node starts at the candidate's `def_range.start`. May live in `grok.rs` or a small helper reusing `siblings.rs`'s `with_query`.
- `fn owner_of_match(m: &Match) -> Option<String>` (or equivalent) — derive a candidate's owner by language: nesting languages → read outline + `find_parent_name`; Go → receiver-type helper; others → `None`. Uses `OutlineCache` so repeated parses of the same file are cheap.
- `fn resolve_def_by_query(query: &str, qualifier: Option<&str>, scope: &Path) -> Result<Option<(ResolvedTarget, String, Lang)>, TilthError>` — reworked to accept and apply the qualifier. `resolve_by_name` passes `None` on the literal full-name attempt and `Some(qualifier)` on the bare-segment retry.
- **`walk_for_definitions` depth cap (`src/search/symbol.rs`, v2 expansion)** — remove the `depth > 3` early return so the walk descends to definitions at any realistic nesting depth (export-wrapped classes, nested modules, namespace-nested classes). Keep a high pathological-recursion guard only if needed to bound stack depth (e.g. `depth > 64`); do not reintroduce a low cap that re-hides real definitions. Nothing else in `walk_for_definitions` or `search_symbol_raw` changes — same `DEFINITION_KINDS` filter, same `Match` shape, same ranking. The behavioral requirement is that a method/function definition is detected as `is_definition: true` regardless of how deeply it nests.

### Decision tree (qualifier present)

1. Gather all definitions named `bare` (as today).
2. Derive each candidate's owner via `owner_of_match`; partition into owner-matched vs not.
3. **Exactly one owner-match** → resolve it; `other_def_count = 0`.
4. **Multiple owner-matches** (genuine same-owner overloads, e.g. `impl Foo` and `impl<T> Foo<T>` both with the method) → resolve the top owner-match; `other_def_count = owner_matched_count − 1`. This preserves PR #61's labeled best-effort for true ambiguity.
5. **Zero owner-matches** → return `TilthError::NotFound` with `suggestion` populated naming where the bare method actually lives (e.g. `no 'dispatch' owned by 'Alpha'; found on Beta (src/beta.rs:4)`). Never return a misleading body for an owner the caller did not ask for.

Qualifier absent → unchanged: `definitions.first()` with `other_def_count = definitions.len() − 1`.

### Rationale for the zero-match behavior

The consumer is an AI agent. Silently returning a different owner's method body (plain best-effort) reintroduces the exact silent-wrong-owner failure this fix exists to remove — worse than the original bug, because in the zero-match case *no* candidate is the requested owner. A `NotFound` carrying a suggestion that names the real owners is strictly more informative than a bare 404, never feeds a misleading body, and costs one cheap re-query. `TilthError::NotFound` already carries `suggestion: Option<String>`.

### Rejected alternative

Returning a bare ambiguity error whenever a qualifier is present and `other_def_count > 0` (~10 lines) was rejected: it *degrades* the feature by 404-ing the multi-match case PR #61 deliberately made best-effort via `other_def_count`, and it does not actually honor the qualifier — it just refuses to choose.

## Acceptance

- `Alpha::dispatch` resolves to Alpha's `dispatch` and `Beta::dispatch` to Beta's, given both defined in separate Rust files (the case the current suite cannot make fail).
- Trait-impl method: `impl Trait for Foo { fn m }`, target `Foo::m` (and `Foo.m`) resolves to `m`.
- Nested-module qualifier `a::b::method` resolves to the `method` whose immediate parent is `b` (depth-5 definition, now discoverable after the v2 cap removal — not `#[ignore]`'d).
- TypeScript/TSX and Python class-method qualifiers resolve to the method owned by the named class (the TS `export class` method is a depth-4 definition, now discoverable after the v2 cap removal — not `#[ignore]`'d).
- **Depth-cap regression (v2):** a direct `symbol.rs` test asserts a deeply-nested definition (e.g. a TS `export class` method, or a doubly-nested module fn) is returned with `is_definition: true` — pinning the cap fix at its source so a future re-cap regresses loudly.
- **Go**: `Foo.Bar` resolves to the `Bar` whose receiver type is `Foo`, not another type's `Bar`.
- Generic impl `impl<T> Foo<T> { fn m }`, target `Foo::m` resolves (generic args stripped during normalization).
- Zero owner-match (e.g. `Alpha::dispatch` when no `dispatch` is owned by `Alpha`) returns `NotFound` with a `suggestion` naming the real owner(s) and location(s) — not a body.
- `resolve_qualified_target_still_404s_when_method_absent` (`grok.rs:1029`) still passes (absent bare method still 404s).
- The existing `resolve_qualified_target_ambiguous_segment_reports_other_defs` (`grok.rs:1044`) is **strengthened** to assert the resolved target's owner is Alpha (e.g. by pinning the resolved file/path or owner), not merely `other_def_count == 1`.
- `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass.

## Non-goals

- Bare (unqualified) resolution — unchanged.
- Elixir `defimpl` owner-matching — degrades to the no-owner-match path (documented limitation).
- Widening `Match` or editing `src/types.rs` — owner is derived from the outline/AST at resolve time.
- Changing ranking weights or the candidate-selection algorithm in `search_symbol_raw`. The v2 expansion moves only the recursion **depth bound** in `walk_for_definitions`; it does not touch how candidates are scored or ordered.

## Open follow-ups (out of this spec)

- Elixir `defimpl` owner-matching, if ever wanted, needs the protocol-vs-type distinction surfaced in the outline.
- File a GitHub tracking issue for the enhancement if desired (none exists yet).

## Provenance

- Deferred from PR #61 (`paulnsorensen/pasteurize-59`); finding graded and pushed back during `/affinage 61`. See `.cheese/affinage/pr-61.md` and the handoff note `.cheese/notes/grok-qualifier-disambiguation.md`.
- Posted push-back: <https://github.com/paulnsorensen/tilth/pull/61#discussion_r3331592189>
- /mold grounding: explorer agent characterized outline parent naming across languages and the finders; user confirmed (1) Go in v1, (2) zero owner-match → 404-with-suggestion.
- **v2 scope expansion (`/ultracook` cook-phase halt):** the cook agent halted with 6/8 criteria passing — TS class methods and `a::b::method` were blocked by the out-of-scope `depth > 3` cap in `src/search/symbol.rs`. User chose to expand scope (option b) to remove the cap and reach 8/8, accepting the wider blast radius across all symbol search. The cook handoff was cleared and the `/ultracook` chain re-run from cook against this amended spec.
