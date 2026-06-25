use serde_json::Value;

pub(in crate::mcp) fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let read_desc = if edit_mode {
        "Read a file with smart outlining. Replaces cat/head/tail and the host Read tool — \
         use this for all file reading. Output uses hashline format (line:hash|content) — \
         the line:hash anchors are required by tilth_write. Small files return full hashlined content. \
         Large files return a structural outline (no hashlines); use `section` to get hashlined \
         content for the lines you want to edit. Use `sections` to grab several disjoint slices \
         from the same file in one call. Use `full` to force complete content. \
         Use `paths` to read multiple files in one call."
    } else {
        "Read a file with smart outlining. Replaces cat/head/tail and the host Read tool — \
         use this for all file reading. Small files return full content. Large files return \
         a structural outline (functions, classes, imports) so you see the shape without \
         consuming your context window. Use `section` to read a specific line range or heading. \
         Use `sections` to grab several disjoint slices from the same file in one call. \
         Use `full` to force complete content. Use `paths` to read multiple files in one call."
    };
    let mut tools = vec![
        serde_json::json!({
            "name": "tilth_search",
            "description": "Search for symbols, text, or regex patterns in code. Replaces grep/rg and the host Grep tool — use this for all code search. Symbol search returns definitions first (via tree-sitter AST), then usages, with full source code inlined for top matches. Content search finds literal text. Regex search supports full regex patterns. For cross-file tracing, pass comma-separated symbol names (max 5).",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name, text string, or regex pattern to search for. e.g. 'resolve_dependencies' or 'ServeHTTP,Next' for comma-separated multi-symbol lookup (max 5)."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Only use scope to search a specific subdirectory. DO NOT USE scope if you want to search the current working directory (initial search)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["symbol", "content", "regex", "callers"],
                        "default": "symbol",
                        "description": "Search type. symbol: structural definitions + usages. content: literal text. regex: regex pattern. callers: find all call sites of a symbol."
                    },
                    "expand": {
                        "type": "number",
                        "default": 2,
                        "description": "Number of top matches to expand with full source code. Definitions show the full function/class body. Usages show ±10 context lines."
                    },
                    "context": {
                        "type": "string",
                        "description": "Path to the file the agent is currently editing. Boosts ranking of matches in the same directory or package."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens in response."
                    },
                    "glob": {
                        "type": "string",
                        "description": "File pattern filter. Whitelist: \"*.rs\" (only Rust files). Exclude: \"!*.test.ts\" (skip test files). Brace expansion: \"*.{go,rs}\" (Go and Rust). Path patterns: \"src/**/*.ts\"."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless `scope` is absolute. Must be an absolute path. A RELATIVE `scope` is anchored under `root`; an absolute `scope` is used as-is. The server cannot see your shell cwd, so a relative `scope` with no absolute `root` is refused."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_read",
            "description": read_desc,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path to read. A relative path requires an absolute `root`; the server cannot see your shell cwd."
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multiple file paths to read in one call. Each file gets independent smart handling. Saves round-trips vs multiple single reads."
                    },
                    "section": {
                        "type": "string",
                        "description": "Line range e.g. '45-89', or heading e.g. '## Architecture'. Bypasses smart view. Use `sections` for multiple ranges."
                    },
                    "sections": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multiple ranges from the same file in one call. Each entry is a line range or heading. Emits each block in user-supplied order, separated by `─── lines X-Y ───` delimiters. Mutually exclusive with `section`. Capped at 20 ranges."
                    },
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Legacy alias for mode='full'. Force full content output, bypass smart outlining."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "full", "signature", "stripped"],
                        "default": "auto",
                        "description": "Read view. auto: smart default. full: full content. signature: hash-prefixed declarations only. stripped: whole-file content with plain comments/debug logs/extra blanks removed."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens in response."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless every path in `paths` is absolute. Must be an absolute path. Every RELATIVE file path is anchored under `root`; absolute paths are used as-is. The server cannot see your shell cwd, so a relative path with no absolute `root` is refused."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_files",
            "description": "Find files matching a glob pattern. Replaces find/ls/pwd and the host Glob tool — use this for all file discovery. Returns matched file paths sorted by relevance with token size estimates. Use `patterns` to run several globs in one call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern e.g. '*' (list directory), '*.rs', 'src/**/*.ts'. Use `patterns` for multiple globs."
                    },
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multiple glob patterns to run in one call against the same scope. Each pattern emits its own `# Glob: ...` block, separated by a blank line. Mutually exclusive with `pattern`. Capped at 20."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Only use scope to list a specific subdirectory. DO NOT USE scope if you want to list the current working directory."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens in response."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless `scope` is absolute. Must be an absolute path. A RELATIVE `scope` is anchored under `root`; an absolute `scope` is used as-is. The server cannot see your shell cwd, so a relative `scope` with no absolute `root` is refused."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_deps",
            "description": "Blast-radius check before breaking changes. Shows what a file imports (local + external) and what other files call its exports, with symbol-level detail. Use ONLY when your planned edit changes a function signature, removes/renames an export, or modifies behavior that callers rely on. Do NOT use for reading files, adding new code, or internal-only changes — use tilth_read instead.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File to check before making breaking changes."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Directory to search for dependents. Default: project root."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens. Truncates 'Used by' first."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless `path` and `scope` are absolute. Must be an absolute path. RELATIVE `path`/`scope` are anchored under `root`; absolute ones are used as-is. The server cannot see your shell cwd, so a relative `path`/`scope` with no absolute `root` is refused."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_grok",
            "description": "Get everything structural about a symbol in one call — definition, body, signature, doc, callees, callers, siblings, tests. Use ONLY for 'understand this symbol' questions. Do NOT use for concept search (use tilth_search) or reading file contents (use tilth_read).",
            "inputSchema": {
                "type": "object",
                "required": ["target"],
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Symbol name, e.g. 'parse_unified_diff'. Also accepts 'src/diff/parse.rs:7' or 'Type::method'."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Subdirectory to narrow the search. Default: project root."
                    },
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Widen caps: 50 callers, 30 callees, 30 siblings, 30 tests (default 5/5/8/8)."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless `scope` is absolute. Must be an absolute path. A RELATIVE `scope` is anchored under `root`; an absolute `scope` is used as-is. The server cannot see your shell cwd, so a relative `scope` with no absolute `root` is refused."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_diff",
            "description": "Structural diff showing function-level changes. Replaces git diff. Call with no args for uncommitted changes overview.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Diff source: 'uncommitted' (default), 'staged', or a git ref (e.g. 'HEAD~1', 'main..feat'). Ignored when a, b, patch, or log is set."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Restrict diff output to a specific file or directory path."
                    },
                    "a": {
                        "type": "string",
                        "description": "First file for a file-to-file diff. Must be used together with b."
                    },
                    "b": {
                        "type": "string",
                        "description": "Second file for a file-to-file diff. Must be used together with a."
                    },
                    "patch": {
                        "type": "string",
                        "description": "Path to a .patch file to parse instead of running git diff."
                    },
                    "log": {
                        "type": "string",
                        "description": "Git log range (e.g. 'HEAD~5..HEAD') — shows per-commit structural summaries."
                    },
                    "search": {
                        "type": "string",
                        "description": "Filter output to symbols or files matching this substring (case-insensitive)."
                    },
                    "blast": {
                        "type": "boolean",
                        "default": false,
                        "description": "Show blast-radius warnings for signature-changed symbols."
                    },
                    "expand": {
                        "type": "number",
                        "default": 0,
                        "description": "Number of changed symbols to expand with full source context."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens in response."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_savings",
            "description": "Report tokens tilth saved this session vs naive grep/cat (conservative lower bound). Call ONLY when the user explicitly asks how much tilth saved — never proactively.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
    ];

    if edit_mode {
        tools.push(serde_json::json!({
            "name": "tilth_write",
            "description": "Batch write one or more files in one call. Replaces the host Edit and Write tools — DO NOT use those. Three per-file modes: `hash` (default — replace lines at hash anchors from tilth_read), `overwrite` (whole file; create-only by default — pass `overwrite: true` to replace an existing file), `append` (append `content`, creates if absent). overwrite/append responses echo the file's hashlines so you can chain anchored edits in the next call without re-reading. ALWAYS group writes to multiple files into a single tilth_write call — never call tilth_write twice in a row. Each file is processed independently (best-effort): a failure on one file does not block the others; results are reported per file. Partial success returns isError: false — scan the per-file `## <path>` sections for failures rather than trusting the top-level status. A parse error on one edit invalidates ALL edits for that file (none applied); retry the whole file after fixing the malformed entry. Each file path may appear at most once per call. Max 20 files per call. Example overwrite (new file): `tilth_write(files: [{path: \"src/new.rs\", mode: \"overwrite\", content: \"fn main(){}\\n\"}])`.",
            "inputSchema": {
                "type": "object",
                "required": ["files"],
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "One entry per file. Use a single-element array for a single-file write. Each path must be unique within the call.",
                        "items": {
                            "type": "object",
                            "required": ["path"],
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Absolute or relative file path. A relative path requires an absolute `root`; the server cannot see your shell cwd."
                                },
                                "mode": {
                                    "type": "string",
                                    "enum": ["hash", "h", "overwrite", "w", "append", "a"],
                                    "default": "hash",
                                    "description": "Write mode. hash (default): replace lines at hash anchors via `edits`. overwrite: write whole file from `content`; create-only by default — set `overwrite: true` to replace existing. append: append `content`, creates if absent."
                                },
                                "edits": {
                                    "type": "array",
                                    "minItems": 1,
                                    "description": "Hash-mode only: edit operations for this file, applied atomically per file.",
                                    "items": {
                                        "type": "object",
                                        "required": ["start", "content"],
                                        "properties": {
                                            "start": {
                                                "type": "string",
                                                "description": "Start anchor: 'line:hash' (e.g. '42:a3f'). Hash from tilth_read hashline output."
                                            },
                                            "end": {
                                                "type": "string",
                                                "description": "End anchor: 'line:hash'. If omitted, replaces only the start line."
                                            },
                                            "content": {
                                                "type": "string",
                                                "description": "Replacement text (can be multi-line). Empty string to delete the line(s)."
                                            }
                                        }
                                    }
                                },
                                "content": {
                                    "type": "string",
                                    "description": "overwrite / append mode only: the file contents (overwrite) or text to append (append)."
                                },
                                "overwrite": {
                                    "type": "boolean",
                                    "default": false,
                                    "description": "overwrite mode only: when true, replace an existing file. Default false fails with `AlreadyExists` so you don't clobber by accident."
                                }
                            },
                            "allOf": [
                                {
                                    "if": {"properties": {"mode": {"enum": ["hash", "h"]}}},
                                    "then": {"required": ["edits"]}
                                },
                                {
                                    "if": {
                                        "required": ["mode"],
                                        "properties": {
                                            "mode": {"enum": ["overwrite", "w", "append", "a"]}
                                        }
                                    },
                                    "then": {"required": ["content"]}
                                }
                            ]
                        }
                    },
                    "diff": {
                        "type": "boolean",
                        "default": false,
                        "description": "Set true to include a compact diff of changes in the response per file."
                    }
                }
            }
        }));
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilth_write_schema_requires_mode_specific_fields() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present in edit mode");
        let items = &write["inputSchema"]["properties"]["files"]["items"];
        let all_of = items["allOf"]
            .as_array()
            .expect("items.allOf clauses present");
        assert_eq!(all_of.len(), 2, "expected hash-branch + content-branch");
        // Hash branch: when mode absent or in {hash, h}, require edits.
        assert_eq!(all_of[0]["then"]["required"][0], "edits");
        // Content branch: when mode in {overwrite, w, append, a}, require content.
        assert_eq!(all_of[1]["then"]["required"][0], "content");
        let content_modes = all_of[1]["if"]["properties"]["mode"]["enum"]
            .as_array()
            .expect("content-mode enum present");
        let modes: Vec<&str> = content_modes.iter().filter_map(|v| v.as_str()).collect();
        assert!(modes.contains(&"overwrite") && modes.contains(&"append"));
    }

    #[test]
    fn edit_mode_exposes_tilth_write_not_tilth_edit() {
        let tools = tool_definitions(true);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(
            names.contains(&"tilth_write"),
            "tilth_write must be exposed"
        );
        assert!(
            !names.contains(&"tilth_edit"),
            "tilth_edit must be renamed away"
        );
    }
}
