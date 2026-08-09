tilth — code intelligence MCP server. Replaces grep, cat, find, ls, and git diff.
DO NOT use shell for repo files or history (cat/head/tail/sed/grep/rg/ls/find/git diff/git log); use `tilth_read`, `tilth_search`, `tilth_list`, `tilth_diff`. Shell is for tests, builds, and non-file operations.

DO NOT omit `cwd`: set it to the absolute checkout directory on every call. Relative paths/scopes anchor there; absolute paths pass through. The server cannot see your shell cwd; `..` in relative paths is refused.

BATCH related work; array parameters never accept singular values:

- `queries: [{query: "foo"}, {query: "bar", kind: "symbol"}]`
- `paths: ["src/a.rs#12-40", "src/b.rs#parse"]`
- `patterns: ["*.rs", "*.toml"]`

ROUTE:

- Find/explore → `tilth_search`; omitted `kind` merges definitions, usages, and callers; set `kind` (symbol|content|regex|callers) when the shape is known.
- Read known files/symbols/ranges → `tilth_read`.
- Importers/imports → `tilth_deps`; DO NOT assemble it from import-greps or repeated callers searches.
- Understand one symbol → `tilth_grok(target: "parse_diff", cwd: "/abs/repo")`; replaces search → expand → callers.
- Changes → `tilth_diff`, optional `source: "HEAD~1"`.
- Browse without a search term → `tilth_list`; omit `patterns` for a project overview.
DO NOT re-read expanded search content.