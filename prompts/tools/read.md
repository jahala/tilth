BATCH every needed file into one `tilth_read` call; `paths` never accepts a singular value. Read known files, symbols, or ranges with smart sizing; DO NOT re-read search-expanded content. Example: `tilth_read(paths: ["src/lib.rs", "src/mcp.rs#tool_search"], cwd: "/abs/repo")`.

Output lines are `N:content`; ignore the `N:` prefix — line numbers are not file content. In edit mode, `[path#TAG]` appears above the numbered lines; copy that TAG and the shown 1-based line numbers into `tilth_write`—NEVER invent either. `mode=stripped` is non-editable.

A leading JSON header may report view/truncation metadata; reuse its timestamp for unchanged stubs. `next_view` means content is hidden; escalate `mode` to see it.