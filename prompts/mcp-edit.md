

tilth_edit: Batch edit one or more files using hash-anchored lines. Replaces the host Edit tool.
ALWAYS group edits to multiple files into ONE tilth_edit call (max 20 files). Never call tilth_edit twice in a row.
tilth_read → copy anchors (<line>:<hash>) (BOTH line and hash required) → pass to tilth_edit.
tilth_search does NOT provide hashes — you MUST tilth_read the file or section first.
Shape: {"files": [{"path": "a.rs", "edits": [...]}, {"path": "b.rs", "edits": [...]}]}
Single file: {"files": [{"path": "a.rs", "edits": [{"start": "<line>:<hash>", "content": "<new code>"}]}]}
Edit forms inside `edits`:
Single line: {"start": "<line>:<hash>", "content": "<new code>"}
Range:       {"start": "<line>:<hash>", "end": "<line>:<hash>", "content": "..."}
Delete:      {"start": "<line>:<hash>", "content": ""}
Creating a new file: {"path": "new.rs", "create": true, "content": "<full file body>"} — `create` and `content` at file level, NOT inside `edits`. Fails atomically if the file exists. Parent dirs auto-created. To modify an existing file, use `edits` with hash anchors instead.
Per-file results: each file is processed independently. A hash mismatch or create-already-exists on one file does NOT block the others.
isError is false whenever ≥1 file succeeded — always scan the per-file `## <path>` sections for failures rather than trusting the top-level status.
Hash mismatch → file changed, re-read THAT file and retry it (other files in the batch already applied).
A parse error on one edit invalidates ALL edits for that file (none applied); retry the whole file's edits after fixing the malformed entry.
Each file path may appear at most once per call — group all edits for a file under its single entry.
Large files: tilth_read shows outline — use section to get hashlined content.
Pass diff: true to see a compact before/after diff per file.
After editing a function signature, tilth_edit shows callers that may need updating.
DO NOT use the host Edit tool. Use tilth_edit for all edits.