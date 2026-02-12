# tilth

**Smart code reading for humans and AI agents.**

tilth is what happens when you give `ripgrep`, `tree-sitter`, and `cat` a shared brain. It reads code the way you do — structure first, details when needed.

```bash
$ tilth src/auth.ts
# src/auth.ts (258 lines, ~3.4k tokens) [outline]

[1-12]   imports: express(2), jsonwebtoken, @/config
[14-22]  interface AuthConfig
[24-42]  fn validateToken(token: string): Claims | null
[44-89]  export fn handleAuth(req, res, next)
[91-120] fn refreshSession(req, res)
[122-258] export class AuthManager
  [130-145] fn constructor(config: AuthConfig)
  [147-180] fn authenticate(credentials)
  [182-220] fn authorize(user, resource)
  [222-258] fn revoke(tokenId)
```

Small files print in full. Large files print their skeleton with line ranges. You drill in with `--section`:

```bash
$ tilth src/auth.ts --section 44-89
```

That's it. No flags to remember. No mode selection. Files under ~1500 tokens come back whole. Everything else gets an outline.

## Search finds definitions first

```bash
$ tilth handleAuth --scope src/
# Search: "handleAuth" in src/ — 6 matches (2 definitions, 4 usages)

## src/auth.ts:44 [definition]
  [24-42]  fn validateToken(token: string)
→ [44-89]  export fn handleAuth(req, res, next)
  [91-120] fn refreshSession(req, res)

## src/routes/api.ts:34 [usage]
→ [34]   router.use('/api/protected/*', handleAuth);
```

Tree-sitter finds where symbols are **defined** — not just where strings appear. Definitions sort first. Each match shows its surrounding file structure so you know what you're looking at without a second read.

## Why

I built this because I watched AI agents make 6 tool calls to find one function. `glob → read → "too big" → grep → read again → read another file`. Each round-trip burns inference time and tokens.

tilth gives agents (and humans) structural awareness in one call. The outline tells you *what's in the file*. The search tells you *where things are defined and used*. The `--section` flag gets you *exactly the lines you need*.

It's also just a nicer `cat` for codebases.

## Install

```bash
cargo install tilth
# or
npx tilth
```

Prebuilt binaries for macOS and Linux on the [releases page](https://github.com/jahala/tilth/releases).

To add tilth as an MCP server to your editor:

```bash
tilth install cursor      # ~/.cursor/mcp.json
tilth install windsurf     # ~/.codeium/windsurf/mcp_config.json
tilth install claude-code  # ~/.claude.json
tilth install vscode       # .vscode/mcp.json (project scope)
tilth install claude-desktop
```

## Usage

```bash
tilth <path>                      # read file (outline if large)
tilth <path> --section 45-89      # exact line range
tilth <path> --full               # force full content
tilth <symbol> --scope <dir>      # definitions + usages
tilth "TODO: fix" --scope <dir>   # content search
tilth "/<regex>/" --scope <dir>   # regex search
tilth "*.test.ts" --scope <dir>   # glob files
tilth --map --scope <dir>         # codebase skeleton
```

## What's inside

Rust. ~3,300 lines. No runtime dependencies.

- **tree-sitter** — AST parsing for 9 languages (Rust, TypeScript, JavaScript, Python, Go, Java, C, C++, Ruby)
- **ripgrep internals** (`grep-regex`, `grep-searcher`) — fast content search
- **ignore** crate — parallel directory walking with explicit junk-directory skip list (searches all files including gitignored ones)
- **memmap2** — memory-mapped file reads
- **DashMap** — concurrent outline cache, keyed by mtime

Files are memory-mapped, not read into buffers. Outlines are cached and invalidated by mtime. Search runs definitions and usages in parallel via `rayon::join`. Binary files, generated lockfiles, and empty files are detected and skipped in one line.

## For AI agents

tilth runs as an MCP server:

```bash
tilth install cursor  # or windsurf, claude-code, vscode, claude-desktop
```

Five tools over JSON-RPC stdio: `tilth_read`, `tilth_search`, `tilth_files`, `tilth_map`, `tilth_session`. One persistent process, grammars loaded once, shared cache. Server instructions are sent to the LLM automatically during initialization.

### Edit mode

Add `--edit` during install to enable hash-anchored file editing:

```bash
tilth install claude-code --edit
tilth install cursor --edit
```

This registers tilth with `["--mcp", "--edit"]` in your MCP config. It adds a sixth tool (`tilth_edit`) and switches `tilth_read` to hashline output — every line tagged with a content hash:

```
42:a3f|  let x = compute();
43:f1b|  return x;
```

The `line:hash` before the `|` is the anchor. `tilth_edit` uses these to apply verified edits — if the file changed since the last read, the hashes won't match and the edit is rejected with current content shown:

```json
{
  "path": "src/auth.ts",
  "edits": [
    { "start": "42:a3f", "content": "  let x = recompute();" },
    { "start": "44:b2c", "end": "46:e1d", "content": "" }
  ]
}
```

For large files, `tilth_read` still returns an outline first. Use `section` to get hashlined content for the lines you need to edit.

To install without edit mode (read-only, no `tilth_edit`):

```bash
tilth install claude-code
```

Inspired by [The Harness Problem](https://blog.can.ac/2026/02/12/the-harness-problem/) — the insight that LLM coding performance can be bottlenecked by the edit interface, not model capability.

Or call the CLI from bash — every agent framework has a shell tool. Add this to your agent prompt:

```
You have `tilth` installed. Use it instead of read_file, grep, glob, and find.
Do not use other code reading tools — tilth replaces all of them.
```

See [AGENTS.md](./AGENTS.md) for the full prompt.

## How it decides what to show

| File size | Behaviour |
|-----------|-----------|
| 0 bytes | `[empty]` one-liner |
| Binary | `[skipped]` with mime type |
| Generated (lockfiles, .min.js) | `[generated]` one-liner |
| < ~1500 tokens | Full content with line numbers |
| > ~1500 tokens | Structural outline with line ranges |

The threshold is token-based, not line-based. A 1-line minified bundle gets outlined. A 120-line focused module prints whole.

## Speed

Benchmarked on x86_64 Mac across codebases of 26–1060 files. CLI times include ~17ms process startup — MCP mode (persistent server) pays this once.

| Operation | ~30 files | ~1000 files |
|-----------|-----------|-------------|
| File read + type detect | ~18ms | ~18ms |
| Code outline (400 lines) | ~18ms | ~18ms |
| Symbol search | ~27ms | — |
| Content search | ~26ms | — |
| Glob | ~24ms | — |
| Map (codebase skeleton) | ~21ms | ~240ms |

Symbol search, content search, and glob use early termination — they return the top results and stop walking, so time is roughly constant regardless of codebase size. Map must visit every file.

## Name

**tilth** — the state of soil that's been prepared for planting. Good tilth means structured ground where things can take root.

Your codebase is the soil. tilth gives it structure so you (or your agent) can find where to dig.

## License

MIT
