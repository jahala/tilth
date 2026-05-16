//! JSON-schema definitions for the tools advertised via `tools/list`.
//! Edit-mode adds `tilth_write` to the catalog.
//!
//! Tool descriptions live in `prompts/tools/*.md` and are compiled in via
//! `include_str!`. Trailing whitespace is stripped at load time so source files
//! can keep their final newline without affecting wire bytes.

use serde_json::Value;

const SEARCH_DESC: &str = include_str!("../../../prompts/tools/search.md");
const READ_DESC: &str = include_str!("../../../prompts/tools/read.md");
const LIST_DESC: &str = include_str!("../../../prompts/tools/list.md");
const DEPS_DESC: &str = include_str!("../../../prompts/tools/deps.md");
const DIFF_DESC: &str = include_str!("../../../prompts/tools/diff.md");
const WRITE_DESC: &str = include_str!("../../../prompts/tools/write.md");

pub(crate) fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let mut tools = vec![
        serde_json::json!({
            "name": "tilth_search",
            "description": SEARCH_DESC.trim_end(),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["query"],
                            "properties": {
                                "query": {"type": "string"},
                                "glob": {"type": "string"},
                                "kind": {"type": "string", "enum": ["symbol", "content", "regex", "callers"]}
                            }
                        },
                        "minItems": 1,
                        "maxItems": 10,
                        "description": "Array of query objects. Each entry runs independently and results are concatenated under `## query: <q>` headers."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Only use scope to search a specific subdirectory. DO NOT USE scope if you want to search the current working directory (initial search)."
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
                        "description": "ISO-8601 timestamp. Files with mtime ≤ this value return as `(unchanged @ <ts>)` stubs instead of bodies."
                    }
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_read",
            "description": READ_DESC.trim_end(),
            "inputSchema": {
                "type": "object",
                "required": ["paths"],
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "File paths (max 20). Suffix grammar on each path: `path#n-m` (line range), `path#n` (from line n), `path### Heading` (markdown heading), `path#symbol_name` (code symbol). Example: paths: [\"src/foo.rs#do_thing\", \"README.md#10-40\"]. Singular `path: \"...\"` is also accepted for one-file reads."
                    },
                    "path": {
                        "type": "string",
                        "description": "Singular form (transitional). Prefer paths: [...]."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "full", "signature"],
                        "default": "auto",
                        "description": "auto = smart-size: small files return full; large code returns signature lines with `<line>:<hash>` prefixes; large markdown returns headings + preview. full forces full content. signature forces outline."
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
            "name": "tilth_list",
            "description": LIST_DESC.trim_end(),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Glob patterns (max 20). e.g. [\"*.rs\", \"src/**/*.ts\"]. Always an array."
                    },
                    "depth": {
                        "type": "number",
                        "description": "Cap directory depth (1 = top-level only)."
                    },
                    "scope": {"type": "string"},
                    "budget": {"type": "number"}
                }
            }
        }),
        serde_json::json!({
            "name": "tilth_deps",
            "description": DEPS_DESC.trim_end(),
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
            "name": "tilth_diff",
            "description": DIFF_DESC.trim_end(),
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
            "description": WRITE_DESC.trim_end(),
            "inputSchema": {
                "type": "object",
                "required": ["files"],
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "required": ["path"],
                            "properties": {
                                "path": {"type": "string"},
                                "mode": {
                                    "type": "string",
                                    "enum": ["hash", "overwrite", "append"],
                                    "default": "hash"
                                },
                                "edits": {
                                    "type": "array",
                                    "description": "Required when mode=hash. Each: {start: '<line>:<hash>', end?: '<line>:<hash>', content: '...'}",
                                    "items": {
                                        "type": "object",
                                        "required": ["start", "content"],
                                        "properties": {
                                            "start": {"type": "string"},
                                            "end": {"type": "string"},
                                            "content": {"type": "string"}
                                        }
                                    }
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Required when mode=overwrite or mode=append. Full file content / append payload."
                                }
                            }
                        }
                    },
                    "diff": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include per-file before/after diff in the response."
                    }
                }
            }
        }));
    }

    tools
}
