# tilth-core

The parsing substrate under [tilth](https://github.com/jahala/tilth), as a
library. It detects a file's language, parses it with tree-sitter into outline
entries and test structure, resolves imports, finds call sites, and analyses
file-level dependencies. It returns data; the `tilth` binary renders that data
for its CLI and MCP tools.

## The public surface

The items re-exported at the crate root are the API. The modules behind them
are shared with the `tilth` binary and carry no stability promise.

| Item | What it does |
|---|---|
| `detect_file_type(&Path) -> FileType` | The file's kind by extension or name; `FileType::Code(Lang)` for code. |
| `Lang`, `FileType` | The languages tilth parses and the file kinds it outlines. |
| `get_outline_entries(&str, Lang) -> Vec<OutlineEntry>` | Definitions with kind, name, line range, signature and children. |
| `OutlineEntry`, `OutlineKind` | The shape of an outline. A JS/TS export clause gives one `Export` entry per specifier, named as exported, with `local_name` set where it renames. |
| `is_test_file(&Path) -> bool` | Test files by path: `.test.`, `.spec.`, `__tests__/`. |
| `test_entries(&str, Lang) -> Vec<TestEntry>` | `describe`/`it`/`test` structure as data, with lines and depth. |
| `TestEntry`, `TestKind` | One suite or case. |
| `extract_imports(&str, Lang) -> Vec<Import>` | Every module a file imports, one `Import` per module: a Go import block, a Rust `use` group and Python's `import os, json` each give one per module named. |
| `Import` | One module: its `source`, the `line` that names it, and its statement's `start_line` and `end_line`. |
| `is_import_line(&str, Lang) -> bool` | Whether a line starts an import statement. |
| `extract_import_source(&str, Option<Lang>) -> String` | The source an import statement's text names, read as one source. |
| `is_external(&str, Lang) -> bool` | Whether that module lies outside the project. |
| `resolve_related_files_with_content(&Path, &str) -> Vec<PathBuf>` | The local files a file's imports resolve to. |
| `find_callers_batch(&HashSet<String>, &Path, &BloomFilterCache, Option<&str>, usize)` | Call sites of a set of symbols across a directory. |
| `CallerMatch`, `BloomFilterCache` | One call site; the per-file pre-filter the walk shares. |
| `analyze_deps(&Path, &Path, &BloomFilterCache) -> Result<DepsResult, TilthError>` | What a file uses and what uses it. |
| `DepsResult`, `LocalDep`, `Dependent` | The shape of a dependency report. |
| `TilthError` | The crate's error type: `std::error::Error + Send + Sync`. |

`tests/api.rs` exercises each item from outside the crate.

## Versions

`tilth-core` and `tilth` share one version number and are released together.
The release workflow publishes `tilth-core` first, because the binary depends
on it by version.
