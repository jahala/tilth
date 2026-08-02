ALWAYS group every file you need into ONE tilth_read call via paths: [...]. Never call tilth_read twice in a row.

Read files with smart auto-sizing (omit `mode` unless you need to override it — see the `mode` schema field for the size options). Use for reading a known file, symbol, or range; do NOT use if the content is already shown in expanded search results. Example: `tilth_read(paths: ["src/lib.rs", "src/mcp.rs#tool_search"], cwd: "/abs/checkout")`.

Output format: each line is `N:content` — a 1-based line number, a colon, then the line's text (e.g. `42:let x = 1;`). Ignore the `N:` prefix; resume after the colon. In edit mode the numbered lines are preceded by a `[path#TAG]` header binding them to the file's current content — copy that header VERBATIM into a `tilth_write` `edits` section and reference the line numbers beneath it (NEVER invent a TAG). Exception: `mode=stripped` is a non-editable survey view and cannot round-trip through `tilth_write`.

Responses lead with a single JSON header line carrying any structured signals at once. Fields:

- `if_modified_since: "<ts>"` — pass that exact ts in your next call to get `(unchanged @ <ts>)` stubs for unchanged files.
- `view: "outline" | "signature" | "stripped"` — the shape rendered. Absent means full content.
- `original_line_count: <N>` — the file's line count before view shaping or budget clipping.
- `next_view: "full"` — escalating would surface content currently hidden. Absent on explicit shape requests.
- `lines_stripped: <N>` — only on `view: "stripped"`; how many lines the strip pass removed.
- `truncated: true`, `truncated_at_line: <N>` — a `budget` arg clipped the body. Use with `original_line_count` to render "showing 1–N of M lines".