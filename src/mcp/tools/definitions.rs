use serde_json::Value;

pub(in crate::mcp) fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let read_desc = include_str!("../../../prompts/tools/read.md");
    let mut tools = vec![
        serde_json::json!({
            "name": "tilth_search",
            "description": "Search for symbols, text, or regex patterns in code. Replaces grep/rg and the host Grep tool — use this for all code search. Symbol search returns definitions first (via tree-sitter AST), then usages, with full source code inlined for top matches. Content search finds literal text. Regex search supports full regex patterns. For cross-file tracing, pass comma-separated symbol names (max 5). Omitting `kind` runs a merged default search — symbol, content, and caller results in one call (`## symbol/content/caller results`); set `kind` to narrow to a single mode.",
            "inputSchema": {
                "type": "object",
                "required": ["queries"],
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["query"],
                            "properties": {
                                "query": {"type": "string", "description": "Symbol name, text string, or regex pattern. e.g. 'resolve_dependencies' or 'ServeHTTP,Next' for multi-symbol lookup."},
                                "glob": {"type": "string"},
                                "kind": {"type": "string", "enum": ["any", "symbol", "content", "regex", "callers"]}
                            }
                        },
                        "minItems": 1,
                        "maxItems": 10,
                        "description": "Array of query objects, each run independently with optional per-entry `kind`/`glob` overriding the top-level values. A single-element array runs one search and returns a clean result; multiple entries concatenate under `## query: <q>` headers. For one search pass `queries: [{query: \"foo\"}]`."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Only use scope to search a specific subdirectory. DO NOT USE scope if you want to search the current working directory (initial search)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["any", "symbol", "content", "regex", "callers"],
                        "default": "any",
                        "description": "Search type. Omit or 'any' for the merged default: symbol + content + caller results in one call. symbol: structural definitions + usages. content: literal text. regex: regex pattern. callers: find all call sites of a symbol."
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
                    "if_modified_since": {
                        "type": "string",
                        "description": "ISO-8601 timestamp. Result sections for files unchanged since this return `(unchanged @ <ts>)` stubs instead of bodies."
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
                        "description": "File paths (max 20). Suffix grammar on each path: `path#n-m` (line range), `path#n` (from line n), `path### Heading` (markdown heading), `path#symbol_name` (code symbol). Example: paths: [\"src/foo.rs#do_thing\", \"README.md#10-40\"]. Pass every file you need in one call; for a single file use a one-element array. Singular `path` is not accepted."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "full", "signature", "stripped"],
                        "default": "auto",
                        "description": "Defaults to `auto` — omit unless you need to override smart-sizing. auto: small files return full; large code returns signature lines with `<line>:<hash>` prefixes; large markdown returns headings + preview. full forces full content. signature forces outline. stripped removes plain comments, debug logs, and blank-line runs (non-editable view)."
                    },
                    "if_modified_since": {
                        "type": "string",
                        "description": "ISO-8601 timestamp. Files unchanged since this return `(unchanged @ <ts>)` stubs."
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
            "description": "Find files matching glob patterns. Replaces find/ls/pwd and the host Glob tool — use this for all file discovery. Returns matched file paths sorted by relevance with token size estimates. Pass one or more globs in `patterns`.",
            "inputSchema": {
                "type": "object",
                "required": ["patterns"],
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Glob patterns to run against the same scope, e.g. ['*'] (list directory), ['*.rs'], ['src/**/*.ts'], or ['*.rs', '*.toml'] for several at once. Each pattern emits its own `# Glob: ...` block, separated by a blank line. Capped at 20."
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
            "name": "tilth_list",
            "description": "List files matching glob patterns as a directory tree. Replaces `ls -R`/`tree` — use this to see project structure with token-size rollups per directory. Pass `patterns` to combine several globs into one tree.",
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
                        "description": "Directory to root the tree at. Default: current working directory."
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
                                    "description": "Absolute or relative file path."
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
        let hash_branch = &all_of[0];
        assert_eq!(hash_branch["then"]["required"][0], "edits");
        // Content branch: when mode in {overwrite, w, append, a}, require content.
        let content_branch = &all_of[1];
        assert_eq!(content_branch["then"]["required"][0], "content");
        let content_modes = content_branch["if"]["properties"]["mode"]["enum"]
            .as_array()
            .expect("content-mode enum present");
        let modes: Vec<&str> = content_modes.iter().filter_map(|v| v.as_str()).collect();
        assert!(modes.contains(&"overwrite") && modes.contains(&"append"));
    }

    /// Compile the per-file `items` sub-schema and exercise the mode-required
    /// rules end-to-end. Structural assertions above protect against
    /// silent shape changes; this protects against drift in semantics.
    #[test]
    fn tilth_write_items_schema_enforces_mode_required_fields() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present in edit mode");
        let items_schema = write["inputSchema"]["properties"]["files"]["items"].clone();
        let compiled = jsonschema::JSONSchema::compile(&items_schema)
            .expect("tilth_write items schema must be a valid JSON Schema");

        let valid_hash = serde_json::json!({
            "path": "src/x.rs",
            "mode": "hash",
            "edits": [{"start": "1:abc", "content": "y"}],
        });
        let valid_default = serde_json::json!({
            "path": "src/x.rs",
            "edits": [{"start": "1:abc", "content": "y"}],
        });
        let valid_overwrite = serde_json::json!({
            "path": "src/x.rs", "mode": "overwrite", "content": "y",
        });
        let valid_append = serde_json::json!({
            "path": "src/x.rs", "mode": "append", "content": "y",
        });
        for v in [&valid_hash, &valid_default, &valid_overwrite, &valid_append] {
            assert!(compiled.is_valid(v), "expected valid instance to pass: {v}");
        }

        // Mode-required field omissions must fail validation, not merely
        // produce per-file dispatcher errors.
        let hash_missing_edits = serde_json::json!({"path": "x.rs", "mode": "hash"});
        let default_missing_edits = serde_json::json!({"path": "x.rs"});
        let overwrite_missing_content = serde_json::json!({"path": "x.rs", "mode": "overwrite"});
        let append_missing_content = serde_json::json!({"path": "x.rs", "mode": "append"});
        for v in [
            &hash_missing_edits,
            &default_missing_edits,
            &overwrite_missing_content,
            &append_missing_content,
        ] {
            assert!(
                !compiled.is_valid(v),
                "expected invalid instance to fail: {v}"
            );
        }
    }

    /// `tilth_search` schema must stay aligned with the runtime: `any` is a
    /// valid `kind` (top-level + per-entry), `any` is the default, and the
    /// root requires `queries` so `{}` (and the dropped singular `query`) are
    /// rejected client-side.
    #[test]
    fn tilth_search_schema_matches_runtime_kind_and_requires_a_query() {
        let tools = tool_definitions(false);
        let search = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_search"))
            .expect("tilth_search tool definition present");
        let schema = &search["inputSchema"];

        let kind = &schema["properties"]["kind"];
        let kind_enum: Vec<&str> = kind["enum"]
            .as_array()
            .expect("kind enum present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            kind_enum.contains(&"any"),
            "top-level kind enum must include 'any': {kind_enum:?}"
        );
        assert_eq!(
            kind["default"], "any",
            "top-level kind default must be 'any'"
        );

        let entry_enum: Vec<&str> = schema["properties"]["queries"]["items"]["properties"]["kind"]
            ["enum"]
            .as_array()
            .expect("per-entry kind enum present")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            entry_enum.contains(&"any"),
            "per-entry kind enum must include 'any': {entry_enum:?}"
        );

        let compiled = jsonschema::JSONSchema::compile(schema)
            .expect("tilth_search inputSchema must be a valid JSON Schema");
        assert!(
            !compiled.is_valid(&serde_json::json!({})),
            "empty args must fail: queries is required"
        );
        assert!(
            !compiled.is_valid(&serde_json::json!({"query": "x"})),
            "the singular `query` key was dropped — only `queries` is accepted"
        );
        assert!(compiled.is_valid(&serde_json::json!({"queries": [{"query": "x"}]})));
        assert!(compiled.is_valid(&serde_json::json!({"queries": [{"query": "x", "kind": "any"}]})));
    }

    /// Regression for issue #47: OpenAI/Codex's strict function-schema
    /// validator rejects any tool whose `parameters` (inputSchema) is not a
    /// plain top-level object, or that uses `oneOf`/`anyOf`/`allOf`/`enum`/`not`
    /// at the top level. Anthropic/Claude tolerates the looser shape, so this
    /// only surfaced under Codex. Every advertised tool's inputSchema must
    /// satisfy the rule (nested `enum`/`allOf` under `properties` is fine —
    /// the constraint is top-level only).
    #[test]
    fn tool_schemas_are_openai_strict_compatible() {
        const FORBIDDEN_TOP_LEVEL: [&str; 5] = ["oneOf", "anyOf", "allOf", "enum", "not"];
        // edit_mode=true advertises the widest tool set (includes tilth_write).
        for tool in tool_definitions(true) {
            let name = tool["name"].as_str().expect("tool name present");
            let schema = &tool["inputSchema"];
            assert_eq!(
                schema["type"].as_str(),
                Some("object"),
                "{name}: inputSchema top level must be type 'object'"
            );
            let obj = schema.as_object().expect("inputSchema is an object");
            for key in FORBIDDEN_TOP_LEVEL {
                assert!(
                    !obj.contains_key(key),
                    "{name}: inputSchema must not use top-level '{key}' \
                     (OpenAI/Codex rejects it — see issue #47)"
                );
            }
        }
    }

    /// Tool names must be unique. A duplicate function name is itself an
    /// invalid request under OpenAI/Codex. Regression for the duplicate
    /// `tilth_list` registration removed alongside #47.
    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tool in tool_definitions(true) {
            let name = tool["name"]
                .as_str()
                .expect("tool name present")
                .to_string();
            assert!(
                seen.insert(name.clone()),
                "duplicate tool definition: {name}"
            );
        }
    }
}
