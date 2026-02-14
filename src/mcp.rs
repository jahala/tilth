use std::fmt::Write as _;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::OutlineCache;
use crate::session::Session;

// Sent to the LLM via the MCP `instructions` field during initialization.
// Keeps the strategic guidance from AGENTS.md available to any host.
const SERVER_INSTRUCTIONS: &str = "\
tilth — code intelligence MCP server. Three core tools: search, read, files.\n\
\n\
IMPORTANT: Use tilth tools for ALL code navigation. Never use Bash for grep, cat, find, or ls — \
tilth_search, tilth_read, and tilth_files replace these with better results.\n\
\n\
Workflow: Start with tilth_search to find what you need. Always pass `context` (the file you're editing) — \
it boosts nearby results. With `expand` (default 2), you get code inlined, often eliminating a separate read. \
For cross-file tracing, pass multiple symbols comma-separated (e.g. query: \"ServeHTTP, HandlersChain, Next\") — \
each gets definitions from different files in one call. Expanded definitions include a `── calls ──` footer \
showing resolved callees — follow these instead of searching for each callee.\n\
\n\
tilth_search: Symbol search (default) finds definitions first via tree-sitter AST, then usages. \
Comma-separated symbols for multi-symbol lookup (max 5). Use `kind: \"content\"` for strings/comments. \
Use `kind: \"callers\"` to find all call sites of a symbol (structural matching, not text search). \
Use `expand` to see full source of top matches. Re-expanding a previously shown definition shows `[shown earlier]` \
instead of the full body.\n\
\n\
tilth_read: Small files → full content. Large files → structural outline. Non-expanded definitions show \
`path:start-end [definition]` with line range for direct section reads. Use `section` to drill into specific \
line ranges. For markdown, you can also use a heading as the section (e.g. \"## Architecture\"). \
Use `paths` to read multiple files in one call — saves round-trips.\n\
\n\
tilth_files: Find files by glob pattern. Returns paths + token estimates. Respects .gitignore.\n\
\n\
IMPORTANT: Expanded search results include full source code — do NOT re-read files already shown \
in search output. Answer from what you have rather than exploring further.";

const EDIT_MODE_INSTRUCTIONS: &str = "\
tilth — code intelligence + edit MCP server. Four tools: read, edit, search, files.\n\
\n\
IMPORTANT: Always use tilth tools instead of host built-in tools for all file operations.\n\
tilth_read output contains line:hash anchors that tilth_edit depends on.\n\
\n\
HASHLINE FORMAT: tilth_read returns lines as `line:hash|content`, e.g.:\n\
  42:a3f|  let x = compute();\n\
The anchor (`42:a3f`) is line number + 3-char content checksum.\n\
\n\
EDIT WORKFLOW:\n\
1. tilth_read → get hashlined content\n\
2. tilth_edit → pass anchors: {\"start\": \"42:a3f\", \"content\": \"new code\"}\n\
   Range: {\"start\": \"42:a3f\", \"end\": \"45:b2c\", \"content\": \"...\"}\n\
   Delete: {\"start\": \"42:a3f\", \"content\": \"\"}\n\
3. Hash mismatch → file changed, re-read and retry\n\
\n\
LARGE FILES: tilth_read returns outline (no hashlines). Use section to get hashlined content.\n\
BATCH READ: paths=[\"a\",\"b\"] reads multiple files in one call.\n\
STRATEGY: minimize tool calls. Use tilth_search with comma-separated symbols for cross-file tracing. \
expand inlines source — often avoids a separate read. Expanded definitions include a `── calls ──` footer \
showing resolved callees — follow these instead of searching for each callee. Use `kind: \"callers\"` to find \
all call sites of a symbol. Re-expanding a previously shown definition shows `[shown earlier]` instead of the full body.";

