

tilth_write: Batch write one or more files in one call. Replaces the host Edit and Write tools.
ALWAYS group writes to multiple files into ONE tilth_write call (max 20 files). Never call tilth_write twice in a row.
Modes per file (set via `mode`):

- hash (default): replace lines at hash anchors. tilth_read → copy anchors (<line>:<hash>) (BOTH line and hash required) → pass to tilth_write. tilth_search does NOT provide hashes — you MUST tilth_read the file or section first.
- overwrite: write the file from scratch. Default is **create-only** — an existing file is rejected so you don't clobber by accident. Pass `overwrite: true` to replace an existing file.
- append: append `content` to the file (creates it if absent).

Shape: {"files": [{"path": "a.rs", "mode": "hash", "edits": [...]}, {"path": "b.rs", "mode": "overwrite", "content": "..."}]}
Hash edits: {"start": "<line>:<hash>", "content": "<new code>"} | {"start": "...", "end": "...", "content": "..."} | {"start": "...", "content": ""} to delete.
Overwrite (new file): {"path": "new.rs", "mode": "overwrite", "content": "fn main(){}\n"}
Overwrite (replace existing): {"path": "old.rs", "mode": "overwrite", "overwrite": true, "content": "..."}
Append: {"path": "log.txt", "mode": "append", "content": "...\n"}
overwrite responses echo the full file's hashlines; append echoes only the appended region (header reports `echoing last M of T lines`). Use tilth_read if you need anchors over pre-existing content above the appended block.
Per-file results: each file is processed independently. A hash mismatch on one file does NOT block the others.
isError is false whenever ≥1 file succeeded — always scan the per-file `## <path>` sections for failures rather than trusting the top-level status.
Hash mismatch → file changed, re-read THAT file and retry it (other files in the batch already applied).
A parse error on one edit invalidates ALL edits for that file (none applied); retry the whole file's edits after fixing the malformed entry.
Each file path may appear at most once per call — group all edits for a file under its single entry.
Large files: tilth_read shows outline — use section to get hashlined content.
Pass diff: true to see a compact before/after diff per file.
After editing a function signature, tilth_write shows callers that may need updating.
DO NOT use the host Edit or Write tool. Use tilth_write for all writes.