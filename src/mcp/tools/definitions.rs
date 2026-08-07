use serde_json::Value;

pub(in crate::mcp) fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let read_desc = if edit_mode {
        "AST-aware file reading with outlines, ranges, batching, and edit-mode hashes. Example: tilth_read({\"path\":\"src/cache.rs\",\"section\":\"45-89\",\"root\":\"/abs/checkout\"}). Copy line:hash anchors into tilth_write; use paths for file batches and sections for disjoint slices."
    } else {
        "AST-aware file reading with outlines, ranges, and batching. Example: tilth_read({\"path\":\"src/cache.rs\",\"section\":\"45-89\",\"root\":\"/abs/checkout\"}). Use paths for file batches and sections for disjoint slices."
    };
    let mut tools = vec![
        serde_json::json!({
            "name": "tilth_search",
            "annotations": { "readOnlyHint": true },
            "description": "Unified code search across symbols, text, regex, and callers. Start here for exploration. Example: tilth_search({\"query\":\"handleRequest\",\"kind\":\"symbol\",\"root\":\"/abs/checkout\"}). Comma-separate up to 5 related queries. Relative scopes require root.",
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
                        "description": "Absolute project root; anchors relative paths and scopes. Required with any relative path/scope."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_read",
            "annotations": { "readOnlyHint": true },
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
                        "description": "Absolute project root; anchors relative paths and scopes. Required with any relative path/scope."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_list",
            "annotations": { "readOnlyHint": true },
            "description": "List files when you have no symbol or text query. Example: tilth_list({\"patterns\":[\"*.rs\"],\"scope\":\"src\",\"root\":\"/abs/checkout\"}). Prefer tilth_search for discovery.",
            "inputSchema": {
                "type": "object",
                "required": ["patterns"],
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Glob patterns rendered into one tree, e.g. ['*.rs'] or ['*.rs', '*.toml']. Capped at 20."
                    },
                    "depth": {
                        "type": "number",
                        "description": "Cap directory depth (1 = top-level only)."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Directory to root the tree at. DO NOT USE scope if you want to list the current working directory."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens in response."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute project root; anchors relative paths and scopes. Required with any relative path/scope."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_deps",
            "annotations": { "readOnlyHint": true },
            "description": "File dependency blast radius: imports and imported_by in one call. Example: tilth_deps({\"path\":\"src/cache.rs\",\"root\":\"/abs/checkout\"}). Relative paths require root.",
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
                        "description": "Absolute project root; anchors relative paths and scopes. Required with any relative path/scope."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_grok",
            "annotations": { "readOnlyHint": true },
            "description": "Deep dive on one symbol: definition, callers, callees, siblings, and ranked context. Example: tilth_grok({\"target\":\"parse_unified_diff\",\"root\":\"/abs/checkout\"}). Replaces search → expand → callers.",
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
                        "description": "Absolute project root; anchors relative paths and scopes. Required with any relative path/scope."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_diff",
            "annotations": { "readOnlyHint": true },
            "description": "Structured git diff. Example: tilth_diff({}) for uncommitted changes or tilth_diff({\"source\":\"HEAD~1\"}) for one commit. Prefer this over shell git diff/log --patch.",
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
            "annotations": { "readOnlyHint": true },
            "description": "Report conservative token savings for this session. Call only when the user explicitly asks.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        serde_json::json!({
            "name": "tilth_scout",
            "annotations": { "readOnlyHint": true },
            "description": "Research a subsystem from a natural-language prompt. Example: tilth_scout({\"prompt\":\"where is response compression applied?\",\"scope\":\"src\",\"root\":\"/abs/checkout\"}). Use for candidate ranking; use tilth_grok for one known symbol.",
            "inputSchema": {
                "type": "object",
                "required": ["prompt"],
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Natural-language prompt describing what to find, e.g. 'parse a unified diff'."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Subdirectory to narrow the search. Default: project root."
                    },
                    "job": {
                        "type": "string",
                        "enum": ["context", "rerank"],
                        "default": "rerank",
                        "description": "Job to run: rerank (default — rrf fusion ranking + skeleton when gate fires) or context (deterministic candidate assembly only)."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute project root; anchors a relative scope"
                    }
                }
            }
        }),
    ];

    if edit_mode {
        tools.push(serde_json::json!({
            "name": "tilth_write",
            "annotations": { "readOnlyHint": false },
            "description": "Hash-anchored multi-file edits. Example: tilth_write({\"files\":[{\"path\":\"src/x.rs\",\"edits\":[{\"start\":\"2:ABCD\",\"content\":\"let y = 1;\"}]}],\"root\":\"/abs/checkout\"}). Copy anchors from edit-mode tilth_read; never invent them. For creation, use mode:\"overwrite\" with content. Partial failures are per-file.",
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
                    },
                    "root": {
                        "type": "string",
                        "description": "Optional absolute path. When provided, every RELATIVE file path in this call is anchored under `root` instead of the server's process cwd. Absolute file paths are used as-is. Use this when the server was launched from a different directory than the worktree you are editing."
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

    /// `tilth_files` was consolidated into `tilth_list`; the removed tool must
    /// no longer be advertised and `tilth_list` must stay present in both modes.
    #[test]
    fn tilth_files_folded_into_tilth_list() {
        for edit_mode in [false, true] {
            let defs = tool_definitions(edit_mode);
            let names: Vec<&str> = defs
                .iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
                .collect();
            assert!(
                !names.contains(&"tilth_files"),
                "tilth_files must not be advertised (folded into tilth_list)"
            );
            assert!(
                names.contains(&"tilth_list"),
                "tilth_list must remain advertised"
            );
        }
    }

    #[test]
    fn all_tool_descriptions_fit_budget() {
        let defs = tool_definitions(true);
        assert_eq!(defs.len(), 9);
        for tool in &defs {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
            let desc = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                desc.len() <= 2_048,
                "{name} description is {} bytes (limit 2048)",
                desc.len()
            );
        }
    }
}
