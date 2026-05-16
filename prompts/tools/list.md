ALWAYS group every pattern you need into ONE tilth_list call via patterns: [...]. Never call tilth_list twice in a row.

Directory layout with token-cost rollups. Example tree output:
```text
src/      ~28k tokens   45 files
├── cache.rs      ~833 tokens
├── search/      ~14k tokens   8 files
│   └── mod.rs      ~5.0k tokens
```
Use before tilth_read to budget context. Accepts patterns: ["*.rs", "src/**/*.ts"] and optional depth.
