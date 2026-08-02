# Diff: symbol output order is not deterministic

**Open bug.** Two `diff()` calls over the same input, in the same process, can
emit their symbol lines in different orders. If you write a diff test that
compares formatted output as one string and it flakes, this is why — the test
is fine, the ordering is not.

## Cause

`match_symbols` in `src/diff/matching.rs` iterates its identity map directly:

```rust
for (id, old_indices) in &old_by_id {
```

`old_by_id` is a `HashMap<SymbolIdentity, Vec<usize>>`, and Rust's default
hasher is randomly seeded per process. Iteration order therefore varies, which
varies the push order into `symbol_changes`, which varies the line order out of
`format_file_detail` and `format_overview`.

## Working around it in tests

Split the output into lines and sort both sides before comparing.
`test_absolute_file_scope_normalizes_to_repo_relative` in `src/diff/mod.rs`
does exactly this and carries a comment pointing back at the cause.

Be aware of what the workaround costs: a sorted comparison also cannot detect a
genuine ordering difference between the two things under test. Sort only when
order is not what the test is about.

## Fixing it

Not yet attempted. The fix is confined to `matching.rs` — iterate a sorted view
of the keys, or switch `old_by_id` to an order-preserving map — but it will
churn the expected output of any test that asserts on line order, so it wants
its own change rather than riding along on unrelated work.

Discovered during the PR #147 review; deliberately left unfixed there because
that PR was about scope matching, not symbol matching.

## Related

- [Diff: git ref resolution and exit-code handling](diff-git-ref-resolution.md)
