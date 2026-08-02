# ADRs — tilth_write teaching errors (slug: tilth-write-teaching-errors)

Session 2026-08-02; spec at the durable corpus
(`paulnsorensen-tilth/specs/tilth-write-teaching-errors.md`). Evidence:
[[../usage-analytics-2026-07]] (July 2026 cross-harness analytics).

### ADR-001: Teaching errors only, no serde aliases [status: accepted]

- **Context:** ~74% of recent tilth_write errors are invented field/op names
  from Claude-family models (`find`, `anchor`, `text`, `seed`, …). Aliasing
  the pure renames (`text`→`content`, `new`→`content`) would prevent ~7 of
  ~48 recent errors outright.
- **Decision:** No aliases. Every wrong name errors, but the error carries
  the corrected example. Keeps tilth-write-json-ops ADR-003's
  one-advertised-grammar rule; schema stays byte-identical; benchmark
  attribution stays clean.
- **Alternatives:** Pure-rename aliases (two grammars in practice, hidden
  from the schema); aggressive op-name coercion (rejected — `insert` is
  ambiguous between before/after, and a wrong silent mapping corrupts an
  edit instead of erroring).
- **Consequences:** The first failed call still happens; its cost drops to
  one teaching round-trip. Evidence check: 330 codex/omp calls produced zero
  guesses, so aliases would only absorb behavior the teaching error corrects
  per-session anyway.

### ADR-002: One generic wrapper, not per-class messages [status: accepted]

- **Context:** Error construction could be per-failure-mode handcrafted text
  (unknown field vs unknown variant vs missing op), a generic
  append-example wrapper, or a hybrid with a `find` special case.
- **Decision:** Generic wrapper at the single op-deserialize interception
  point in `src/edit/json.rs` `lower_section`: append one canonical example
  op plus the line-addressed sentence to every op parse failure.
- **Alternatives:** Per-class arms (max teaching value, more maintenance,
  new guess patterns fall through raw until added); hybrid `find`
  special-case (rejected with it — the generic sentence already names the
  find/replace misconception).
- **Consequences:** ~10 lines; covers unknown future guesses; one test
  surface.

### ADR-003: Silent leading-`#` strip on tags [status: accepted]

- **Context:** Observed `tag: "#A0EA"` — an agent copying the tag with the
  display prefix from the `[path#TAG]` read format. The tag is the drift
  gate, where silent normalization has the worst downside if the assumption
  is wrong.
- **Decision:** Strip one leading `#` before `parse_tag`; the 4-hex gate
  runs unchanged on the stripped value. This is parsing display syntax, not
  guessing intent — a wrong tag still fails.
- **Alternatives:** Teaching error without strip (zero silent normalization
  near the gate, cost of one retry) — viable, rejected for a prefix that is
  unambiguous by construction.
- **Consequences:** Strip lives at the lowering site (`json.rs`), keeping
  `parse_tag`/`tag.rs` strict.
