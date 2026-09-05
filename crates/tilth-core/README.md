# tilth-core

tilth's parsing substrate as a library: tree-sitter outlines in fourteen
languages, language detection, test-file classification, import resolution,
callers, and file-level dependency analysis. The `tilth` binary is its first
consumer; the garden's diff judge is its second.

## The documented surface

Everything re-exported at the crate root is the API. The modules behind it are
the binary's internals and carry no stability promise.

| Item | What it does |
|---|---|
| `detect_file_type(&Path) -> FileType` | language by extension or filename; `FileType::Code(Lang)` for code |
| `Lang`, `FileType` | the languages tilth parses and the file kinds it outlines |
| `get_outline_entries(&str, Lang) -> Vec<OutlineEntry>` | definitions with kind, name, line range, signature, children |
| `OutlineEntry`, `OutlineKind` | the outline's shape |
| `is_test_file(&Path) -> bool` | `.test.`, `.spec.`, `__tests__/` |
| `test_entries(&str, Lang) -> Vec<TestEntry>` | `describe`/`it`/`test` structure as data, with depth and lines |
| `TestEntry`, `TestKind` | suite or case |
| `is_import_line(&str, Lang) -> bool` | whether a line is an import statement |
| `extract_import_source(&str, Option<Lang>) -> String` | the module an import names |
| `is_external(&str, Lang) -> bool` | whether that module is outside the project |
| `resolve_related_files_with_content(&Path, &str) -> Vec<PathBuf>` | local files a file's imports resolve to |
| `find_callers_batch(&HashSet<String>, &Path, &BloomFilterCache, Option<&str>, usize)` | call sites of a set of symbols across a scope |
| `CallerMatch`, `BloomFilterCache` | a call site; the per-file pre-filter the walk shares |
| `analyze_deps(&Path, &Path, &BloomFilterCache) -> Result<DepsResult, TilthError>` | what a file uses and what uses it |
| `DepsResult`, `LocalDep`, `Dependent` | the dependency report's shape |
| `TilthError` | the crate's error, `std::error::Error + Send + Sync` |

The acceptance test in `tests/api.rs` exercises each of these from outside the
crate on TypeScript, Python, Rust, and Go samples, and on a two-file callers
case.

## Depending on it

Until the crate is on crates.io, pin a commit on the tilth repository:

```toml
[dependencies]
tilth-core = { git = "https://github.com/jahala/tilth", rev = "<commit>" }
```

Versions move in lockstep with `tilth`; the release workflow publishes
`tilth-core` first, then the binary.
