# tilth

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/jahala/tilth/badge)](https://scorecard.dev/viewer/?uri=github.com/jahala/tilth)

**Smart code reading for humans and AI agents.** Reduces cost per correct answer by **44%** on Sonnet, **39%** on Opus, and **38%** on Haiku across 160 benchmark runs. ([benchmarks](#benchmarks))

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

### Expanded search

CLI search returns compact results by default. Use `--expand` to inline source for the top matches:

```bash
$ tilth handleAuth --scope src/ --expand       # top 2 (default when flag is bare)
$ tilth handleAuth --scope src/ --expand=5     # top 5
```

In MCP mode, `expand` defaults to 2 — no flag needed.

### Multi-symbol search

Trace across files in one call:

```bash
$ tilth "ServeHTTP, HandlersChain, Next" --scope .
```

Each symbol gets its own result block with definitions and expansions. The expand budget is shared — at least one expansion per symbol, deduped across files.

### Callers query

Find all call sites of a symbol using structural tree-sitter matching (not text search):

```bash
$ tilth isTrustedProxy --callers --scope .
# Callers of "isTrustedProxy" — 5 call sites

## context.go:1011 [caller: ClientIP]
→ trusted = c.engine.isTrustedProxy(remoteIP)
```

In MCP mode, use `kind: "callers"` on `tilth_search` instead.

### Blast-radius deps

See what a file imports and what depends on it — useful before renaming or changing exports:

```bash
$ tilth src/auth.ts --deps
# deps: src/auth.ts

## Imports (3)
  jsonwebtoken      (external)
  @/config          src/config.ts
  express           (external)

## Dependents (4)
  src/routes/api.ts        uses: handleAuth, AuthManager
  src/middleware/cors.ts   uses: validateToken
  src/app.ts               uses: AuthManager
  test/auth.test.ts        uses: handleAuth, AuthManager
```

In MCP mode, use the `tilth_deps` tool.

### Session dedup

In MCP mode, previously expanded definitions show `[shown earlier]` instead of the full body on subsequent searches. Saves tokens when the agent revisits symbols it already saw.

## Structural diff

```bash
$ tilth diff HEAD~1
# Diff: HEAD~1 — 3 files, 2 modified, 1 added (~350 tokens)

## src/auth.rs (3 symbols)
  [~:sig]  fn handleAuth(req) → (req, ctx)    L42
  [~]      fn validate_session                 L88
  [+]      fn refresh_token                    L120
```

Function-level change detection. Drill in with `--scope`, summarize history with `--log`, detect merge conflicts automatically. Replaces `git diff` for AI agents.

## Benchmarks

Code navigation tasks across 4 real-world repos (Express, FastAPI, Gin, ripgrep). Baseline = Claude Code built-in tools. tilth = built-in tools + tilth MCP server. We report **cost per correct answer** (`total_spend / correct_answers`) — the expected cost under retry. See [benchmark/](benchmark/) for full methodology.

| Model | Tasks | Runs | Baseline $/correct | tilth $/correct | Change | Baseline acc | tilth acc |
|---|---|---|---|---|---|---|---|
| Sonnet 4.6 | 26 | 86 | $0.26 | $0.15 | **-44%** | 84% | 94% |
| Opus 4.6 | 26 | 25 | $0.22 | $0.14 | **-39%** | 91% | 92% |
| Haiku 4.5 | 26 | 49 | $0.12 | $0.08 | **-38%** | 54% | 73% |
| **Average** | | **160** | **$0.20** | **$0.12** | **-40%** | **76%** | **86%** |

v0.5.0 introduces top-weighted MCP instructions and scope fallback, achieving 40% average cost reduction across all three models. Sonnet accuracy improves from 84% to 94%, Haiku from 54% to 73%. All models show significant turn reduction (25% average fewer turns).

Scope confusion (models passing invalid directory paths) is now handled with automatic fallback to cwd with a warning. DO NOT rules at the top of MCP instructions reduced redundant built-in tool usage (Grep, Read, Glob) to near-zero across all models.

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
tilth install opencode         # ~/.config/opencode/opencode.json
tilth install gemini           # ~/.gemini/settings.json
tilth install codex            # ~/.codex/config.toml
tilth install amp              # ~/.config/amp/settings.json
tilth install droid            # ~/.factory/mcp.json
tilth install antigravity      # ~/.gemini/antigravity/mcp_config.json
tilth install zed              # ~/.config/zed/settings.json
tilth install copilot-cli      # ~/.copilot/mcp-config.json
tilth install augment          # ~/.augment/settings.json
tilth install kiro             # ~/.kiro/settings/mcp.json
tilth install kilo-code        # VS Code globalStorage (extension)
tilth install cline            # VS Code globalStorage (extension)
tilth install roo-code         # VS Code globalStorage (extension)
tilth install trae             # .trae/mcp.json (project scope)
tilth install qwen-code        # ~/.qwen/settings.json
tilth install crush            # ~/.config/crush/crush.json
tilth install pi               # ~/.pi/agent/mcp.json
```

Add `--edit` to enable hash-anchored file editing (see [Edit mode](#edit-mode)):

```bash
tilth install claude-code --edit
```

Or call it from bash — see [AGENTS.md](./AGENTS.md) for the MCP agent prompt, or [skills/SKILL.md](./skills/SKILL.md) for a Claude Code skill prompt.

### Smaller models

Smaller models (e.g. Haiku) may ignore tilth tools in favor of built-in Bash/Grep. To force tilth adoption, disable the overlapping built-in tools:

```bash
claude --disallowedTools "Bash,Grep,Glob"
```

Benchmarks show Haiku benefits significantly from tilth (54% → 73% accuracy) but may still fall back to built-in tools. Forced mode ensures consistent tool adoption.

## How it decides what to show

| Input | Behaviour |
|-------|-----------|
| 0 bytes | `[empty]` |
| Binary | `[skipped]` with mime type |
| Generated (lockfiles, .min.js) | `[generated]` |
| < ~6000 tokens | Full content with line numbers |
| > ~6000 tokens | Structural outline with line ranges |

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
tilth <symbol> --expand=5         # inline source for top 5 matches
tilth <symbol> --callers          # find call sites (structural)
tilth <path> --deps               # imports + dependents
tilth "TODO: fix" --scope <dir>   # content search
tilth "/<regex>/" --scope <dir>   # regex search
tilth "*.test.ts" --scope <dir>   # glob files
tilth diff HEAD~1                 # structural diff (function-level)
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

Rust. ~20,000 lines. No runtime dependencies.

- **tree-sitter** — AST parsing for 14 languages (Rust, TypeScript, TSX, JavaScript, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift). Used for definition detection, callee extraction, callers query, and structural outlines.
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
