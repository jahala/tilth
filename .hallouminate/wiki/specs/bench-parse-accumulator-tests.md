# Port upstream benchmark parse fixes + create benchmark/tests/

## Provenance

Upstream-sync round 2, checklist item 1(a)+(b) (`.cheese/notes/upstream-sync-checklist.md`).
Grounded findings this session:

- Upstream `60c1cbd` (jahala/tilth) fixes a result_text overwrite bug in `parse_stream_json` only. Fork has the same bug shape in **all three** parsers: `benchmark/parse.py:102` (`parse_stream_json`), `:158` (`parse_codex_json`), `:320` (`parse_opencode_json`, comment "last assistant text wins"). Briesearch verdict (`.cheese/research/upstream-benchmark-tooling-refactor/`): do NOT adopt upstream's tooling rewrite; re-derive this fix in place.
- Handoff item (b) as originally written is stale: `7e1d09c` (.git restore in `check_correctness`) is already in fork `benchmark/tasks/base.py:132-140` verbatim, and `required_matches()` OR-alternation is fork code (`base.py:28`, from `281d3cc`). What remains: its only tests are assertions in dead script `benchmark/check_stats.py:99-103` — extract them into a real test file.
- Upstream `2643c97` adds 2 lines to `benchmark/README.md` pinning grading semantics (substring over text accumulated across all assistant turns).

## Contract

1. **Accumulator fix, all three parsers** (`benchmark/parse.py`): replace the per-turn overwrite of `result_text` with list accumulation + `"\n".join(...)` at construction, mirroring upstream `60c1cbd`'s shape (`result_text_parts: list[str]`). Apply the same pattern to `parse_stream_json`, `parse_codex_json`, and `parse_opencode_json`. Remove the now-false "last assistant text wins" comment. Public signatures unchanged (`run.py` imports `parse_opencode_json`).
2. **`benchmark/tests/test_parse.py`** (new dir): port upstream's test file (`git show 60c1cbd:benchmark/tests/test_parse.py`, 69 lines), adapting imports to fork layout. Extend with equivalent multi-turn accumulation cases for `parse_codex_json` and `parse_opencode_json` — each test must fail against the pre-fix overwrite behavior (multi-turn input where an early substantive turn is followed by a short wrap-up turn; assert both texts present).
3. **`benchmark/tests/test_required_matches.py`** (new): port the assertions from `check_stats.py:99-103` into pytest tests covering: plain substring hit/miss, `"foo|bar"` alternation hit/miss, whitespace-stripped alternates, empty-alternate handling (leading/trailing/doubled `|` never an unconditional pass, all-empty never matches — per the `required_matches` docstring). Leave `check_stats.py` untouched (disposition is checklist item 2).
4. **README** : port `2643c97`'s 2-line grading-semantics addition to `benchmark/README.md` Methodology section.

## Cut list (out of scope)

- No adoption of upstream's benchmark tooling rewrite (analyze/run/config stay fork).
- No changes to `check_stats.py`, `regrade.py`, `paired.py`, `stats.py`.
- No changes to `benchmark/tasks/` or task definitions.

## Verification

- `python -m pytest benchmark/tests/` green.
- Each new accumulation test demonstrably fails when the fix is reverted (run once against stashed pre-fix parse.py or assert via inspection of the multi-turn case).
- `python -c "from benchmark.parse import parse_stream_json, parse_codex_json, parse_opencode_json"` (or fork-equivalent import path) still works; `run.py` / `analyze.py` imports unaffected.

## Environment

- Branch from `main` (`d0b410d`) in a dedicated worktree; branch name `port/bench-parse-accumulator`.
- Upstream remote `jahala` is fetched in-repo; full checkout at `~/Dev/tilth-jahala`.
- Commit only — no push/PR (orchestrator handles).
