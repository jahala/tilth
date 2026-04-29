use std::fmt::Write as _;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::OutlineCache;
use crate::index::bloom::BloomFilterCache;
use crate::index::SymbolIndex;
use crate::session::Session;

/// Shared dependencies passed through the request → dispatch pipeline.
#[derive(Clone)]
pub(crate) struct Services {
    cache: Arc<OutlineCache>,
    session: Arc<Session>,
    index: Arc<SymbolIndex>,
    bloom: Arc<BloomFilterCache>,
    edit_mode: bool,
}

impl Services {
    pub(crate) fn new(edit_mode: bool) -> Self {
        Self {
            cache: Arc::new(OutlineCache::new()),
            session: Arc::new(Session::new()),
            index: Arc::new(SymbolIndex::new()),
            bloom: Arc::new(BloomFilterCache::new()),
            edit_mode,
        }
    }

    pub(crate) fn cache(&self) -> &OutlineCache {
        &self.cache
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    pub(crate) fn index(&self) -> &Arc<SymbolIndex> {
        &self.index
    }

    pub(crate) fn bloom(&self) -> &Arc<BloomFilterCache> {
        &self.bloom
    }

    pub(crate) fn edit_mode(&self) -> bool {
        self.edit_mode
    }
}

/// Tracks abandoned threads (timed out but still running). Warns on stderr
/// when accumulation exceeds threshold to help diagnose resource pressure.
static ABANDONED_THREADS: AtomicUsize = AtomicUsize::new(0);
const ABANDONED_THREAD_WARN: usize = 3;

/// Per-request timeout for tool calls. If a tool doesn't respond within this
/// duration, the MCP server returns a timeout error instead of hanging.
/// Override with `TILTH_TIMEOUT` env var (seconds). Default: 90s.
fn request_timeout() -> Duration {
    let secs = std::env::var("TILTH_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90);
    Duration::from_secs(secs)
}

// Sent to the LLM via the MCP `instructions` field during initialization.
// Keeps the strategic guidance from AGENTS.md available to any host.
const SERVER_INSTRUCTIONS: &str = "\
tilth — code intelligence MCP server. Replaces grep, cat, find, ls with AST-aware equivalents.\n\
\n\
To explore code, always search first. tilth_search finds definitions, usages, and file locations in one call.\n\
Usage: tilth_search(query: \"handleRequest\").\n\
tilth_files is ONLY for listing directory contents when you have no symbol or text to search for.\n\
DO NOT use Read if content is already shown in expanded search results.\n\
DO NOT use Grep, Read, or Glob. Always use the better tools tilth_search (grep), tilth_read (read), tilth_files (glob).\n\
\n\
tilth_search: Search code — finds definitions, usages, and text. Replaces grep/rg for all code search.\n\
  For multi-symbol lookup, separate each with a comma \"symbol1,symbol2\" (max 5).\n\
  kind: \"symbol\" (default) | \"content\" (strings/comments) | \"callers\" (call sites)\n\
  expand (default 2): inline full source for top matches.\n\
  context: path to file being edited — boosts nearby results.\n\
  glob: file pattern filter — \"*.rs\" (whitelist), \"!*.test.ts\" (exclude).\n\
  Output per match:\n\
    ## <path>:<start>-<end> [definition|usage|impl]\n\
    <outline context>\n\
    <expanded source block>\n\
    ── calls ──\n\
      <name>  <path>:<start>-<end>  <signature>\n\
    ── siblings ──\n\
      <name>  <path>:<start>-<end>  <signature>\n\
  Re-expanding a previously shown definition returns [shown earlier].\n\
\n\
tilth_read: Read file content with smart outlining. Replaces cat/head/tail.\n\
  Small files → full content. Large files → structural outline.\n\
  section: \"<start>-<end>\" or \"<heading text>\"\n\
  paths: read multiple files in one call.\n\
  Output:\n\
    <line_number> │ <content>                  ← full/section mode\n\
    [<start>-<end>]  <symbol name>             ← outline mode\n\
\n\
tilth_files: Find files by glob pattern. Replaces find, ls, pwd, and the host Glob tool.\n\
  Output: <path>  (~<token_count> tokens). Respects .gitignore.\n\
\n\
tilth_deps: Blast-radius check — what imports this file and what it imports.\n\
  Use ONLY before renaming, removing, or changing an export's signature.\n\
\n\
tilth_diff: Structural diff — shows what changed at function level. Replaces Bash(git diff).\n\
  Usage: tilth_diff(source: \"HEAD~1\") for last commit. No args = uncommitted changes.\n\
  scope: \"file.rs\" or \"file.rs:fn_name\". log: \"HEAD~5..HEAD\" for per-commit summaries.\n\
  search: filter to lines matching a term. blast: true to show callers of changed signatures.\n\
  Output: [+] added, [-] deleted, [~] body changed, [~:sig] signature changed.\n\
  DO NOT use Bash(git diff) or Bash(git log --patch). Use tilth_diff instead.\n\
\n\
To search code, use tilth_search instead of Grep or Bash(grep/rg).\n\
To read files, use tilth_read instead of Read or Bash(cat).\n\
To find files, use tilth_files instead of Glob or Bash(find/ls).\n\
To check what changed, use tilth_diff instead of Bash(git diff/git log).\n\
DO NOT re-read files already shown in expanded search results.";

const EDIT_MODE_EXTRA: &str = "\n\
\n\
tilth_edit: Edit files using hash-anchored lines. Replaces the host Edit tool.\n\
  tilth_read → copy anchors (<line>:<hash>) (BOTH line and hash required) → pass to tilth_edit.\n\
  tilth_search does NOT provide hashes — you MUST tilth_read the file or section first.\n\
  Single line: {\"start\": \"<line>:<hash>\", \"content\": \"<new code>\"}\n\
  Range: {\"start\": \"<line>:<hash>\", \"end\": \"<line>:<hash>\", \"content\": \"...\"}\n\
  Delete: {\"start\": \"<line>:<hash>\", \"content\": \"\"}\n\
  Hash mismatch → file changed, re-read and retry.\n\
  Large files: tilth_read shows outline — use section to get hashlined content.\n\
  Pass diff: true to see a compact before/after diff in the response.\n\
  After editing a function signature, tilth_edit shows callers that may need updating.\n\
DO NOT use the host Edit tool. Use tilth_edit for all edits.";

/// MCP server over stdio. When `edit_mode` is true, exposes `tilth_edit` and
/// switches `tilth_read` to hashline output format.
///
/// `scope` overrides the default search root. When provided, tilth chdir's to it
/// at startup so all tools, git commands, and searches use the correct project root.
/// This fixes MCP hosts that launch tilth with cwd=/ (e.g., Codex).
pub fn run(edit_mode: bool, scope: Option<&Path>) -> io::Result<()> {
    let scope_is_explicit = scope.is_some();

    // Resolve the project root and chdir to it.
    // Priority: explicit --scope > MCP roots (handled later) > package_root(cwd) > cwd
    if let Some(s) = scope {
        if s.is_dir() {
            let _ = std::env::set_current_dir(s);
        }
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(root) = crate::lang::package_root(&cwd) {
            let _ = std::env::set_current_dir(root);
        }
    }

    let services = Services::new(edit_mode);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    // Track pending roots/list request (for MCP roots protocol)
    let mut pending_roots_id: Option<Value> = None;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }

        // Parse as generic JSON first — could be a request, notification, or response
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_error(&mut stdout, None, -32700, &format!("parse error: {e}"))?;
                continue;
            }
        };

        // Check if this is a response to our roots/list request
        if let Some(ref roots_id) = pending_roots_id {
            if msg.get("id") == Some(roots_id) {
                pending_roots_id = None;
                // Only apply roots on success and if --scope was NOT explicitly provided
                if !scope_is_explicit {
                    if let Some(root_path) = extract_root_from_response(&msg) {
                        let _ = std::env::set_current_dir(&root_path);
                    }
                }
                continue;
            }
        }

        // Must have "method" to be a request or notification
        let method = match msg.get("method").and_then(Value::as_str) {
            Some(m) => m.to_string(),
            None => continue, // Not a request — skip (could be an unexpected response)
        };

        let id = msg.get("id").cloned();
        if id.is_none() {
            // Notifications have no id — silently drop them per JSON-RPC spec
            continue;
        }

        // Parse params
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let req = JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id,
            method: method.clone(),
            params,
        };

        let response = handle_request(&req, &services);
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;

        // After initialize response: send roots/list if client supports it
        // and we don't already have an explicit --scope
        if method == "initialize" && !scope_is_explicit && pending_roots_id.is_none() {
            let client_caps = req.params.get("capabilities").unwrap_or(&Value::Null);
            if client_caps.get("roots").is_some() {
                let roots_id = Value::String("tilth_roots_1".to_string());
                let roots_req = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": roots_id,
                    "method": "roots/list"
                });
                serde_json::to_writer(&mut stdout, &roots_req)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
                pending_roots_id = Some(roots_id);
            }
        }
    }

    Ok(())
}

