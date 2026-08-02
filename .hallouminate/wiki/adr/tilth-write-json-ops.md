# ADRs — tilth_write JSON-native ops (slug: tilth-write-json-ops)

Session 2026-07-05; spec at the durable corpus (`paulnsorensen-tilth/specs/tilth-write-json-ops.md`). Fumble evidence: `.cheese/notes/roots-worktrees-wozcode-test.md` lines 55-57.

### ADR-001: Replace the tilth_write string op-grammar with JSON-native ops [status: accepted]

- **Context:** Every logged haiku benchmark fumble on tilth_write was grammar/encoding (bespoke op mini-language inside a JSON string: zero pretraining prior, double-encoding traps, no API-layer validation), not wrong-line targeting. Three options were on the table: (1) JSON-native ops keeping the tag, (2) fuzzy old_string anchor as an additional op type, (3) teaching errors on the existing blob.
- **Decision:** Option 1 now, with option 3's error style as the transition bridge. The whole-file-tag drift model (tag gate, seen-lines, 3-way merge recovery) is kept unchanged — only the encoding dies.
- **Alternatives:** Teaching-errors-only (option 3) lowers fumble *cost* (fewer retries) but cannot prevent the first failed call; the benchmark-ladder sequencing (baseline → option 3 → re-bench → maybe option 1) was rejected as slower with the architecture already believed unsalvageable for weak models. The fork-surface objection dissolved: the whole-file-tag edit model is already a permanent keep-ours fork feature, so this reshapes an already-divergent surface (no env-var gate needed, nothing to propose upstream — upstream lacks the edit model entirely).
- **Consequences:** Breaking, user-visible schema change; tool description, prompts/mcp-edit.md, AGENTS.md, byte-lock tests, and the external cheez-write skill text all need updating. Gated on the haiku edit benchmark: baseline recorded before merge, post-change run must show fewer failed-call turns and >= pass rate.

### ADR-002: 1:1 op vocabulary with pretrained names [status: accepted]

- **Context:** The JSON shape needed a verb set. The parsed `Op` enum (`src/edit/parser.rs:58-82`) is grammar-independent, so any wire vocabulary lowers into it.
- **Decision:** Mirror the existing 11 verbs' field shapes 1:1 but rename to pretrained-friendly words: replace, delete, insert_before, insert_after, prepend, append, replace_block, delete_block, insert_after_block, delete_file, move_file. Block ops take a single `at` anchor (integer line or `"#symbol"`).
- **Alternatives:** (a) Keep fork-invented names (swap, ins.pre, …) — lowest doc diff but carries the zero-prior vocabulary into the new schema. (b) Collapse to 3 verbs (replace/insert/delete) with orthogonal fields — least vocabulary, but legality moves into cross-field rules, exactly where weak models generate invalid combos. (c) A separate `replace_symbol` verb — rejected by the user as redundant with `replace_block`'s `at` anchor, contingent on the MCP instructions explaining both anchor modes clearly.
- **Consequences:** Trivial lowering onto the existing apply layer (per-verb fields stay flat); 11 enum values remain in the schema, so the tool-definition token cost must be watched.

### ADR-003: Clean cutover with translating teaching errors, no back-compat alias [status: accepted]

- **Context:** The old string blob could have stayed parseable as a hidden alias during transition.
- **Decision:** Clean cutover. `edits` accepts only the JSON array. A legacy `[path#TAG]` blob arriving as a string is rejected with an error containing the exact JSON translation of the submitted ops — rendered via `parse_sections`, which is removed from the input path but retained internally, error-path-only. A double-encoded JSON string is rejected naming the double-encoding and showing the unwrapped form.
- **Alternatives:** Hidden alias (two parsers forever, ambiguous error paths, muddied benchmark attribution) — rejected; this is a single-user fork tool whose known callers are the user's own skill texts, which ship updated alongside.
- **Consequences:** One advertised grammar; stale-context regurgitation is handled by the teaching error rather than silent acceptance; `parse_sections` survives with a narrower, clearly-marked role.
