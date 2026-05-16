Hash-anchored / overwrite / append edits across one or more files. Replaces the host Edit and Write tools — DO NOT use those.

Example overwrite (new file): `tilth_write(files: [{path: "src/new.rs", mode: "overwrite", content: "fn main(){}\n"}])`.

Request shape:
```json
{
  "files": [
    {"path": "a.rs", "mode": "hash", "edits": [
      {"start": "<line>:<hash>", "content": "<new code>"},
      {"start": "<line>:<hash>", "end": "<line>:<hash>", "content": "..."},
      {"start": "<line>:<hash>", "content": ""}
    ]},
    {"path": "new.rs", "mode": "overwrite", "content": "..."},
    {"path": "log.txt", "mode": "append",    "content": "...\n"}
  ],
  "diff": true
}
```

Anchor grammar — only the `<line>:<hash>` prefix, no body, no pipe, no bare line:

```text
WRONG: "20:7ae|    def create_run("    do NOT include the body
WRONG: "20"                             hash is required
WRONG: "20:7ae|"                        drop the trailing pipe
RIGHT: "20:7ae"
```

Modes per file: hash (default — replace lines at hash anchors), overwrite (whole file; creates if absent), append (creates if absent). Hash mode tolerates a stale anchor hash: if the line at your claimed `<line>` still holds the same content you read (byte-exact, hash drifted only because the line was re-hashed), the edit re-applies and the response notes `auto-fixed: <line> → <line>`. Any other mismatch — line has different content, file has shifted, body genuinely moved — returns a fresh hashlined region inline for one-turn retry rather than guessing at a relocation. A malformed edit entry fails that whole file but does not block siblings.

ALWAYS group edits to all ready files into ONE tilth_write call (max 20 files). Each path may appear only once per call. Never call tilth_write twice in a row.

Pass `diff: true` to verify what landed without a separate read. If the diff reveals another needed edit, DO NOT call tilth_write again immediately — collect every visible follow-up, then issue ONE grouped follow-up call (after a read/search verification step if needed).
