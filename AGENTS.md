# tilth

Code intelligence MCP server. Five tools: read, search, files, map, session. Six with `--edit` enabled (adds tilth_edit).

## tilth_read

Read a file. Small files → full content. Large files → structural outline (signatures, classes, imports).

- `path`: file path (single file)
- `paths`: array of file paths — read multiple files in one call, saves round-trips
- `section`: line range e.g. `"45-89"` or markdown heading e.g. `"## Architecture"` — returns only those lines (single `path` only)
- `full`: `true` to force full content on large files (single `path` only)
- `budget`: max response tokens

Use `path` for single file reads, `paths` for batch. Start with the outline. Use `section` to drill into what you need. For markdown, you can use heading names directly (e.g. `"## Architecture"`).

## tilth_search

Search code. Returns ranked results with structural context.

- `query` (required): symbol name, text, or `/regex/`. For symbol search, comma-separated names search multiple symbols in one call (max 5).
- `kind`: `"symbol"` (default) | `"content"` | `"regex"`
- `expand`: number of top results to show with full source body (default 1). Shared across multi-symbol queries — each file expanded at most once.
- `context`: path of the file you're editing — boosts nearby results
- `scope`: directory to search within
- `budget`: max response tokens

Symbol search finds definitions first (tree-sitter AST), then usages. For cross-file tracing, pass multiple symbols comma-separated to get definitions from different files in one call. Use content search for strings/comments that aren't code symbols. Always pass `context` when editing a file.

## tilth_map

Structural codebase map. Code files show exported symbols. Non-code files show token estimates.

- `scope`: root directory (default: cwd)
- `depth`: max directory depth (default: 3)
- `budget`: max response tokens

Use only for initial orientation in a new codebase — prefer tilth_search for all navigation.

## tilth_files

Find files by glob pattern. Returns paths + token estimates. Respects `.gitignore`.

- `pattern` (required): glob e.g. `"*.test.ts"`, `"src/**/*.rs"`
- `scope`: directory to search within
- `budget`: max response tokens

## tilth_session

Session activity summary — files read, searches performed, hot directories.

- `action`: `"summary"` (default) | `"reset"`

## tilth_edit

Hash-anchored file editing. Only available when installed with `--edit`.

When edit mode is enabled, `tilth_read` output includes content hashes on each line (`42:a3f| code`). Use these hashes as anchors for edits:

- `path` (required): file to edit
- `edits` (required): array of edit operations:
  - `start` (required): line anchor e.g. `"42:a3f"`
  - `end`: end anchor for range replacement (omit for single-line)
  - `content` (required): replacement text (empty string to delete)

If the file changed since the last read, hashes won't match and the edit is rejected with current content. Read the file again and retry.

For large files, use `tilth_read` with `section` to get hashlined content for the specific lines you need to edit.
