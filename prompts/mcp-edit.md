

tilth_write: Batch edit files with an op-grammar text blob (the `edits` string). Replaces the host Edit and Write tools.
Read first (edit mode): tilth_read / tilth_search show a `[path#TAG]` header then `N:content` numbered lines. Copy the `[path#TAG]` header VERBATIM and reference the line numbers you see. NEVER invent a TAG.
Send edits as ONE `edits` string of `[path#TAG]` sections, with op lines beneath each header. Line numbers are 1-based inclusive, from the numbered read:
SWAP a.=b:  then payload — replace lines a..b (SWAP n: for a single line)
DEL n | DEL a.=b — delete a line or range
INS.PRE n: | INS.POST n:  then payload — insert before/after line n
INS.HEAD: | INS.TAIL:  then payload — insert at start/end of file
SWAP.BLK n: | SWAP.BLK #symbol:  then payload — replace the tree-sitter block at line n or named symbol
DEL.BLK n | DEL.BLK #symbol — delete that block
INS.BLK.POST n: | INS.BLK.POST #symbol:  then payload — insert after that block
REM — delete the file
MV dest — move/rename the file
Payload: one row per line after the op header; prefix a row with `+` to force it literal (e.g. a row that itself looks like an op or a `[header]`).
Example:
[src/x.rs#1A2B]
SWAP 2:

+ let y = 1;
DEL 5
INS.POST 8:
+// trailing note
Drift: the TAG binds the section to the content you read. If the file changed since, tilth 3-way-merges your ops onto the live file; if the merge conflicts the section is REJECTED — re-read THAT file and retry it. A tag not from this session is rejected (never invent one).
New file: a tagless `[path]` header (no #TAG) seeds a NEW file — use INS.HEAD for its content.
Append cleanly: prefer `INS.POST <last-content-line>` over `INS.TAIL:` — INS.TAIL inserts after the file's trailing empty row, adding a blank line for newline-terminated files.
Sections are independent (best-effort): a rejected section does NOT block the others; scan the per-`## <path>` results for failures. Max 20 sections.
root: absolute checkout dir. Required if any section path is relative. RELATIVE section paths and MV destinations are anchored under root and confined to it; ABSOLUTE section paths are also confined to root (or to the server's startup directory when root is omitted) — `..` traversal or paths outside the confinement root are refused. The server cannot see your shell cwd.
Pass diff: true for a compact before/after diff per section.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.