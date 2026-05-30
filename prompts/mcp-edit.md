

tilth_write: Batch write one or more files. Replaces the host Edit and Write tools.
Three per-file modes: hash (default — replace lines at hash anchors), overwrite (whole file), append (add to end). Mode aliases: h=hash, w=overwrite, a=append.
ALWAYS group writes to multiple files into ONE tilth_write call (max 20 files). Never call tilth_write twice in a row.
Each file path may appear at most once per call.
hash mode — edit an existing file:
tilth_read → copy anchors (<line>:<hash>) (BOTH line and hash required) → pass to tilth_write.
tilth_search does NOT provide hashes — you MUST tilth_read the file or section first.
Shape: {"files": [{"path": "a.rs", "edits": [{"start": "<line>:<hash>", "content": "<new code>"}]}]}
Single line: {"start": "<line>:<hash>", "content": "<new code>"}
Range:       {"start": "<line>:<hash>", "end": "<line>:<hash>", "content": "..."}
Delete:      {"start": "<line>:<hash>", "content": ""}
Hash mismatch → file changed, re-read THAT file and retry it (other files in the batch already applied).
A parse error on one edit invalidates ALL edits for that file (none applied); retry the whole file's edits after fixing the malformed entry.
overwrite mode — create a file, or replace one whole:
{"path": "new.rs", "mode": "overwrite", "content": "<full file body>"}
Create-only by default — fails if the file exists. Pass "overwrite": true to replace an existing file.
append mode — add to the end, creating the file if absent:
{"path": "log.txt", "mode": "append", "content": "<text to append>"}
Per-file results: each file is processed independently. A failure on one file does NOT block the others.
isError is false whenever ≥1 file succeeded — always scan the per-file `## <path>` sections for failures rather than trusting the top-level status.
Large files: tilth_read shows outline — use section to get hashlined content.
Pass diff: true to see a compact before/after diff per file.
After editing a function signature, tilth_write shows callers that may need updating.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.