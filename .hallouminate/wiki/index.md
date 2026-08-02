# tilth wiki — index

This wiki is what an LLM working in the `tilth` repo writes to and reads from
when it wants to remember things across sessions. It lives at
`.hallouminate/wiki/` and is indexed as the `repo:tilth:wiki` corpus, separate
from the source-code corpus (`repo:tilth:corpus`) and the per-session reports
under `.cheese/`.

## Topics

- [Edit-anchor design: per-line hash vs whole-file tag](edit-anchor-design.md) — why tilth originally anchored edits with a per-line content hash, the FNV low-bit-mask bug, the measured ~25% per-read token tax vs oh-my-pi's O(1) whole-file tag, and the analysis behind the since-shipped switch to the whole-file-tag model.
- [Local gate gotchas (macOS)](local-gate-gotchas.md) — why a local `cargo test` shows one failure CI does not (`batch_budget_represents_every_query`), and why CI's `cargo clippy -- -D warnings` is clean while `--all-targets` is not. Both are baseline; check before "fixing" either.
- [MCP cwd / workspace-root binding](mcp-cwd-root-binding.md) — why tilth uses a required per-call `cwd` param (renamed from `root` in PR #113, hook removed in #144; not the MCP `roots` capability) to resolve paths to the right git-worktree checkout; the silent worktree gotcha; 8-harness client survey.

## How to use this index

`index.md` is a table of contents, not a topic. Add new pages to the list
above (alphabetical), keeping a one-line gloss per entry. Anything substantive
belongs in a topic file — one topic per file.

If you read this index and don't see the topic you need, run `list_files`
against the `repo:tilth:wiki` corpus first — the index may be out of date
relative to the directory.
