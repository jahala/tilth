# Security Policy

## Reporting a vulnerability

Please **don't** open a public issue. Use GitHub's private advisory flow:

→ <https://github.com/jahala/tilth/security/advisories/new>

We'll acknowledge within 72 hours and coordinate disclosure with you.

## Supported versions

Only the latest minor release receives security updates. Older versions don't.

## Security testing

tilth runs:

- **Unit and integration tests** on every push (`cargo test`).
- **CodeQL** static analysis on Rust, Python, and JavaScript on every push.
- **OpenSSF Scorecard** weekly, results uploaded to the Code scanning view.
- **cargo-fuzz** nightly across three input surfaces — `outline` (tree-sitter outline rendering across 18 languages), `strip` (comment / debug-log stripping), and `diff_parse` (unified diff parser). See `.github/workflows/fuzz.yml`. Crash artifacts land in the workflow run on failure; corpus is cached across runs.

Reproducing a fuzz finding locally:

```bash
rustup install nightly
cargo install cargo-fuzz
cargo +nightly fuzz run <target> -- -max_total_time=300
# To reproduce a specific crash artifact:
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<crash-file>
```

Adding a new fuzz target is a 3-step recipe — see `fuzz/fuzz_targets/outline.rs` for the pattern.
