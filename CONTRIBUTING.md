# Contributing

Thanks for your interest. tilth is small and intentionally so — clean, focused changes are easiest to land.

## Workflow

1. Fork, branch, change.
2. Run the gates locally:
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   ```
3. Open a PR. Describe what changed and how to test it.

CI runs the same three commands on every push.

## What helps

- **Small PRs.** Easier to review, easier to merge.
- **Conventional commits.** `fix: ...`, `feat: ...`, `refactor: ...`, `docs: ...`. The log is the changelog.
- **A test.** Bug fixes need a regression test. Features need at least one.
- **Surgical edits.** Don't rewrite surrounding code unrelated to the change.

## Code style

See [CLAUDE.md](./CLAUDE.md) for the project layout and conventions. Match the style of the file you're editing.

## Bigger changes

For anything that adds an MCP tool, changes a tool schema, or restructures a module: open an issue first so we can agree on the shape before you spend time on the implementation.

## License

By contributing, you agree your work is licensed under the project's [MIT License](./LICENSE).