/// MCP server over stdio. When `edit_mode` is true, exposes `tilth_edit` and
/// switches `tilth_read` to hashline output format.
pub fn run(edit_mode: bool) -> io::Result<()> {
    let cache = OutlineCache::new();
    let session = Session::new();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_error(&mut stdout, None, -32700, &format!("parse error: {e}"))?;
                continue;
            }
        };

        // Notifications have no id — silently drop them per JSON-RPC spec
        if req.id.is_none() {
            continue;
        }

        let response = handle_request(&req, &cache, &session, edit_mode);
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }

    Ok(())
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

fn handle_request(
    req: &JsonRpcRequest,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => {
            let instructions = if edit_mode {
                EDIT_MODE_INSTRUCTIONS
            } else {
                SERVER_INSTRUCTIONS
            };
            JsonRpcResponse {
                jsonrpc: "2.0",
                id: req.id.clone(),
                result: Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "tilth",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": instructions
                })),
                error: None,
            }
        }

        "tools/list" => JsonRpcResponse {
            jsonrpc: "2.0",
            id: req.id.clone(),
            result: Some(serde_json::json!({
                "tools": tool_definitions(edit_mode)
            })),
            error: None,
        },

        "tools/call" => handle_tool_call(req, cache, session, edit_mode),

        "ping" => JsonRpcResponse {
            jsonrpc: "2.0",
            id: req.id.clone(),
            result: Some(serde_json::json!({})),
            error: None,
        },

        _ => JsonRpcResponse {
            jsonrpc: "2.0",
            id: req.id.clone(),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("method not found: {}", req.method),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

/// Execute a tool by name with the given arguments. Returns formatted output or error string.
/// No classifier involved — the caller specifies the tool explicitly.
pub(crate) fn dispatch_tool(
    tool: &str,
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> Result<String, String> {
    match tool {
        "tilth_read" => tool_read(args, cache, session, edit_mode),
        "tilth_search" => tool_search(args, cache, session),
        "tilth_files" => tool_files(args, cache),
        "tilth_map" => Err("tilth_map is disabled — use tilth_search instead".into()),
        "tilth_session" => tool_session(args, session),
        "tilth_edit" if edit_mode => tool_edit(args, session),
        _ => Err(format!("unknown tool: {tool}")),
    }
}

fn tool_read(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> Result<String, String> {
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    // Multi-file batch read (capped at 20 to bound I/O)
    if let Some(paths_arr) = args.get("paths").and_then(|v| v.as_array()) {
        if paths_arr.len() > 20 {
            return Err(format!(
                "batch read limited to 20 files (got {})",
                paths_arr.len()
            ));
        }
        let mut results = Vec::with_capacity(paths_arr.len());
        for p in paths_arr {
            let path_str = p.as_str().ok_or("paths must be an array of strings")?;
            let path = PathBuf::from(path_str);
            session.record_read(&path);
            match crate::read::read_file(&path, None, false, cache, edit_mode) {
                Ok(output) => results.push(output),
                Err(e) => results.push(format!("# {} — error: {}", path.display(), e)),
            }
        }
        let combined = results.join("\n\n");
        return Ok(apply_budget(combined, budget));
    }

    // Single file read
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path (or use paths for batch read)")?;
    let path = PathBuf::from(path_str);
    let section = args.get("section").and_then(|v| v.as_str());
    let full = args
        .get("full")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    session.record_read(&path);
    let mut output = crate::read::read_file(&path, section, full, cache, edit_mode)
        .map_err(|e| e.to_string())?;

    // Append related-file hint for outlined code files (not section reads, not batch).
    if section.is_none() && crate::read::would_outline(&path) {
        let related = crate::read::imports::resolve_related_files(&path);
        if !related.is_empty() {
            output.push_str("\n\n> Related: ");
            for (i, p) in related.iter().enumerate() {
                if i > 0 {
                    output.push_str(", ");
                }
                let _ = write!(output, "{}", p.display());
            }
        }
    }

    Ok(apply_budget(output, budget))
}

fn tool_search(args: &Value, cache: &OutlineCache, session: &Session) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: query")?;
    let scope = resolve_scope(args);
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("symbol");
    let expand = args
        .get("expand")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2) as usize;
    let context_path = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let context = context_path.as_deref();
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let output = match kind {
        "symbol" => {
            let queries: Vec<&str> = query
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            match queries.len() {
                0 => return Err("missing required parameter: query".into()),
                1 => {
                    session.record_search(queries[0]);
                    crate::search::search_symbol_expanded(
                        queries[0], &scope, cache, session, expand, context,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, expand, context,
                    )
                }
                _ => {
                    return Err(format!(
                        "multi-symbol search limited to 5 queries (got {})",
                        queries.len()
                    ))
                }
            }
        }
        "content" => {
            session.record_search(query);
            crate::search::search_content_expanded(query, &scope, cache, session, expand, context)
        }
        "regex" => {
            session.record_search(query);
            let result = crate::search::content::search(query, &scope, true, context)
                .map_err(|e| e.to_string())?;
            crate::search::format_content_result(&result, cache)
        }
        "callers" => {
            session.record_search(query);
            crate::search::callers::search_callers_expanded(
                query, &scope, cache, session, expand, context,
            )
        }
        _ => {
            return Err(format!(
                "unknown search kind: {kind}. Use: symbol, content, regex, callers"
            ))
        }
    }
    .map_err(|e| e.to_string())?;

    Ok(apply_budget(output, budget))
}

fn tool_files(args: &Value, cache: &OutlineCache) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: pattern")?;
    let scope = resolve_scope(args);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let output = crate::search::search_glob(pattern, &scope, cache).map_err(|e| e.to_string())?;

    Ok(apply_budget(output, budget))
}

