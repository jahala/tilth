use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::OutlineCache;
use crate::session::Session;

// Sent to the LLM via the MCP `instructions` field during initialization.
// Keeps the strategic guidance from AGENTS.md available to any host.
const SERVER_INSTRUCTIONS: &str = "\
tilth — code intelligence MCP server. Five tools: read, search, files, map, session.\n\
\n\
Workflow: Start with tilth_map to orient, then tilth_search to find symbols/text, \
then tilth_read to view files. Always pass `context` (the file you're editing) to \
tilth_search — it boosts nearby results.\n\
\n\
tilth_read: Small files → full content. Large files → structural outline. \
Start with the outline, then use `section` to drill into specific line ranges.\n\
\n\
tilth_search: Symbol search (default) finds definitions first via tree-sitter AST, \
then usages. Use `kind: \"content\"` for strings/comments. Use `expand` to see full \
source of top matches.\n\
\n\
tilth_files: Glob search with token estimates. Use to find test files, configs, etc.\n\
\n\
tilth_session: Check what you've already read/searched to avoid redundant calls.";

/// MCP server over stdio. Three tools:
/// - `tilth_read`  → smart file view
/// - `tilth_search` → symbol/content/regex search
/// - `tilth_files`  → glob with previews
pub fn run() -> io::Result<()> {
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

        let response = handle_request(&req, &cache, &session);
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
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => JsonRpcResponse {
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
                "instructions": SERVER_INSTRUCTIONS
            })),
            error: None,
        },

        "tools/list" => JsonRpcResponse {
            jsonrpc: "2.0",
            id: req.id.clone(),
            result: Some(serde_json::json!({
                "tools": tool_definitions()
            })),
            error: None,
        },

        "tools/call" => handle_tool_call(req, cache, session),

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
) -> Result<String, String> {
    match tool {
        "tilth_read" => tool_read(args, cache, session),
        "tilth_search" => tool_search(args, cache, session),
        "tilth_files" => tool_files(args, cache),
        "tilth_map" => tool_map(args, cache, session),
        "tilth_session" => tool_session(args, session),
        _ => Err(format!("unknown tool: {tool}")),
    }
}

fn tool_read(args: &Value, cache: &OutlineCache, session: &Session) -> Result<String, String> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path")?;
    let path = PathBuf::from(path_str);
    let section = args.get("section").and_then(|v| v.as_str());
    let full = args
        .get("full")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    session.record_read(&path);
    let output = crate::read::read_file(&path, section, full, cache).map_err(|e| e.to_string())?;

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
        .unwrap_or(0) as usize;
    let context_path = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let context = context_path.as_deref();
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    session.record_search(query);
    let output = match kind {
        "symbol" => crate::search::search_symbol_expanded(query, &scope, cache, expand, context),
        "content" => crate::search::search_content_expanded(query, &scope, cache, expand, context),
        "regex" => {
            let result = crate::search::content::search(query, &scope, true, context)
                .map_err(|e| e.to_string())?;
            crate::search::format_content_result(&result, cache)
        }
        _ => {
            return Err(format!(
                "unknown search kind: {kind}. Use: symbol, content, regex"
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
) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").unwrap_or(&Value::Null);

    let result = dispatch_tool(tool_name, args, cache, session);

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

fn tool_definitions() -> Vec<Value> {
    vec![
        serde_json::json!({
            "name": "tilth_read",
            "description": "Read a file with smart outlining. Small files return full content. Large files return a structural outline (functions, classes, imports). Use `section` to read specific line ranges. Use `full` to force complete content.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path to read."
                    },
                    "section": {
                        "type": "string",
                        "description": "Line range e.g. '45-89'. Bypasses smart view."
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
            "name": "tilth_search",
            "description": "Search for symbols, text, or regex patterns in code. Symbol search returns definitions first (via tree-sitter AST), then usages, with structural outline context. Content search finds literal text. Regex search supports full regex patterns.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name, text string, or regex pattern to search for."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Directory to search within. Default: current directory."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["symbol", "content", "regex"],
                        "default": "symbol",
                        "description": "Search type. symbol: structural definitions + usages. content: literal text. regex: regex pattern."
                    },
                    "expand": {
                        "type": "number",
                        "default": 0,
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
            "name": "tilth_map",
            "description": "Generate a structural codebase map. Code files show exported symbol names. Non-code files show token estimates. Replaces multi-call directory exploration with one call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "description": "Root directory to map. Default: current directory."
                    },
                    "depth": {
                        "type": "number",
                        "default": 3,
                        "description": "Maximum directory depth to traverse."
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
        serde_json::json!({
            "name": "tilth_session",
            "description": "View or reset session activity summary. Shows files read, searches performed, top symbols, and hot paths. Use action='reset' to clear.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["summary", "reset"],
                        "default": "summary",
                        "description": "summary: show activity stats. reset: clear all counters."
                    }
                }
            }
        }),
    ]
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
