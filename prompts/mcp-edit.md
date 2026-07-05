

tilth_write: Batch edit files with a JSON `edits` array of `{path, tag?, ops}` section objects. Replaces the host Edit and Write tools.
Read first (edit mode): tilth_read / tilth_search show a `[path#TAG]` header then `N:content` numbered lines. Copy the 4-hex TAG into the section's `tag` and reference the line numbers you see. NEVER invent a TAG.
Send `edits` as an ARRAY of section objects, each `{path, tag?, ops}`. Each op is an object tagged by `op`. Line numbers are 1-based inclusive, from the numbered read:
replace {start, end, content} — replace lines start..end (start==end for one line)
delete {start, end} — delete a line or range
insert_before {line, content} | insert_after {line, content} — insert before/after line
prepend {content} | append {content} — insert at start/end of file
replace_block {at, content} — replace the tree-sitter block at `at` (a line number or a "#symbol" string)
delete_block {at} — delete that block
insert_after_block {at, content} — insert after that block
delete_file — delete the file
move_file {dest} — move/rename the file
`content` is a single string with embedded newlines (use \n). `at` is an integer line number or a "#symbol" name string (the leading `#` is optional — a bare `symbol` name is also accepted).
Example:
tilth_write(edits: [{"path": "src/x.rs", "tag": "1A2B", "ops": [
  {"op": "replace", "start": 2, "end": 2, "content": "let y = 1;"},
  {"op": "delete", "start": 5, "end": 5},
  {"op": "insert_after", "line": 8, "content": "// trailing note"}
]}])
Drift: the TAG binds the section to the content you read. If the file changed since, tilth 3-way-merges your ops onto the live file; if the merge conflicts the section is REJECTED — re-read THAT file and retry it. A tag not from this session is rejected (never invent one).
New file: OMIT `tag` to seed a NEW file — use prepend for its content.
Sections are independent (best-effort): a rejected section does NOT block the others; scan the per-`## <path>` results for failures. Max 20 sections.
DO NOT pass `edits` as a string (the old `[path#TAG]` text grammar or a JSON-encoded string) — it is rejected. Pass the array itself.
cwd: your absolute checkout dir (REQUIRED). RELATIVE section paths and move_file destinations anchor under `cwd`; absolute paths pass through as-is. `..` traversal in a relative path is refused.
Pass diff: true for a compact before/after diff per section.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.