#[allow(dead_code)] // Map disabled in v0.3.2 — kept for potential re-enable
fn tool_map(args: &Value, cache: &OutlineCache, session: &Session) -> Result<String, String> {
    let scope = resolve_scope(args);
    let depth = args
        .get("depth")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(3) as usize;
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    session.record_map();
    Ok(crate::map::generate(&scope, depth, budget, cache))
}

fn tool_session(args: &Value, session: &Session) -> Result<String, String> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("summary");
    match action {
        "reset" => {
            session.reset();
            Ok("Session reset.".to_string())
        }
        _ => Ok(session.summary()),
    }
}

fn tool_edit(args: &Value, session: &Session) -> Result<String, String> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path")?;
    let path = PathBuf::from(path_str);

    let edits_val = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or("missing required parameter: edits")?;

    let mut edits = Vec::with_capacity(edits_val.len());
    for (i, e) in edits_val.iter().enumerate() {
        let start_str = e
            .get("start")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edit[{i}]: missing 'start'"))?;
        let (start_line, start_hash) = crate::format::parse_anchor(start_str)
            .ok_or_else(|| format!("edit[{i}]: invalid start anchor '{start_str}'"))?;

        let (end_line, end_hash) = if let Some(end_str) = e.get("end").and_then(|v| v.as_str()) {
            crate::format::parse_anchor(end_str)
                .ok_or_else(|| format!("edit[{i}]: invalid end anchor '{end_str}'"))?
        } else {
            (start_line, start_hash)
        };

        let content = e
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edit[{i}]: missing 'content'"))?;

        edits.push(crate::edit::Edit {
            start_line,
            start_hash,
            end_line,
            end_hash,
            content: content.to_string(),
        });
    }

    session.record_read(&path);

    match crate::edit::apply_edits(&path, &edits).map_err(|e| e.to_string())? {
        crate::edit::EditResult::Applied(output) => Ok(output),
        crate::edit::EditResult::HashMismatch(msg) => Err(format!(
            "hash mismatch — file changed since last read:\n\n{msg}"
        )),
    }
}

