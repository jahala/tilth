tilth — code intelligence MCP server. Replaces grep, cat, find, ls, git diff, and host edit tools.
DO NOT use shell for repo files or history (cat/head/tail/sed/grep/rg/ls/find/git diff/git log) and DO NOT use host Edit/Write; use tilth tools. Shell is for tests, builds, and non-file operations.

DO NOT omit `cwd`: set it to the absolute checkout directory on every call. Relative paths/scopes anchor there; absolute paths pass through. The server cannot see your shell cwd; `..` in relative paths is refused.

BATCH related work; array parameters never accept singular values:

- `queries: [{query: "foo"}, {query: "bar", kind: "symbol"}]`
- `paths: ["src/a.rs#12-40", "src/b.rs#parse"]`
- `patterns: ["*.rs", "*.toml"]`
- `edits: [{path: "src/a.rs", tag: "1A2B", ops: [...]}, {path: "src/b.rs", tag: "3C4D", ops: [...]}]`

READ BEFORE WRITE: edit-mode `tilth_read` prints `[path#TAG]` above 1-based numbered lines. Copy its TAG and integer line numbers; NEVER invent either. A section read (`path#12-40`) carries the whole-file TAG; edit only lines it showed. `tilth_write` accepts `{path, tag?, ops}` sections. Line ops use copied integer `start`/`end`; `replace_text` swaps one exact unique `old`; `create_file` seeds a new path. Omit `tag` only for a new file or one too large to tag. Drift is 3-way-merged; a conflict rejects that section—re-read and retry it. Sections are independent.

JSON string values must escape tabs/newlines as `\t` and `\n`; literal controls break the call before the server receives it.

ROUTE: find/explore → `tilth_search`; read → `tilth_read`; importers/imports → `tilth_deps` (not import-greps); understand one symbol → `tilth_grok` (replaces search → expand → callers); changes → `tilth_diff`; browse → `tilth_list` (omit `patterns` for a project overview); edit → `tilth_write`.
DO NOT re-read expanded search content.