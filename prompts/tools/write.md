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

Modes per file: hash (default — replace lines at hash anchors), overwrite (whole file; creates if absent), append (creates if absent). Hash mode auto-fixes safe mismatches: if your anchor body appears at exactly one new location, the edit lands there and the response notes `auto-fixed: <old_line> → <new_line>`. Zero or 2+ matches → fresh hashlined region returned inline for one-turn retry. A malformed edit entry fails that whole file but does not block siblings.

ALWAYS group edits to all ready files into ONE tilth_write call (max 20 files). Each path may appear only once per call. Never call tilth_write twice in a row.

Pass `diff: true` to verify what landed without a separate read.
