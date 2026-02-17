# tilth

**Smart code reading for humans and AI agents.** Reduces cost per correct answer by **29%** on Sonnet and **22%** on Opus across 114 benchmark runs. ([benchmarks](#benchmarks))

tilth is what happens when you give `ripgrep`, `tree-sitter`, and `cat` a shared brain.

```bash
$ tilth src/auth.ts
# src/auth.ts (258 lines, ~3.4k tokens) [outline]

[1-12]   imports: express(2), jsonwebtoken, @/config
[14-22]  interface AuthConfig
[24-42]  fn validateToken(token: string): Claims | null
[44-89]  export fn handleAuth(req, res, next)
[91-258] export class AuthManager
  [99-130]  fn authenticate(credentials)
  [132-180] fn authorize(user, resource)
```

Small files come back whole. Large files get an outline. Drill in with `--section`:

```bash
$ tilth src/auth.ts --section 44-89
$ tilth docs/guide.md --section "## Installation"
```

## Search finds definitions first

```
$ tilth handleAuth --scope src/
# Search: "handleAuth" in src/ — 6 matches (2 definitions, 4 usages)

## src/auth.ts:44-89 [definition]
  [24-42]  fn validateToken(token: string)
→ [44-89]  export fn handleAuth(req, res, next)
  [91-120] fn refreshSession(req, res)

  44 │ export function handleAuth(req, res, next) {
  45 │   const token = req.headers.authorization?.split(' ')[1];
  ...
  88 │   next();
  89 │ }

── calls ──
  validateToken  src/auth.ts:24-42  fn validateToken(token: string): Claims | null
  refreshSession  src/auth.ts:91-120  fn refreshSession(req, res)

## src/routes/api.ts:34 [usage]
→ [34]   router.use('/api/protected/*', handleAuth);
```

Tree-sitter finds where symbols are **defined** — not just where strings appear. Each match shows its surrounding file structure so you know what you're looking at without a second read.

Expanded definitions include a **callee footer** (`── calls ──`) showing resolved callees with file, line range, and signature — the agent can follow call chains without separate searches for each callee.

### Multi-symbol search

Trace across files in one call:

```bash
$ tilth "ServeHTTP, HandlersChain, Next" --scope .
```

Each symbol gets its own result block with definitions and expansions. The expand budget is shared — at least one expansion per symbol, deduped across files.

### Callers query

Find all call sites of a symbol using structural tree-sitter matching (not text search):

```bash
$ tilth isTrustedProxy --kind callers --scope .
# Callers of "isTrustedProxy" — 5 call sites

## context.go:1011 [caller: ClientIP]
→ trusted = c.engine.isTrustedProxy(remoteIP)
```

### Session dedup

In MCP mode, previously expanded definitions show `[shown earlier]` instead of the full body on subsequent searches. Saves tokens when the agent revisits symbols it already saw.

## Benchmarks

Code navigation tasks across 4 real-world repos (Express, FastAPI, Gin, ripgrep). Baseline = Claude Code built-in tools. tilth = built-in tools + tilth MCP server. We report **cost per correct answer** (`total_spend / correct_answers`) — the expected cost under retry. See [benchmark/](benchmark/) for full methodology.

| Model | Tasks | Baseline $/correct | tilth $/correct | Change | Baseline acc | tilth acc |
|---|---|---|---|---|---|---|
| Sonnet 4.5 | 26 (52 runs) | $0.26 | $0.19 | **-29%** | 96% | 92% |
| Opus 4.6 | 5 hard (10 runs) | $0.29 | $0.23 | **-22%** | 100% | 100% |
| Haiku 4.5 | 26 (52 runs) | $0.17 | $0.19 | +12% | 58% | 69% |
| **Average** | **114 runs** | **$0.23** | **$0.19** | **-18%** | **79%** | **82%** |

Sonnet achieves 98% tilth tool adoption and wins 19 of 26 tasks on cost. Opus solves all 5 hard tasks with 100% adoption and -22% $/correct. Haiku gains +12pp accuracy with tilth (5 new tasks solved) but costs more per attempt — net +12% $/correct. Forced mode (`--disallowedTools`) is recommended for Haiku.

See [benchmark/](benchmark/) for per-task results, by-language breakdowns, and model comparison.

## Why

I built this because I watched AI agents make 6 tool calls to find one function. `glob → read → "too big" → grep → read again → read another file`. Each round-trip burns tokens and inference time.

tilth gives structural awareness in one call. The outline tells you *what's in the file*. The search tells you *where things are defined*. `--section` gets you *exactly the lines you need*.

## Install

```bash
cargo install tilth
# or
npx tilth
```

Prebuilt binaries on the [releases page](https://github.com/jahala/tilth/releases).

### MCP server

```bash
tilth install claude-code      # ~/.claude.json
tilth install cursor           # ~/.cursor/mcp.json
tilth install windsurf         # ~/.codeium/windsurf/mcp_config.json
tilth install vscode           # .vscode/mcp.json (project scope)
tilth install claude-desktop
```

Add `--edit` to enable hash-anchored file editing (see [Edit mode](#edit-mode)):

```bash
tilth install claude-code --edit
```

Or call it from bash — see [AGENTS.md](./AGENTS.md) for the agent prompt.

### Smaller models

Smaller models (e.g. Haiku) may ignore tilth tools in favor of built-in Bash/Grep. To force tilth adoption, disable the overlapping built-in tools:

```bash
claude --disallowedTools "Bash,Grep,Glob"
```

Benchmarks show Haiku only adopts tilth tools 42% of the time in hybrid mode. Forced mode ensures consistent tool adoption and improves accuracy.

## How it decides what to show

| Input | Behaviour |
|-------|-----------|
| 0 bytes | `[empty]` |
| Binary | `[skipped]` with mime type |
| Generated (lockfiles, .min.js) | `[generated]` |
| < ~3500 tokens | Full content with line numbers |
| > ~3500 tokens | Structural outline with line ranges |

Token-based, not line-based — a 1-line minified bundle gets outlined; a 120-line focused module prints whole.

## Edit mode

Install with `--edit` to add `tilth_edit` and switch `tilth_read` to hashline output:

```
42:a3f|  let x = compute();
43:f1b|  return x;
```

`tilth_edit` uses these hashes as anchors. If the file changed since the last read, hashes won't match and the edit is rejected with current content shown:

```json
{
  "path": "src/auth.ts",
  "edits": [
    { "start": "42:a3f", "content": "  let x = recompute();" },
    { "start": "44:b2c", "end": "46:e1d", "content": "" }
  ]
}
```

Large files still outline first — use `section` to get hashlined content for the part you need.

Inspired by [The Harness Problem](https://blog.can.ac/2026/02/12/the-harness-problem/).

## Usage

```bash
tilth <path>                      # read file (outline if large)
tilth <path> --section 45-89      # exact line range
tilth <path> --section "## Foo"   # markdown heading
tilth <path> --full               # force full content
tilth <symbol> --scope <dir>      # definitions + usages
tilth "TODO: fix" --scope <dir>   # content search
tilth "/<regex>/" --scope <dir>   # regex search
tilth "*.test.ts" --scope <dir>   # glob files
tilth --map --scope <dir>         # codebase skeleton (CLI only)
```

`--map` is available in the CLI but not exposed as an MCP tool — benchmarks showed AI agents overused it, hurting accuracy.

## Speed

CLI times on x86_64 Mac, 26–1060 file codebases. Includes ~17ms process startup (MCP mode pays this once).

| Operation | ~30 files | ~1000 files |
|-----------|-----------|-------------|
| File read + type detect | ~18ms | ~18ms |
| Code outline (400 lines) | ~18ms | ~18ms |
| Symbol search | ~27ms | — |
| Content search | ~26ms | — |
| Glob | ~24ms | — |
| Map (codebase skeleton) | ~21ms | ~240ms |

Search, content search, and glob use early termination — time is roughly constant regardless of codebase size.

## What's inside

Rust. ~6,000 lines. No runtime dependencies.

- **tree-sitter** — AST parsing for 9 languages (Rust, TypeScript, JavaScript, Python, Go, Java, C, C++, Ruby). Used for definition detection, callee extraction, callers query, and structural outlines.
- **ripgrep internals** (`grep-regex`, `grep-searcher`) — fast content search
- **ignore** crate — parallel directory walking, searches all files including gitignored
- **memmap2** — memory-mapped file reads (no buffers)
- **DashMap** — concurrent outline cache, invalidated by mtime

Search runs definitions and usages in parallel via `rayon::join`. Callee resolution runs at expand time — extract callee names via tree-sitter queries, resolve against the source file's outline and imported files. Callers query uses the same tree-sitter patterns in reverse, walking the codebase with `memchr` SIMD pre-filtering for fast elimination.

The search output format is informed by wavelet multi-resolution (outline headers show line ranges for drill-down) and 1-hop callee expansion (expanded definitions resolve callees inline).

## Name

**tilth** — the state of soil that's been prepared for planting. Your codebase is the soil; tilth gives it structure so you can find where to dig.

## Support

[!["Buy Me A Coffee"](https://www.buymeacoffee.com/assets/img/custom_images/orange_img.png)](https://buymeacoffee.com/jahala)

## License

MIT
