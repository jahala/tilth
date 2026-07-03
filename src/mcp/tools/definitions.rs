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
                                "query": {"type": "string", "description": "Symbol name, text string, or regex pattern. e.g. 'resolve_dependencies' or 'ServeHTTP,Next' for multi-symbol lookup. Commas mean multiple symbols (works under the default and kind:symbol/callers); for mixed symbol + content terms use separate query objects instead."},
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
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless every path in `paths` is absolute. Must be an absolute path. Every RELATIVE file path is anchored under `root`; absolute paths are used as-is. The server cannot see your shell cwd, so a relative path with no absolute `root` is refused."
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
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless `scope` is absolute. Must be an absolute path. A RELATIVE `scope` (the tree root) is anchored under `root`; an absolute `scope` is used as-is. The server cannot see your shell cwd, so a relative `scope` with no absolute `root` is refused."
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
    ];

    if edit_mode {
        tools.push(serde_json::json!({
            "name": "tilth_write",
            "description": "Edit files by sending a text blob of `[path#TAG]` sections in tilth's op grammar. Replaces the host Edit and Write tools — DO NOT use those. Copy the `[path#TAG]` header and `N:content` numbered lines from a tilth_read/tilth_search edit-mode view, then write ops beneath the header. One op per line; multi-line payloads follow their op header, one payload line each (prefix `+` to force a line literal). Ops: `SWAP a.=b:` then payload (replace line range), `DEL n` / `DEL a.=b` (delete), `INS.PRE n:` / `INS.POST n:` then payload (insert before/after line n), `INS.HEAD:` / `INS.TAIL:` then payload (start/end of file), `SWAP.BLK n:` / `SWAP.BLK #symbol:` then payload (replace the tree-sitter block at a line or named symbol), `DEL.BLK n` / `DEL.BLK #symbol`, `INS.BLK.POST n:` / `INS.BLK.POST #symbol:` then payload, `REM` (delete file), `MV dest` (move/rename). Line numbers are 1-based inclusive and come from the numbered read. The TAG binds the section to the exact content you read: if the file drifted, tilth 3-way-merges your ops onto the live file; if it can't, the section is rejected — re-read and retry that file. A tagless `[path]` header seeds a NEW file (use INS.HEAD). Each section is independent (best-effort); results report per `## <path>`. Max 20 sections. Example: `tilth_write(edits: \"[src/x.rs#1A2B]\\nSWAP 2:\\n+    let y = 1;\\n\")`.",
            "inputSchema": {
                "type": "object",
                "required": ["edits"],
                "properties": {
                    "edits": {
                        "type": "string",
                        "description": "Op-grammar blob: one or more `[path#TAG]` sections, each followed by op lines. Copy the `[path#TAG]` header verbatim from the edit-mode read; never invent a TAG. To append cleanly to a newline-terminated file, prefer `INS.POST <last-content-line>` over `INS.TAIL:` (INS.TAIL inserts after the file's trailing empty row)."
                    },
                    "diff": {
                        "type": "boolean",
                        "default": false,
                        "description": "Set true to include a compact before/after diff per section."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute path to your checkout directory. REQUIRED unless every section path is absolute. Must be an absolute path. RELATIVE section paths (and MV destinations) are anchored under `root` and confined to it; ABSOLUTE section paths are also confined to `root` (or to the server's startup directory when `root` is omitted); `..` traversal and paths outside the confinement root are refused. The server cannot see your shell cwd."
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
    fn tilth_write_schema_requires_edits_blob() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present in edit mode");
        let schema = &write["inputSchema"];
        assert_eq!(schema["required"][0], "edits", "edits blob is required");
        assert_eq!(
            schema["properties"]["edits"]["type"], "string",
            "edits is a single op-grammar text blob"
        );
        // The old per-file `files` array surface is gone.
        assert!(
            schema["properties"].get("files").is_none(),
            "the per-file `files` array must be replaced by the `edits` blob"
        );
    }

    /// Compile the full inputSchema and exercise the required-blob rule
    /// end-to-end: `{}` fails (edits missing), a bare blob passes.
    #[test]
    fn tilth_write_schema_enforces_required_edits() {
        let tools = tool_definitions(true);
        let write = tools
            .iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tilth_write"))
            .expect("tilth_write tool definition present in edit mode");
        let compiled = jsonschema::JSONSchema::compile(&write["inputSchema"])
            .expect("tilth_write inputSchema must be a valid JSON Schema");

        assert!(
            !compiled.is_valid(&serde_json::json!({})),
            "empty args must fail: edits is required"
        );
        assert!(
            compiled.is_valid(&serde_json::json!({"edits": "[a#0000]\nDEL 1\n"})),
            "a bare edits blob must validate"
        );
        assert!(
            compiled.is_valid(
                &serde_json::json!({"edits": "[a#0000]\nDEL 1\n", "root": "/abs", "diff": true})
            ),
            "edits + root + diff must validate"
        );
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

    /// `tilth_files` was consolidated into `tilth_list`; it must no longer be
    /// advertised so clients can't discover a removed tool.
    #[test]
    fn tilth_files_is_not_advertised() {
        for edit_mode in [false, true] {
            let defs = tool_definitions(edit_mode);
            let names: Vec<&str> = defs.iter().filter_map(|t| t["name"].as_str()).collect();
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
