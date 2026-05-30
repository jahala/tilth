use serde_json::Value;

pub(in crate::mcp) fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let read_desc = if edit_mode {
        "Read a file with smart outlining. Replaces cat/head/tail and the host Read tool — \
         use this for all file reading. Output uses hashline format (line:hash|content) — \
         the line:hash anchors are required by tilth_edit. Small files return full hashlined content. \
         Large files return a structural outline (no hashlines); use `sections` to get hashlined \
         content for the lines you want to edit — a single-element array for one range, several \
         for disjoint slices in one call. Use `full` to force complete content. \
         Always pass `paths` as an array of file paths — a single-element array reads one file."
    } else {
        "Read a file with smart outlining. Replaces cat/head/tail and the host Read tool — \
         use this for all file reading. Small files return full content. Large files return \
         a structural outline (functions, classes, imports) so you see the shape without \
         consuming your context window. Use `sections` to read specific line ranges or headings — \
         a single-element array for one range, several for disjoint slices in one call. \
         Use `full` to force complete content. Always pass `paths` as an array of file paths — a single-element array reads one file."
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
                        "description": "Symbol name, text string, or regex pattern to search for. e.g. 'resolve_dependencies' or 'ServeHTTP,Next' for multi-symbol lookup."
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
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_read",
            "description": read_desc,
            "inputSchema": {
                "type": "object",
                "required": ["paths"],
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "File paths to read (max 20). ALWAYS an array — use a single-element array for one file. Each file gets independent smart handling. Singular `path` is not accepted."
                    },
                    "sections": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Line ranges or headings to read from the file. Each entry is a line range e.g. '45-89' or a heading e.g. '## Architecture'. ALWAYS an array — use a single-element array for one range. Single-file only (one entry in `paths`). Bypasses smart view. Emits each block in user-supplied order, separated by `─── lines X-Y ───` delimiters. Capped at 20 ranges."
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
    ];

    if edit_mode {
        tools.push(serde_json::json!({
            "name": "tilth_edit",
            "description": "Batch edit one or more files in one call using hashline anchors from tilth_read. ALWAYS group edits to multiple files into a single tilth_edit call — never call tilth_edit twice in a row. Each file is processed independently (best-effort): a hash mismatch on one file does not block the others; results are reported per file. Partial success returns isError: false — scan the per-file `## <path>` sections for failures rather than trusting the top-level status. A parse error on one edit invalidates ALL edits for that file (none applied); retry the whole file after fixing the malformed entry. Each file path may appear at most once per call. Max 20 files per call.",
            "inputSchema": {
                "type": "object",
                "required": ["files"],
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "One entry per file. Use a single-element array for a single-file edit. Each path must be unique within the call.",
                        "items": {
                            "type": "object",
                            "required": ["path", "edits"],
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Absolute or relative file path to edit."
                                },
                                "edits": {
                                    "type": "array",
                                    "minItems": 1,
                                    "description": "Edit operations for this file, applied atomically per file.",
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
                                }
                            }
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
    fn tilth_read_schema_is_paths_only() {
        for edit_mode in [false, true] {
            let tools = tool_definitions(edit_mode);
            let read = tools
                .iter()
                .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_read"))
                .expect("tilth_read tool definition present");
            let schema = &read["inputSchema"];
            let props = &schema["properties"];
            assert!(props.get("paths").is_some(), "paths must be present");
            assert!(
                props.get("path").is_none(),
                "singular path must be removed (paths-only API)"
            );
            assert!(props.get("sections").is_some(), "sections must be present");
            assert!(
                props.get("section").is_none(),
                "singular section must be removed (sections-only API)"
            );
            let required = schema["required"]
                .as_array()
                .expect("tilth_read schema must declare required");
            assert!(
                required.iter().any(|v| v == "paths"),
                "paths must be required"
            );
        }
    }
}