/// Extract the first root directory path from a roots/list response.
/// Parses `file://` URIs and returns the path, or None if no valid roots.
fn extract_root_from_response(msg: &Value) -> Option<PathBuf> {
    let roots = msg.get("result")?.get("roots")?.as_array()?;
    for root in roots {
        let uri = root.get("uri")?.as_str()?;
        let raw_path = uri.strip_prefix("file://").unwrap_or(uri);
        let path = PathBuf::from(percent_decode(raw_path));
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

/// Decode percent-encoded URI path components (e.g. `%20` → space).
fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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

fn handle_request(req: &JsonRpcRequest, services: &Services) -> JsonRpcResponse {
    let edit_mode = services.edit_mode();
    match req.method.as_str() {
        "initialize" => {
            let overview = if std::env::var("TILTH_NO_OVERVIEW").is_ok() {
                String::new()
            } else {
                let cwd = std::env::current_dir().unwrap_or_default();
                crate::overview::fingerprint(&cwd)
            };
            let instructions = if edit_mode {
                if overview.is_empty() {
                    format!("{SERVER_INSTRUCTIONS}{EDIT_MODE_EXTRA}")
                } else {
                    format!("{overview}\n\n{SERVER_INSTRUCTIONS}{EDIT_MODE_EXTRA}")
                }
            } else if overview.is_empty() {
                SERVER_INSTRUCTIONS.to_string()
            } else {
                format!("{overview}\n\n{SERVER_INSTRUCTIONS}")
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

        "tools/call" => handle_tool_call(req, services),

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
    services: &Services,
) -> Result<String, String> {
    let edit_mode = services.edit_mode();
    match tool {
        "tilth_read" => tool_read(args, services.cache(), services.session(), edit_mode),
        "tilth_search" => tool_search(
            args,
            services.cache(),
            services.session(),
            services.index(),
            services.bloom(),
        ),
        "tilth_files" => tool_files(args, services.cache()),
        "tilth_deps" => tool_deps(args, services.cache(), services.bloom()),
        "tilth_diff" => tool_diff(args),
        "tilth_map" => Err("tilth_map is disabled — use tilth_search instead".into()),
        "tilth_session" => tool_session(args, services.session()),
        "tilth_edit" if edit_mode => {
            tool_edit(args, services.session(), services.cache(), services.bloom())
        }
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

        // Aggregate deadline for batch reads: 60s default, override with TILTH_BATCH_TIMEOUT
        // Note: deadline is checked between files, so a single massive file could still
        // exceed it. The per-request timeout (handle_tool_call) catches that case.
        let batch_timeout = std::env::var("TILTH_BATCH_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(batch_timeout);

        let mut results = Vec::with_capacity(paths_arr.len());
        for (i, p) in paths_arr.iter().enumerate() {
            // Check deadline before each file
            if std::time::Instant::now() > deadline {
                results.push(format!(
                    "# batch read stopped — deadline exceeded after {}/{} files. \
                     Reduce batch size or set TILTH_BATCH_TIMEOUT=<seconds>.",
                    i,
                    paths_arr.len()
                ));
                break;
            }

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

fn tool_search(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    index: &Arc<SymbolIndex>,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: query")?;
    let (scope, scope_warning) = resolve_scope(args);
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
    let glob = args.get("glob").and_then(|v| v.as_str());
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
                        queries[0], &scope, cache, session, index, bloom, expand, context, glob,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, index, bloom, expand, context, glob,
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
            crate::search::search_content_expanded(
                query, &scope, cache, session, expand, context, glob,
            )
        }
        "regex" => {
            session.record_search(query);
            let result = crate::search::content::search(query, &scope, true, context, glob)
                .map_err(|e| e.to_string())?;
            crate::search::format_raw_result(&result, cache)
        }
        "callers" => {
            session.record_search(query);
            crate::search::callers::search_callers_expanded(
                query, &scope, cache, session, bloom, expand, context, glob,
            )
        }
        _ => {
            return Err(format!(
                "unknown search kind: {kind}. Use: symbol, content, regex, callers"
            ))
        }
    }
    .map_err(|e| e.to_string())?;

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(output, budget));
    Ok(result)
}

fn tool_files(args: &Value, cache: &OutlineCache) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: pattern")?;
    let (scope, scope_warning) = resolve_scope(args);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let output = crate::search::search_glob(pattern, &scope, cache).map_err(|e| e.to_string())?;

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(output, budget));
    Ok(result)
}

fn tool_deps(
    args: &Value,
    cache: &OutlineCache,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path")?;
    let path = PathBuf::from(path_str);
    let (scope, scope_warning) = resolve_scope(args);
    let budget = args
        .get("budget")
        .and_then(serde_json::Value::as_u64)
        .map(|b| b as usize);

    let deps_result = crate::search::deps::analyze_deps(&path, &scope, cache, bloom)
        .map_err(|e| e.to_string())?;
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&crate::search::deps::format_deps(
        &deps_result,
        &scope,
        budget,
    ));
    Ok(output)
}

fn tool_diff(args: &Value) -> Result<String, String> {
    let source = args.get("source").and_then(|v| v.as_str());
    let scope = args.get("scope").and_then(|v| v.as_str());
    let a = args.get("a").and_then(|v| v.as_str());
    let b = args.get("b").and_then(|v| v.as_str());
    let patch = args.get("patch").and_then(|v| v.as_str());
    let log = args.get("log").and_then(|v| v.as_str());
    let search = args.get("search").and_then(|v| v.as_str());
    let blast = args.get("blast").and_then(Value::as_bool).unwrap_or(false);
    let expand = args.get("expand").and_then(Value::as_u64).unwrap_or(0) as usize;
    let budget = args.get("budget").and_then(Value::as_u64);

    let diff_source = crate::diff::resolve_source(source, a, b, patch, log)?;
    crate::diff::diff(&diff_source, scope, search, blast, expand, budget)
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

fn tool_edit(
    args: &Value,
    session: &Session,
    _cache: &OutlineCache,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
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

    let show_diff = args
        .get("diff")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    session.record_read(&path);

    match crate::edit::apply_edits(&path, &edits).map_err(|e| e.to_string())? {
        crate::edit::EditResult::Applied { diff, context } => {
            let mut output = String::new();

            if show_diff && !diff.is_empty() {
                output.push_str(&diff);
                if !context.is_empty() {
                    output.push_str("\n\n");
                }
            }

            if !context.is_empty() {
                output.push_str(&context);
            }

            let abs_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let scope = crate::lang::package_root(&abs_path).map_or_else(
                || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                std::path::Path::to_path_buf,
            );

            if let Some(blast) = crate::search::blast::blast_radius(&path, &edits, &scope, bloom) {
                output.push_str(&blast);
            }

            Ok(output)
        }
        crate::edit::EditResult::HashMismatch(msg) => Err(format!(
            "hash mismatch — file changed since last read:\n\n{msg}"
        )),
    }
}

/// Falls back to cwd when scope is invalid, with a warning message.
fn resolve_scope(args: &Value) -> (PathBuf, Option<String>) {
    let raw_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or(".");
    let raw: PathBuf = raw_str.into();
    let resolved = raw.canonicalize().unwrap_or_else(|_| raw.clone());
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    if resolved == cwd {
        return (".".into(), None);
    }
    if !resolved.is_dir() {
        return (
            ".".into(),
            Some(format!(
                "scope \"{raw_str}\" is not a valid directory, searching current directory instead.\n\n"
            )),
        );
    }
    (resolved, None)
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

fn handle_tool_call(req: &JsonRpcRequest, services: &Services) -> JsonRpcResponse {
    let params = &req.params;
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").unwrap_or(&Value::Null);

    // Clone values needed by the worker thread
    let tool_name_owned = tool_name.to_string();
    let args_owned = args.clone();
    let services_clone = services.clone();

    let (tx, rx) = mpsc::channel();
    let timeout = request_timeout();

    let handle = std::thread::spawn(move || {
        let result = dispatch_tool(&tool_name_owned, &args_owned, &services_clone);
        let _ = tx.send(result);
    });

    let result = match rx.recv_timeout(timeout) {
        Ok(result) => {
            // Thread finished in time — join it to reclaim resources
            let _ = handle.join();
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Thread is still running — we can't safely kill it, but we return
            // an error to the client immediately. The thread will finish in the
            // background and its result will be dropped.
            //
            // Note: we intentionally do NOT join here to avoid blocking the
            // main loop. The thread will complete and exit on its own.
            let abandoned = ABANDONED_THREADS.fetch_add(1, Ordering::Relaxed) + 1;
            if abandoned >= ABANDONED_THREAD_WARN {
                eprintln!(
                    "tilth: warning: {abandoned} abandoned threads still running. \
                     Consider reducing scope or increasing TILTH_TIMEOUT."
                );
            }
            Err(format!(
                "tool timed out after {}s — the operation took too long. \
                 Try: reduce scope, use section instead of full, or set \
                 TILTH_TIMEOUT=<seconds> to increase the limit.",
                timeout.as_secs()
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // Thread panicked — the channel was dropped without sending
            Err("tool panicked during execution".into())
        }
    };

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
        "Read a file with smart outlining. Replaces cat/head/tail and the host Read tool — \
         use this for all file reading. Output uses hashline format (line:hash|content) — \
         the line:hash anchors are required by tilth_edit. Small files return full hashlined content. \
         Large files return a structural outline (no hashlines); use `section` to get hashlined \
         content for the lines you want to edit. Use `full` to force complete content. \
         Use `paths` to read multiple files in one call."
    } else {
        "Read a file with smart outlining. Replaces cat/head/tail and the host Read tool — \
         use this for all file reading. Small files return full content. Large files return \
         a structural outline (functions, classes, imports) so you see the shape without \
         consuming your context window. Use `section` to read specific line ranges. \
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
            "description": "Find files matching a glob pattern. Replaces find/ls/pwd and the host Glob tool — use this for all file discovery. Returns matched file paths sorted by relevance with token size estimates. Respects .gitignore.",
            "inputSchema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern e.g. '*' (list directory), '*.rs', 'src/**/*.ts'"
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
        // tilth_map disabled — benchmark data shows 62% of losing tasks use map
        // vs 22% of winners. Re-enable after measuring impact.
        // serde_json::json!({
        //     "name": "tilth_map",
        //     ...
        // }),
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
                    },
                    "diff": {
                        "type": "boolean",
                        "default": false,
                        "description": "Set true to include a compact diff of changes in the response. Useful for batch edits or verifying unexpected results."
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_root_from_response -------------------------------------------

    #[test]
    fn extract_root_valid_file_uri() {
        // Claude Code sends: {"result":{"roots":[{"uri":"file:///Users/x/project"}]}}
        let tmp = tempfile::tempdir().unwrap();
        let uri = format!("file://{}", tmp.path().display());
        let msg = serde_json::json!({
            "result": { "roots": [{ "uri": uri }] }
        });
        let path = extract_root_from_response(&msg);
        assert_eq!(path, Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn extract_root_percent_encoded_uri() {
        let tmp = tempfile::tempdir().unwrap();
        let space_dir = tmp.path().join("my project");
        std::fs::create_dir(&space_dir).unwrap();
        let encoded =
            format!("file://{}", tmp.path().display()).replace(' ', "%20") + "/my%20project";
        let msg = serde_json::json!({
            "result": { "roots": [{ "uri": encoded }] }
        });
        let path = extract_root_from_response(&msg);
        assert_eq!(path, Some(space_dir));
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(
            percent_decode("/Users/Jan%20Hallvard/project"),
            "/Users/Jan Hallvard/project"
        );
        assert_eq!(percent_decode("/normal/path"), "/normal/path");
        assert_eq!(percent_decode("%2F%2Fweird"), "//weird");
        assert_eq!(percent_decode("no%percent"), "no%percent"); // incomplete sequence preserved
    }

    #[test]
    fn extract_root_empty_roots() {
        // Codex sends: {"result":{"roots":[]}}
        let msg = serde_json::json!({
            "result": { "roots": [] }
        });
        assert_eq!(extract_root_from_response(&msg), None);
    }

    #[test]
    fn extract_root_nonexistent_path() {
        let msg = serde_json::json!({
            "result": { "roots": [{ "uri": "file:///nonexistent/path/that/does/not/exist" }] }
        });
        assert_eq!(extract_root_from_response(&msg), None);
    }

    #[test]
    fn extract_root_no_result() {
        let msg = serde_json::json!({"error": {"code": -1, "message": "nope"}});
        assert_eq!(extract_root_from_response(&msg), None);
    }

    #[test]
    fn extract_root_multiple_roots_takes_first_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let uri = format!("file://{}", tmp.path().display());
        let msg = serde_json::json!({
            "result": { "roots": [
                { "uri": "file:///nonexistent" },
                { "uri": uri },
            ]}
        });
        // First root is invalid, second is valid — should return second
        let path = extract_root_from_response(&msg);
        assert_eq!(path, Some(tmp.path().to_path_buf()));
    }

    // -- resolve_scope --------------------------------------------------------

    #[test]
    fn resolve_scope_explicit_arg() {
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "scope": tmp.path().to_str().unwrap() });
        let (scope, warning) = resolve_scope(&args);
        assert_eq!(scope, tmp.path().canonicalize().unwrap());
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_no_arg_uses_cwd() {
        let args = serde_json::json!({});
        let (scope, warning) = resolve_scope(&args);
        // With no arg, defaults to "." which is cwd
        let cwd = std::env::current_dir().unwrap();
        // The function returns "." when resolved == cwd
        assert!(scope == PathBuf::from(".") || scope == cwd);
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_invalid_dir_warns() {
        let args = serde_json::json!({ "scope": "/nonexistent/directory/zzz" });
        let (scope, warning) = resolve_scope(&args);
        assert_eq!(scope, PathBuf::from("."));
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("not a valid directory"));
    }

    // -- Issue #37 reproduction: cwd=/ with --scope fixes it ------------------

    #[test]
    fn scope_flag_overrides_bad_cwd() {
        // Reproduce issue #37: MCP host launches tilth with cwd=/
        // The --scope flag should override this.
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path();

        // Create a manifest so package_root can find it
        std::fs::write(
            project_path.join("Cargo.toml"),
            "[package]\nname = \"test\"",
        )
        .unwrap();
        std::fs::create_dir(project_path.join("src")).unwrap();
        std::fs::write(project_path.join("src/main.rs"), "fn main() {}").unwrap();

        // Save current cwd
        let orig_cwd = std::env::current_dir().unwrap();

        // Simulate Codex: cwd=/
        std::env::set_current_dir("/").unwrap();

        // Without --scope: resolve_scope returns "." which is /
        let args = serde_json::json!({});
        let (scope, _) = resolve_scope(&args);
        assert_eq!(
            scope,
            PathBuf::from("."),
            "Without --scope, should return . (which is /)"
        );

        // With --scope pointing to project: set_current_dir should fix everything
        let _ = std::env::set_current_dir(project_path);
        let args = serde_json::json!({});
        let (scope, _) = resolve_scope(&args);
        assert_eq!(
            scope,
            PathBuf::from("."),
            "After chdir to project, . should resolve correctly"
        );

        // Verify tilth_files would search in the project, not /
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(cwd, project_path.canonicalize().unwrap());

        // Restore
        std::env::set_current_dir(orig_cwd).unwrap();
    }

    // -- package_root fallback from subdirectory ------------------------------

    #[test]
    fn package_root_finds_project_from_subdirectory() {
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path();
        std::fs::write(
            project_path.join("Cargo.toml"),
            "[package]\nname = \"test\"",
        )
        .unwrap();
        let subdir = project_path.join("src").join("deep").join("nested");
        std::fs::create_dir_all(&subdir).unwrap();

        // package_root from the nested subdir should find the project root
        let root = crate::lang::package_root(&subdir);
        assert!(root.is_some(), "package_root should find the project");
        // Compare canonicalized paths to handle macOS /var -> /private/var symlinks
        let root_canon = root.unwrap().canonicalize().unwrap();
        let expected_canon = project_path.canonicalize().unwrap();
        assert_eq!(root_canon, expected_canon);
    }
}