/// Canonicalize scope path, falling back to the raw path if canonicalization fails.
fn resolve_scope(args: &Value) -> PathBuf {
    let raw: PathBuf = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .into();
    raw.canonicalize().unwrap_or(raw)
}

fn apply_budget(output: String, budget: Option<u64>) -> String {
    match budget {
        Some(b) => crate::budget::apply(&output, b),
        None => output,
    }
}

// ---------------------------------------------------------------------------
// MCP tool call handler
// ---------------------------------------------------------------------------

fn handle_tool_call(
    req: &JsonRpcRequest,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").unwrap_or(&Value::Null);

    let result = dispatch_tool(tool_name, args, cache, session, edit_mode);

    match result {
        Ok(output) => JsonRpcResponse {
            jsonrpc: "2.0",
            id: req.id.clone(),
            result: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": output
                }]
            })),
            error: None,
        },
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0",
            id: req.id.clone(),
            result: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": e
                }],
                "isError": true
            })),
            error: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

fn tool_definitions(edit_mode: bool) -> Vec<Value> {
    let read_desc = if edit_mode {
        "Read a file with smart outlining. Output uses hashline format (line:hash|content) — \
         the line:hash anchors are required by tilth_edit. Small files return full hashlined content. \
         Large files return a structural outline (no hashlines); use `section` to get hashlined \
         content for the lines you want to edit. Use `full` to force complete content. \
         Use `paths` to read multiple files in one call."
    } else {
        "Read a file with smart outlining. Small files return full content. Large files return \
         a structural outline (functions, classes, imports). Use `section` to read specific \
         line ranges. Use `full` to force complete content. \
         Use `paths` to read multiple files in one call."
    };
    let mut tools = vec![
        serde_json::json!({
            "name": "tilth_search",
            "description": "Search for symbols, text, or regex patterns in code. Symbol search returns definitions first (via tree-sitter AST), then usages, with structural outline context. Content search finds literal text. Regex search supports full regex patterns. For cross-file tracing, pass comma-separated symbol names (max 5).",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name, text string, or regex pattern to search for. For symbol search, comma-separated names for multi-symbol lookup."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Directory to search within. Default: current directory."
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
                        "description": "Absolute or relative file path to read."
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multiple file paths to read in one call. Each file gets independent smart handling. Saves round-trips vs multiple single reads."
                    },
                    "section": {
                        "type": "string",
                        "description": "Line range e.g. '45-89', or heading e.g. '## Architecture'. Bypasses smart view."
                    },
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Force full content output, bypass smart outlining."
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
            "description": "Find files matching a glob pattern. Returns matched file paths with token estimates. Respects .gitignore.",
            "inputSchema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern e.g. '*.rs', 'src/**/*.ts', '*.test.*'"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Directory to search within. Default: current directory."
                    },
                    "budget": {
                        "type": "number",
                        "description": "Max tokens in response."
                    }
                }
            }
        }),
        // tilth_map disabled — benchmark data shows 62% of losing tasks use map
        // vs 22% of winners. Re-enable after measuring impact.
        // serde_json::json!({
        //     "name": "tilth_map",
        //     ...
        // }),
    ];

    if edit_mode {
        tools.push(serde_json::json!({
            "name": "tilth_edit",
            "description": "Apply edits to a file using hashline anchors from tilth_read. Each edit targets a line range by line:hash anchors. Edits are verified against content hashes and rejected if the file has changed since the last read.",
            "inputSchema": {
                "type": "object",
                "required": ["path", "edits"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path to edit."
                    },
                    "edits": {
                        "type": "array",
                        "description": "Array of edit operations, applied atomically.",
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
        }));
    }

    tools
}

fn write_error(w: &mut impl Write, id: Option<Value>, code: i32, msg: &str) -> io::Result<()> {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: msg.into(),
        }),
    };
    serde_json::to_writer(&mut *w, &resp)?;
    w.write_all(b"\n")?;
    w.flush()
}
