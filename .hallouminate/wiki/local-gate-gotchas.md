# Local gate gotchas (macOS)

Running the CI gates locally on macOS does **not** reproduce CI's result. Both
divergences below are pre-existing baseline, not something you broke — verify
against `origin/main` before "fixing" either.

## `cargo test` fails one test on macOS

`mcp::tests::batch_budget_represents_every_query` (`src/mcp/mod.rs`) panics with
`expected truncation marker` on macOS. It passes in CI (ubuntu).

Verified 2026-08-01 by running the single test in a scratch worktree checked out
at a clean `origin/main`: **842 passed / 1 failed** there, and the same single
failure on the feature branch. The test body was unchanged between the two.

So the local baseline is `N passed / 1 failed`. A PR body claiming
`cargo test` → `0 failed` was written from CI output, not a local run. When a
handoff records a baseline, this is the failure it means.

## CI runs plain `cargo clippy`, not `--all-targets`

`.github/workflows/ci.yml`'s `check` job runs exactly:

```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

`cargo clippy -- -D warnings` is **clean**. `cargo clippy --all-targets -- -D warnings`
is **not** — it surfaces `unused variable: full` at `src/mcp/tools/grok.rs:82`,
a test-only path CI never lints. Reaching for `--all-targets` out of habit
produces a red gate that CI does not have, and reads as "this PR broke clippy."

Use the three commands above verbatim when checking whether a change is
shippable.

## Why this page exists

Three independent review agents on PR #144 each rediscovered both facts and
each had to re-verify them against `origin/main` before concluding they were
baseline. That is the rederivation this page exists to prevent.
