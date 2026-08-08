# Multi-agent workflow notes (fan-out builds, 2026-08-08)

Operational lessons from fanning out PRs #169, #170, #171 to codex
gpt-5.6-sol workers in worktrees under `/home/paul/Dev/tilth-wt/`, each then
adversarially reviewed by an opus-tier agent. Source:
`.cheese/notes/tilth-pr196-sonnet5-audit.md`.

## Worktree workers can't commit — the orchestrator must

`<certain>` A codex worker running inside a git worktree cannot `git commit`
in that worktree: the worktree's `.git` file is a pointer (`gitdir: ...`)
into the main checkout's `.git/worktrees/<name>/`, which lives *outside* the
worker's sandboxed filesystem view. Commits have to happen from the
orchestrating session, not the worker.

## Adversarial review caught real defects on every PR

`<certain>` Every one of the three fanned-out PRs had at least one genuine,
reproduced defect caught by its opus review pass before merge:

- **PR #171** (surface shrink): review caught an MD032 CI blocker, a dropped
  `N:`-prefix guard (a silent file-corruption vector — must be restored, not
  optional), and lost semantics (kind grammar, `#n` gloss, `next_view`
  semantics, anti-patterns section) that the trim had cut along with genuine
  bloat. All restored, funded by additional semantics-free trims elsewhere.
- **PR #170** (batch nudge): review caught self-batch tips firing on
  repeated items (burning the 2-emission budget on redundant advice), a seam
  test that didn't actually cover the seam, errored dispatches preserving a
  broken streak state, and a stray double blank line. All fixed; 906 tests
  green after.
- **PR #169** (batch metric): review caught opposite-direction share biases
  in the new Batching table — dropped coerced singletons in one direction,
  non-batchable tools inflating the denominator in the other. Fixed with an
  explicit batchable-tool allowlist; 115 tests green after.

Takeaway: treat a single-pass fan-out build as a draft, not a mergeable
result — budget for the adversarial review round as part of the workflow,
not as optional polish.

## Pre-existing debt surfaced, not introduced

`<certain>` A reviewer noted `cargo clippy --all-targets -- -D warnings`
fails at `main`'s merge-base (`src/mcp/tools/grok.rs:116` unused var,
`src/mcp/tools/read.rs:1350` doc lint) — this is baseline, not something the
fanned-out PRs broke. See
[Local gate gotchas](local-gate-gotchas.md) for the general pattern: CI's real
gate is plain `cargo clippy -- -D warnings`, which stays clean.

## Byte-lock tests constrain prompt file formatting

`<certain>` `src/mcp/mod.rs` embeds `prompts/mcp-base.md` and
`prompts/mcp-edit.md` via `include_str!`, and byte-lock tests
(`server_instructions_byte_lock`, `edit_mode_instructions_byte_lock`) assert
their exact byte length. Prompt files must not carry a trailing newline (or
any other stray byte) that the byte-lock test doesn't expect — every fan-out
worker touching these files had to update the byte-lock constants alongside
the content change, not just the content.

## Squash-merging a stacked PR's base needs `rebase --onto` for the child

`<certain>` PR #169 was stacked on PR #168 (`base: benchmark/sonnet5-failure-taxonomy`).
When a stacked PR's base branch is squash-merged (as #168 was, into
`c29b3e8`), the child branch's history still contains the base's original
unsquashed commits. Rebasing the child cleanly onto the new squashed base
requires `git rebase --onto <new-base> <old-base> <child-branch>`, not a
plain rebase — a plain rebase replays the now-duplicate base commits and
produces spurious conflicts.

## Related

- `.cheese/notes/tilth-pr196-sonnet5-audit.md`
- [Local gate gotchas](local-gate-gotchas.md)
- [Tool-call batching](tool-batching-behavior.md)
- [MCP instructions & tool descriptions](mcp-instructions-limits-and-format.md)
