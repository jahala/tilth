# tilth

[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/jahala/tilth/badge)](https://scorecard.dev/viewer/?uri=github.com/jahala/tilth)

**Smart code reading for humans and AI agents.** tilth parses your code with tree-sitter and answers questions that text search can't. It shows where a symbol is defined, who calls it, what it calls, what depends on a file, and what changed at function level.

tilth is one binary. It works as a command-line tool and as an MCP server, which is how most agent hosts connect to tools. It builds no index and needs no setup.

```
$ tilth fastapi/dependencies/utils.py
# fastapi/dependencies/utils.py (1027 lines, ~9.6k tokens)

[1-]   imports: dataclasses, inspect, sys
[86-110]     fn ensure_multipart_is_installed
           def ensure_multipart_is_installed() -> None
[113-125]    fn get_parameterless_sub_dependant
           def get_parameterless_sub_dependant(*, depends: params.Depends, path: str) -> Dependant
[128-179]    fn get_flat_dependant
           def get_flat_dependant(
...
[203-215]    fn _get_signature
           def _get_signature(call: Callable[..., Any]) -> inspect.Signature
[218-232]    fn get_typed_signature
           def get_typed_signature(call: Callable[..., Any]) -> inspect.Signature
...
```

Small files come back whole. Large files come back as an outline with line ranges. Ask for the part you need with `--section`:

```bash
tilth fastapi/dependencies/utils.py --section 218-232
tilth docs/guide.md --section "## Installation"
```

The examples in this README are real output from the [FastAPI](https://github.com/fastapi/fastapi) and [Gin](https://github.com/gin-gonic/gin) repositories. Lines marked `...` are cut for length.

## Search finds definitions first

````
$ tilth get_typed_signature --scope fastapi --expand=1
# Search: "get_typed_signature" in fastapi — 2 matches (1 definitions, 1 usages)

### dependencies/utils.py:218-232 [definition]
  [203-215]    fn _get_signature
             def _get_signature(call: Callable[..., Any]) -> inspect.Signature
-> [218-232]    fn get_typed_signature
             def get_typed_signature(call: Callable[..., Any]) -> inspect.Signature
  [235-241]    fn get_typed_annotation

```dependencies/utils.py:218-232
 218 | def get_typed_signature(call: Callable[..., Any]) -> inspect.Signature:
 219 |     signature = _get_signature(call)
 220 |     unwrapped = inspect.unwrap(call)
 ...
 232 |     return typed_signature
```

-- calls --
  _get_signature  dependencies/utils.py:203-215  def _get_signature(call: Callable[..., Any]) -> inspect.Signature
  get_typed_annotation  dependencies/utils.py:235-241  def get_typed_annotation(annotation: Any, globalns: dict[str, Any]) -> Any

### dependencies/utils.py:277 [usage in function get_dependant]
  [244-253]    fn get_typed_return_annotation
             def get_typed_return_annotation(call: Callable[..., Any]) -> Any
-> [256-328]    fn get_dependant
             def get_dependant(
  [331-352]    fn add_non_field_param_to_dependency

(~439 tokens)
````

tilth uses the syntax tree to tell a definition from a mention, and it lists definitions first. Each match is shown with the symbols around it, so you can see where you are without reading the file.

An expanded definition ends with a `-- calls --` footer. It lists the functions the definition calls, each with its file, line range and signature. An agent can follow a call chain from there without searching again.

When a search finds more than it shows, the header says so, for example `10 of 43 matches`. Add `--full` to raise the limit from 10 to 100.

### Expanded search

On the command line, search returns compact results. `--expand` adds the source of the top matches:

```bash
tilth ServeHTTP --scope . --expand       # top 2
tilth ServeHTTP --scope . --expand=5     # top 5
```

In MCP mode, `expand` defaults to 2.

### Multi-symbol search

Look up several symbols in one call:

```bash
tilth "ServeHTTP, HandlersChain, Next" --scope .
```

Each symbol gets its own block of results. The expand budget is shared across them, with at least one expansion per symbol.

### Callers

Find every call site of a symbol. tilth matches calls on the syntax tree, so comments and strings that mention the name are left out.

```
$ tilth isTrustedProxy --callers --scope .
# Callers of "isTrustedProxy" in ~/gin — 5 call sites

## context.go:1011 [caller: ClientIP]
-> trusted = c.engine.isTrustedProxy(remoteIP)
...
## gin.go:496 [caller: validateHeader]
-> if (i == 0) || (!engine.isTrustedProxy(ip)) {
```

In MCP mode, use `kind: "callers"` on `tilth_search`.

### File dependencies

See what a file uses and what uses it. This is worth a look before you rename or remove an export.

```
$ tilth recovery.go --deps
# Deps: recovery.go — 5 local, 0 external, 3 dependents

## Uses (local)
context.go                     Abort, AbortWithStatus, Error, Next
debug.go                       IsDebugging
fs.go                          Open
gin.go                         New
routergroup.go                 handle

## Used by
benchmarks_test.go:22          BenchmarkRecoveryMiddleware → Recovery
benchmarks_test.go:36          BenchmarkManyHandlers → Recovery
gin.go:239                     Default              → Recovery
recovery_test.go:22            TestPanicClean       → RecoveryWithWriter
...
```

In MCP mode, use the `tilth_deps` tool.

### Grok a symbol

`grok` returns everything about one symbol in a single call: signature, doc comment, body, callees, callers, siblings and tests.

```
$ tilth grok get_typed_signature --scope fastapi
# grok: get_typed_signature [dependencies/utils.py:218]

## signature
def get_typed_signature(call: Callable[..., Any]) -> inspect.Signature

## body
...

## callees (2 internal, 5 extern)
  dependencies/utils.py
    _get_signature       [203-215]   def _get_signature(call: Callable[..., Any]) -> inspect.Signature
    get_typed_annotation [235-241]   def get_typed_annotation(annotation: Any, globalns: dict[str, Any]) -> Any
...

## callers (1)
  dependencies/utils.py
    [277]   in get_dependant()
```

In MCP mode, use the `tilth_grok` tool.

### Session memory

In MCP mode, a definition that was already expanded comes back as `[shown earlier]` in later searches. The agent does not receive the same body twice.

## Structural diff

```
$ tilth diff HEAD~1
# Diff: HEAD~1 — 10 files, 10 modified, 9 added (~447 tokens)
...
## binding/bson.go (3 symbols)
  [+]      Name                                     L16  (new, 3 lines)
  [+]      Bind                                     L20  (new, 7 lines)
  [+]      BindBody                                 L28  (new, 3 lines)

## context.go (3 symbols)
  [~]      <const>                                  L31  (body, 13→14 lines)
  [~]      Negotiate                                L1357  (body, 30→34 lines)
  [+]      BSON                                     L1242
...
```

The diff reports changes per function: what was added, what was removed, and whether a signature or only a body changed. Narrow it with `--scope`, or summarise a range of commits with `--log`.

## Why

I built tilth after watching AI agents make 6 tool calls to find one function: glob, read, "too big", grep, read again, read another file.

An agent with grep can find where a piece of text appears. To find out who calls a function, it has to read files and work out the answer, and that answer can be wrong without anyone noticing. tilth parses the code and computes the answer. The outline shows what is in a file, search shows where things are defined, and `--section` returns the lines you asked for.

## What it doesn't do

- tilth parses syntax. It does not resolve types.
- Callers and dependencies are matched by name. tilth cannot tell apart two functions with the same name in different modules, and it cannot follow dynamic dispatch.
- A language without a tree-sitter grammar in tilth gets text search and plain file reading only.

## Install

```bash
cargo install tilth
# or
npx tilth
```

Prebuilt binaries are on the [releases page](https://github.com/jahala/tilth/releases).

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
tilth install antigravity-cli  # ~/.gemini/config/mcp_config.json
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

Add `--edit` to turn on hash-anchored file editing (see [Edit mode](#edit-mode)):

```bash
tilth install claude-code --edit
```

For an MCP client that is not in the list, the server entry is the same everywhere. Only the config file location and the top-level key vary (`mcpServers` for most hosts, `amp.mcpServers` for Amp, TOML syntax for Codex):

```json
{
  "mcpServers": {
    "tilth": {
      "command": "tilth",
      "args": ["--mcp"]
    }
  }
}
```

For edit mode, use `"args": ["--mcp", "--edit"]`.

You can also call tilth from a shell. See [AGENTS.md](./AGENTS.md) for the MCP agent prompt, or [skills/SKILL.md](./skills/SKILL.md) for a Claude Code skill.

### Smaller models

Smaller models sometimes reach for their built-in Bash and Grep tools even when tilth is available. To make tilth the only route, turn off the tools it overlaps with:

```bash
claude --disallowedTools "Bash,Grep,Glob"
```

## How it decides what to show

| Input | Behaviour |
|-------|-----------|
| 0 bytes | `[empty]` |
| Binary | `[skipped]` with mime type |
| Generated (lockfiles, .min.js) | `[generated]` |
| Up to ~6,000 tokens | Full content with line numbers |
| Over ~6,000 tokens | Structural outline with line ranges |

The rule counts tokens, so a 1-line minified bundle gets outlined and a 120-line module prints whole.

When the output is piped to another program, the command-line tool prints the full file, the way `cat` would. MCP reads and terminal reads follow the table.

## Edit mode

Install with `--edit` to add `tilth_write` and switch `tilth_read` to hashline output:

```
42:a3f|  let x = compute();
43:f1b|  return x;
```

`tilth_write` takes one or more files in a single call. Each file uses one of three modes: `hash` (the default, which replaces lines at the hash anchors shown above), `overwrite` (the whole file, and create-only unless `overwrite: true`), and `append`. In hash mode the anchors must match the last read. If the file changed in the meantime, the hashes no longer match, and tilth rejects the edit and shows the current content:

```json
{
  "files": [
    {
      "path": "src/auth.ts",
      "edits": [
        { "start": "42:a3f", "content": "  let x = recompute();" },
        { "start": "44:b2c", "end": "46:e1d", "content": "" }
      ]
    }
  ]
}
```

Large files still come back as an outline first. Use `section` to get hashlined content for the part you need.

Inspired by [The Harness Problem](https://blog.can.ac/2026/02/12/the-harness-problem/).

## Usage

```bash
tilth <path>                      # read file (outline if large)
tilth <path> --section 45-89      # exact line range
tilth <path> --section "## Foo"   # markdown heading
tilth <path> --full               # force full content
tilth <symbol> --scope <dir>      # definitions + usages
tilth <symbol> --expand=5         # inline source for top 5 matches
tilth <symbol> --full             # up to 100 matches instead of 10
tilth <symbol> --callers          # find call sites (structural)
tilth grok <symbol>               # everything about one symbol
tilth <path> --deps               # what a file uses, and what uses it
tilth "TODO: fix" --scope <dir>   # content search
tilth "/<regex>/" --scope <dir>   # regex search
tilth "*.test.ts" --scope <dir>   # glob files
tilth diff HEAD~1                 # structural diff (function-level)
tilth --map --scope <dir>         # codebase skeleton (CLI only)
```

`--map` is a command-line feature only. It is not offered as an MCP tool, because in testing agents called it far more often than it helped.

## Speed

Median of 15 runs of the release build (main after v0.10.1) on an Apple M5 Pro. Each time includes process startup. MCP mode pays startup once.

| Operation | Gin (130 files) | FastAPI (2,700 files) |
|-----------|-----------------|-----------------------|
| File read | 3 ms | 3 ms |
| Glob | 5 ms | 9 ms |
| Callers | 13 ms | 64 ms |
| Deps | 17 ms | 28 ms |
| Symbol search | 20 ms | 77 ms |
| Content search | 37 ms | 80 ms |
| Grok | 37 ms | 91 ms |
| Map | 52 ms | 184 ms |

Search walks the whole tree, so time grows with the size of the repository.

## What's inside

tilth is about 35,000 lines of Rust with no runtime dependencies.

- **tree-sitter** parses 17 languages: Rust, TypeScript, TSX, JavaScript, Python, Go, Java, Scala, C, C++, Ruby, PHP, C#, Swift, Kotlin, Elixir and Bash. tilth uses it for definitions, callees, callers and outlines. Dockerfile and Make files are recognised but not parsed.
- **ripgrep's crates** (`grep-regex`, `grep-searcher`) run content search.
- The **ignore** crate walks directories in parallel with `.tilthignore` support and optional gitignore handling. It always skips common build and dependency folders such as `node_modules`, `target` and `.venv`.
- `.tilthignore` is a custom ignore file that works like `.gitignore` but is always honored, even when gitignore rules are bypassed. Place it in any directory to exclude its contents from walks.
- This release adds opt-in `.gitignore`, `.ignore` and Git exclude handling. In MCP mode, call `tilth_config` with `{"action":"set","respect_gitignore":true}` to honor these rules for the lifetime of the server. `TILTH_RESPECT_GITIGNORE=1` is the startup equivalent. CLI `--no-respect-gitignore` bypasses them; per-call `gitignore: false` on `tilth_search` and `tilth_list` does the same for one call. `.tilthignore` and built-in junk-directory exclusions always remain active.
- **memmap2** reads files through memory maps.
- **DashMap** holds the outline cache, which is invalidated when a file's modified time changes.

Definitions and usages are searched in parallel through `rayon::join`. Callees are resolved when a definition is expanded. tilth extracts the callee names with tree-sitter queries and looks them up in the file's own outline and in the files it imports. The callers query runs the same patterns in reverse across the codebase, with a `memchr` pre-filter to skip files that cannot match.

tilth makes no network calls, so your code stays on your machine.

## A note on benchmarks

Earlier versions of this README carried a cost benchmark. It was measured on v0.5.0 with early-2026 models, and it no longer describes the tool or the models people use today. We have retired it and have not replaced it, so tilth makes no cost claim. The harness and its task definitions are preserved under the `benchmark-archive` tag.

## Name

**tilth** is the state of soil that has been prepared for planting. Your codebase is the soil, and tilth gives it structure so you can find where to dig.

## Support

[!["Buy Me A Coffee"](https://www.buymeacoffee.com/assets/img/custom_images/orange_img.png)](https://buymeacoffee.com/jahala)

## License

MIT
