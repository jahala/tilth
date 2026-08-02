# `tilth_read` budget accounting — and why budget guards go vacuous

Two hard-won facts about `src/mcp/tools/read.rs`: where the response-budget
contract actually lives, and why the obvious test for it silently fails to
test anything. Established while curing PR #149 (bare-string `paths` coercion),
which took three passes precisely because the guard looked fine and wasn't.

## Where the budget contract lives

`finalize_response` is the **only** budget gate. Every `Ok` exit of
`tool_read_paths` routes through it, and `session.record_savings(...)` measures
the body *before* anything downstream appends to it.

So anything a wrapper appends to the response **after** `tool_read_paths`
returns escapes both. That is a two-part defect, and it is easy to fix only
half of it:

1. the response overshoots the caller's declared `budget` (advertised in the
   `tilth_read` schema as "Max tokens in response"), and
2. `record_savings` books tokens the caller never actually saved.

PR #149's batching nudge hit both. The fix shrinks the budget passed down to
`tool_read_paths` by the appended text's token cost, and adds that cost back at
`record_savings`.

**There are two `record_savings` call sites, and they are easy to conflate:**

| site | path | reached when |
| --- | --- | --- |
| signature / auto-promotion | `respond_signature` return | `should_auto_signature` is true — **code** file types only |
| general auto-read | the `finalize_response` return at the end | everything else, including any **non-code** file over `TOKEN_THRESHOLD` (markdown, JSON, YAML, TOML) |

A `.rs` test fixture only ever exercises the first. A cure pass fixed that one,
reported both as done, and the second stayed broken until a scoped re-review
caught it. If you touch savings accounting, **write one test per site** and use
a large-markdown fixture for the general path (`tool_read_auto_large_markdown_returns_outline`
is the shape reference).

## Why a `<= budget` assertion cannot catch a budget regression

`truncate` flat-reserves **50 header tokens**. The actual header is shorter than
that, and by an amount that depends on the absolute path length — so a read
always lands some tokens *under* the declared cap, with the gap varying by
tempdir path length.

For a short path (Linux CI, `/tmp/.tmpXXXXXX/…`) that structural slack exceeds
25 tokens, which is more than enough for an unbudgeted note to hide in. The
assertion passes on broken code. On macOS the same test fails, because
`/var/folders/…` paths are ~69 chars and eat the slack.

**Net effect: the guard bites only on the developer's laptop and is vacuous on
the CI that enforces it.** That is the worst possible arrangement, and it looks
like a passing test.

## Why the obvious differential is also wrong

The natural repair is a path-length-invariant differential — compare the
coerced read against the equivalent array-shaped read. Two traps:

- **Do not subtract the appended text's tokens from the coerced side.**
  `estimate_tokens` is `div_ceil(len, 4)` and therefore subadditive, so
  `coerced − note ≤ array` holds *identically* on the unfixed code. This
  cancels the very defect under test. It was tried in PR #149 and verified
  vacuous by reverting the fix and watching the assertion still pass.
- **Do not compare raw token counts either.** The shrink reserves 25 tokens
  (100 bytes) for a 99-byte note, so exactly **1 byte** of margin separates the
  two counts — and truncation rounds to line boundaries, which swamps it in
  both directions. `coerced <= array` fails post-fix by one token on a
  perfectly correct implementation.

## What actually works: assert the contract exactly

Byte equality has no margin to lose, is path-length independent, and fails on
the unfixed code on every platform:

```rust
// a coerced read at `budget` IS the array read at `budget - note_tokens`,
// with the note appended
assert_eq!(out, format!("{array_out}{note}"));
```

Keep the plain `<= budget` assertion alongside it as a cheap guard, but never
let it carry the test alone.

**Verification rule this episode earned:** a new budget/accounting guard is not
done when it passes. Revert the fix and confirm the test *fails* — and when the
guard has a platform-dependent component, neutralise that component first so
you are proving the assertion that CI will actually rely on.

## Known local-only test failure

`mcp::tests::batch_budget_represents_every_query` (`src/mcp/mod.rs`) **fails on
macOS and passes on CI.** Same root cause as above: long `/var/folders/…`
tempdir paths push a `tool_search` batch-budget assertion over its cap. It is
unrelated to whatever you are working on.

Confirm before chasing it: `git stash push -u` → `cargo test` → `git stash pop`.
If it fails identically with your changes stashed, it is baseline and not yours.

## Gate note

The CI clippy gate is `cargo clippy -- -D warnings` (see `.github/workflows/ci.yml`),
**not** `--all-targets`. `cargo clippy --all-targets -- -D warnings` reports a
pre-existing unused-variable warning in `src/mcp/tools/grok.rs` that CI does not
enforce — do not "fix" it under the impression the branch is red.
