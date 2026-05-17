# Fuzz seed corpus (tracked)

Real findings preserved as regression inputs. Unlike `fuzz/corpus/` (the
working corpus, gitignored and cached across CI runs), files here are
checked in so they survive across machines.

Add a new seed when triaging a fuzz finding: copy the artifact here with
a descriptive name, and document below what it triggers.

## Inventory

### outline/

- **`oom-rust-imports-9kb`** — 9 KB Rust-shaped input (real source from
  `globset/src/serde_impl.rs` in the ripgrep tree). When iterated through
  all 18 supported `Lang` variants, one of them allocates >2 GB in
  tree-sitter's `ts_parser_parse`. Suspected language: C grammar
  confused by Rust syntax (heavy `::` and braces). **Not yet fixed.**
  Tracked as a follow-up task. The OOM is reproducible by:

  ```bash
  cargo +nightly fuzz run outline fuzz/seeds/outline/oom-rust-imports-9kb
  ```

  Mitigation options under consideration:
  1. `tree_sitter::Parser::set_timeout_micros` — bound parser work
  2. Reject inputs larger than N when target lang inference is uncertain
  3. Skip languages whose extension doesn't match the input shape
