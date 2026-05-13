use std::fmt::Write as _;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::OutlineCache;
use crate::index::bloom::BloomFilterCache;
use crate::session::Session;
use crate::timeout::{self, spawn_with_timeout, SpawnFailure, ThreadTracker};

/// Shared dependencies passed through the request → dispatch pipeline.
#[derive(Clone)]
pub(crate) struct Services {
    pub(crate) cache: Arc<OutlineCache>,
    pub(crate) session: Arc<Session>,
    pub(crate) bloom: Arc<BloomFilterCache>,
    pub(crate) tracker: Arc<ThreadTracker>,
    pub(crate) edit_mode: bool,
}

impl Services {
    pub(crate) fn new(edit_mode: bool) -> Self {
        Self {
            cache: Arc::new(OutlineCache::new()),
            session: Arc::new(Session::new()),
            bloom: Arc::new(BloomFilterCache::new()),
            tracker: Arc::new(ThreadTracker::new()),
            edit_mode,
        }
    }
}

// Sent to the LLM via the MCP `instructions` field during initialization.
// Source of truth: prompts/mcp-base.md and prompts/mcp-edit.md.
// AGENTS.md is regenerated from these via scripts/regen-agents-md.sh.
const SERVER_INSTRUCTIONS: &str = include_str!("../prompts/mcp-base.md");
const EDIT_MODE_EXTRA: &str = include_str!("../prompts/mcp-edit.md");

/// Compose the `instructions` field for MCP `initialize`. Strips trailing
/// whitespace from the embedded markdown so trailing file newlines don't leak
/// into the wire format, then reinjects `\n\n` between sections. Leading bytes
/// are preserved verbatim so prompt files remain byte-stable on the wire.
fn build_instructions(edit_mode: bool, overview: &str) -> String {
    let base = SERVER_INSTRUCTIONS.trim_end();
    let mut out = String::with_capacity(SERVER_INSTRUCTIONS.len() + EDIT_MODE_EXTRA.len() + 64);
    if !overview.is_empty() {
        out.push_str(overview);
        out.push_str("\n\n");
    }
    out.push_str(base);
    if edit_mode {
        out.push_str("\n\n");
        out.push_str(EDIT_MODE_EXTRA.trim_end());
    }
    out
}

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
        // On invalid UTF-8 in a percent-encoded path, fall back to the
        // original input rather than substituting U+FFFD replacements.
        let decoded = percent_encoding::percent_decode_str(raw_path)
            .decode_utf8()
            .map_or_else(|_| raw_path.to_string(), std::borrow::Cow::into_owned);
        let path = PathBuf::from(decoded);
        if path.is_dir() {
            return Some(path);
        }
    }
    None
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
    let edit_mode = services.edit_mode;
    match req.method.as_str() {
        "initialize" => {
            let overview = if std::env::var("TILTH_NO_OVERVIEW").is_ok() {
                String::new()
            } else {
                let cwd = std::env::current_dir().unwrap_or_default();
                crate::overview::fingerprint(&cwd)
            };
            let instructions = build_instructions(edit_mode, &overview);
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

/// No classifier involved — the caller specifies the tool explicitly.
fn dispatch_tool(services: &Services, tool: &str, args: &Value) -> Result<String, String> {
    let edit_mode = services.edit_mode;
    match tool {
        "tilth_read" => tool_read(args, &services.cache, &services.session, edit_mode),
        "tilth_search" => tool_search(args, &services.cache, &services.session, &services.bloom),
        "tilth_files" => tool_files(args),
        "tilth_deps" => tool_deps(args, &services.bloom),
        "tilth_diff" => tool_diff(args),
        "tilth_session" => tool_session(args, &services.session),
        "tilth_edit" if edit_mode => tool_edit(args, &services.session, &services.bloom),
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

    let paths_arr = match args.get("paths") {
        Some(v) => v.as_array().ok_or(
            "paths must be an array of file paths (use single-element array for one file)",
        )?,
        None => {
            return Err("missing required parameter: paths (array of file paths, use single-element array for one file)".into());
        }
    };

    if paths_arr.is_empty() {
        return Err("paths must contain at least one file".into());
    }
    if paths_arr.len() > 20 {
        return Err(format!(
            "batch read limited to 20 files (got {})",
            paths_arr.len()
        ));
    }

    let paths: Vec<PathBuf> = paths_arr
        .iter()
        .map(|p| {
            p.as_str()
                .ok_or("paths must be an array of strings")
                .map(PathBuf::from)
        })
        .collect::<Result<_, _>>()?;

    let section = args.get("section").and_then(|v| v.as_str());
    let sections_arr = args.get("sections").and_then(|v| v.as_array());
    let full = args
        .get("full")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if section.is_some() && sections_arr.is_some() {
        return Err("provide either section (single) or sections (array), not both".into());
    }

    // section/sections/full only apply when reading a single file.
    let has_single_file_args = section.is_some() || sections_arr.is_some() || full;
    if has_single_file_args && paths.len() > 1 {
        return Err(
            "section, sections, and full are only valid with a single-element paths array".into(),
        );
    }

    // Multi-file batch: per-file smart view applies, but no related-file hints
    // (those only make sense for whole-file reads of a single target).
    if paths.len() > 1 {
        let combined = crate::read::read_batch(&paths, cache, session, edit_mode);
        return Ok(apply_budget(combined, budget));
    }

    let path = paths.into_iter().next().expect("paths non-empty");

    // Multi-section path: bypass smart view + related-file hints (those only
    // apply to whole-file reads).
    if let Some(arr) = sections_arr {
        let ranges: Vec<&str> = arr
            .iter()
            .map(|v| v.as_str().ok_or("sections must be an array of strings"))
            .collect::<Result<Vec<_>, _>>()?;
        if ranges.is_empty() {
            return Err("sections must contain at least one range".into());
        }
        if ranges.len() > 20 {
            return Err(format!(
                "sections limited to 20 per call (got {})",
                ranges.len()
            ));
        }
        session.record_read(&path);
        let output = match budget {
            Some(b) => crate::read::read_ranges_with_budget(&path, &ranges, edit_mode, b)
                .map_err(|e| e.to_string())?,
            None => {
                crate::read::read_ranges(&path, &ranges, edit_mode).map_err(|e| e.to_string())?
            }
        };
        return Ok(output);
    }

    session.record_read(&path);
    let mut output = crate::read::read_file(&path, section, full, cache, edit_mode)
        .map_err(|e| e.to_string())?;

    // Append related-file hint for outlined code files (not section reads).
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
        "symbol" | "any" => {
            use crate::search::symbol::SymbolMode;
            let mode = if kind == "symbol" {
                SymbolMode::Strict
            } else {
                SymbolMode::Any
            };
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
                        queries[0], &scope, cache, session, bloom, expand, context, glob, mode,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, bloom, expand, context, glob, mode,
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
                query, &scope, bloom, expand, context, glob,
            )
        }
        _ => {
            return Err(format!(
                "unknown search kind: {kind}. Use: symbol, any, content, regex, callers"
            ))
        }
    }
    .map_err(|e| e.to_string())?;

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(output, budget));
    Ok(result)
}

fn tool_files(args: &Value) -> Result<String, String> {
    let (scope, scope_warning) = resolve_scope(args);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let patterns_arr = match args.get("patterns") {
        Some(v) => v.as_array().ok_or(
            "patterns must be an array of globs (use single-element array for one pattern)",
        )?,
        None => {
            return Err("missing required parameter: patterns (array of globs, use single-element array for one pattern)".into());
        }
    };

    if patterns_arr.is_empty() {
        return Err("patterns must contain at least one glob".into());
    }
    if patterns_arr.len() > 20 {
        return Err(format!(
            "patterns limited to 20 per call (got {})",
            patterns_arr.len()
        ));
    }
    let patterns: Vec<&str> = patterns_arr
        .iter()
        .map(|v| v.as_str().ok_or("patterns must be an array of strings"))
        .collect::<Result<Vec<_>, _>>()?;

    let mut blocks = Vec::with_capacity(patterns.len());
    for p in &patterns {
        let block = crate::search::search_glob(p, &scope).map_err(|e| e.to_string())?;
        blocks.push(block);
    }
    let combined = blocks.join("\n\n");

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(combined, budget));
    Ok(result)
}

fn tool_deps(args: &Value, bloom: &Arc<BloomFilterCache>) -> Result<String, String> {
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

    let deps_result =
        crate::search::deps::analyze_deps(&path, &scope, bloom).map_err(|e| e.to_string())?;
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

/// Parse one `files[]` entry. Parse errors are deferred onto the task so a
/// malformed entry surfaces as a per-file failure instead of aborting the
/// whole batch.
fn parse_file_edit(index: usize, val: &Value) -> crate::edit::FileEditTask {
    use crate::edit::FileEditTask;

    let Some(path_str) = val.get("path").and_then(|v| v.as_str()) else {
        return FileEditTask::ParseError {
            label: format!("files[{index}]"),
            msg: "missing 'path'".into(),
        };
    };
    let Some(edits_val) = val.get("edits").and_then(|v| v.as_array()) else {
        return FileEditTask::ParseError {
            label: path_str.to_string(),
            msg: "missing 'edits' array".into(),
        };
    };
    if edits_val.is_empty() {
        return FileEditTask::ParseError {
            label: path_str.to_string(),
            msg: "'edits' array is empty".into(),
        };
    }

    let mut edits = Vec::with_capacity(edits_val.len());
    for (i, e) in edits_val.iter().enumerate() {
        match parse_edit_entry(i, e) {
            Ok(edit) => edits.push(edit),
            Err(msg) => {
                return FileEditTask::ParseError {
                    label: path_str.to_string(),
                    msg,
                };
            }
        }
    }

    FileEditTask::Ready {
        path: PathBuf::from(path_str),
        edits,
    }
}

/// Parse a single `edits[]` entry. Flat early-returns keep nesting shallow.
fn parse_edit_entry(i: usize, e: &Value) -> Result<crate::edit::Edit, String> {
    let start_str = e
        .get("start")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("edit[{i}]: missing 'start'"))?;
    let (start_line, start_hash) = crate::format::parse_anchor(start_str)
        .ok_or_else(|| format!("edit[{i}]: invalid start anchor '{start_str}'"))?;
    let (end_line, end_hash) = match e.get("end").and_then(|v| v.as_str()) {
        Some(end_str) => crate::format::parse_anchor(end_str)
            .ok_or_else(|| format!("edit[{i}]: invalid end anchor '{end_str}'"))?,
        None => (start_line, start_hash),
    };
    let content = e
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("edit[{i}]: missing 'content'"))?;
    Ok(crate::edit::Edit {
        start_line,
        start_hash,
        end_line,
        end_hash,
        content: content.to_string(),
    })
}

fn tool_edit(
    args: &Value,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let files_val = args
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("missing required parameter: files (array of {path, edits})")?;

    if files_val.is_empty() {
        return Err("files array is empty".into());
    }
    if files_val.len() > 20 {
        return Err(format!(
            "batch edit limited to 20 files (got {})",
            files_val.len()
        ));
    }

    let show_diff = args
        .get("diff")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let tasks: Vec<crate::edit::FileEditTask> = files_val
        .iter()
        .enumerate()
        .map(|(i, v)| parse_file_edit(i, v))
        .collect();

    // Record reads only for files whose edits actually committed. Hash
    // mismatches, parse errors, and I/O failures didn't change the file, so
    // they shouldn't inflate the session activity counter.
    let outcome = crate::edit::apply_batch(tasks, bloom, show_diff)?;
    for path in &outcome.applied {
        session.record_read(path);
    }
    Ok(outcome.output)
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

    let result = if services.tracker.is_at_cap() {
        Err(
            "server busy: too many prior operations still running after timeout. \
             Wait or set TILTH_TIMEOUT=<seconds> higher."
                .into(),
        )
    } else {
        run_tool_with_timeout(services, tool_name, args, timeout::request_timeout())
    };

    build_response(req.id.as_ref(), result)
}

fn build_response(id: Option<&Value>, result: Result<String, String>) -> JsonRpcResponse {
    let (text, is_error) = match result {
        Ok(output) => (output, false),
        Err(e) => (e, true),
    };
    let mut payload = serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    });
    if is_error {
        payload["isError"] = Value::Bool(true);
    }
    JsonRpcResponse {
        jsonrpc: "2.0",
        id: id.cloned(),
        result: Some(payload),
        error: None,
    }
}

fn run_tool_with_timeout(
    services: &Services,
    tool_name: &str,
    args: &Value,
    timeout: Duration,
) -> Result<String, String> {
    let services_worker = services.clone();
    let tool = tool_name.to_string();
    let args_owned = args.clone();

    let result = spawn_with_timeout(&services.tracker, timeout, move || {
        dispatch_tool(&services_worker, &tool, &args_owned)
    });

    match result {
        Ok(inner) => inner,
        Err(SpawnFailure::Timeout) => Err(format!(
            "tool timed out after {}s — the operation took too long. \
             Try: reduce scope, use section instead of full, or set \
             TILTH_TIMEOUT=<seconds> to increase the limit.",
            timeout.as_secs()
        )),
        Err(SpawnFailure::Panic) => Err("tool panicked during execution".into()),
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
                        "description": "Symbol name, text string, or regex pattern to search for. e.g. 'resolve_dependencies' or 'ServeHTTP,Next' for multi-symbol lookup."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Only use scope to search a specific subdirectory. DO NOT USE scope if you want to search the current working directory (initial search)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["symbol", "any", "content", "regex", "callers"],
                        "default": "symbol",
                        "description": "Search type. symbol: declarations only — tree-sitter AST where supported, with keyword/heading fallbacks for code without grammars and for markdown. No comment/string hits. any: symbol-name matches including comments/strings/usages. content: literal text. regex: regex pattern. callers: find all call sites of a symbol."
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
                        "description": "File paths to read in one call (max 20). ALWAYS pass every file you need in one call — never call tilth_read twice in a row. For a single file, use a one-element array: paths: [\"a.rs\"]. Each file gets independent smart handling."
                    },
                    "section": {
                        "type": "string",
                        "description": "Line range e.g. '45-89', or heading e.g. '## Architecture'. Bypasses smart view. Only valid with a single-element paths array. Use `sections` for multiple ranges."
                    },
                    "sections": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multiple ranges from the same file in one call. Each entry is a line range or heading. Only valid with a single-element paths array. Emits each block in user-supplied order, separated by `─── lines X-Y ───` delimiters. Mutually exclusive with `section`. Capped at 20 ranges."
                    },
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Force full content output, bypass smart outlining. Only valid with a single-element paths array."
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
            "description": "Find files matching glob patterns. Replaces find/ls/pwd and the host Glob tool — use this for all file discovery. Returns matched file paths sorted by relevance with token size estimates.",
            "inputSchema": {
                "type": "object",
                "required": ["patterns"],
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "Glob patterns to run in one call against the same scope (max 20). ALWAYS pass every glob you need in one call — never call tilth_files twice in a row. For a single glob, use a one-element array: patterns: [\"*.rs\"]. Each pattern emits its own `# Glob: ...` block, separated by a blank line."
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
            "description": "Batch edit one or more files in one call using hashline anchors from tilth_read. ALWAYS group all ready edits into a single tilth_edit call — never call tilth_edit twice in a row when one `files` array could include everything. Each file is processed independently: a hash mismatch on one file does not block the others. Partial success returns isError: false, so scan every per-file `## <path>` section for failures. A malformed edit fails that whole file before any of its edits apply. Each path may appear at most once per call. Max 20 files per call.",
            "inputSchema": {
                "type": "object",
                "required": ["files"],
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "One entry per file. Use a single-element array for a single-file edit. Each path must be unique within the call; group all edits for that path here.",
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

    // -- build_instructions ---------------------------------------------------

    #[test]
    fn build_instructions_base_has_expected_anchors() {
        let s = build_instructions(false, "");
        assert!(
            s.starts_with("tilth — code intelligence MCP server."),
            "missing opening anchor: {:?}",
            &s[..60.min(s.len())]
        );
        assert!(
            s.ends_with("[+] added, [-] deleted, [~] body changed, [~:sig] signature changed"),
            "missing closing anchor"
        );
        assert!(
            !s.contains("tilth_edit:"),
            "edit-mode content leaked into base"
        );
    }

    #[test]
    fn build_instructions_edit_appends_extra_with_blank_line() {
        let s = build_instructions(true, "");
        assert!(
            s.contains(
                "[+] added, [-] deleted, [~] body changed, [~:sig] signature changed\n\ntilth_edit:"
            ),
            "expected single blank-line separator between base and edit-mode sections"
        );
        assert!(s.ends_with("DO NOT use the host Edit tool. Use tilth_edit for all edits."));
    }

    #[test]
    fn build_instructions_overview_prepends_with_blank_line() {
        let s = build_instructions(false, "OVERVIEW");
        assert!(
            s.starts_with("OVERVIEW\n\ntilth — "),
            "overview should be followed by blank line then base"
        );
    }

    #[test]
    fn build_instructions_no_trailing_whitespace() {
        for &edit in &[false, true] {
            let s = build_instructions(edit, "");
            assert!(
                !s.ends_with('\n') && !s.ends_with(' '),
                "wire output must not end with whitespace (edit={edit})"
            );
        }
    }

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

    // -- tool_files multi-pattern --------------------------------------------

    /// Build a small scratch project with .rs and .toml files, switch cwd to
    /// it, and return the tempdir guard so the caller controls cleanup.
    fn scratch_project() -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        let p = project.path();
        std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"t\"").unwrap();
        std::fs::create_dir(p.join("src")).unwrap();
        std::fs::write(p.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(p.join("src/lib.rs"), "pub fn x() {}").unwrap();
        project
    }

    #[test]
    fn tool_files_patterns_emits_one_block_per_pattern() {
        let project = scratch_project();
        let args = serde_json::json!({
            "patterns": ["*.rs", "*.toml"],
            "scope": project.path().to_str().unwrap(),
        });
        let out = tool_files(&args).expect("tool_files should succeed");
        // Two `# Glob:` headers — one per pattern.
        let header_count = out.matches("# Glob:").count();
        assert_eq!(header_count, 2, "expected 2 Glob headers, got: {out}");
        assert!(out.contains("\"*.rs\""), "missing rs header in: {out}");
        assert!(out.contains("\"*.toml\""), "missing toml header in: {out}");
        // Files from both patterns appear in the combined output.
        assert!(out.contains("main.rs"), "missing main.rs in: {out}");
        assert!(out.contains("Cargo.toml"), "missing Cargo.toml in: {out}");
    }

    #[test]
    fn tool_files_empty_patterns_errors() {
        let args = serde_json::json!({ "patterns": [] });
        let err = tool_files(&args).expect_err("expected empty-patterns error");
        assert!(err.contains("at least one"), "unexpected error: {err}");
    }

    #[test]
    fn tool_files_patterns_capped_at_20() {
        let twenty_one: Vec<&str> = vec!["*.rs"; 21];
        let args = serde_json::json!({ "patterns": twenty_one });
        let err = tool_files(&args).expect_err("expected cap error");
        assert!(err.contains("limited to 20"), "unexpected error: {err}");
    }

    #[test]
    fn tool_files_missing_patterns_errors() {
        let args = serde_json::json!({});
        let err = tool_files(&args).expect_err("expected missing-patterns error");
        assert!(
            err.contains("missing required parameter: patterns"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tool_files_singular_pattern_rejected() {
        let args = serde_json::json!({ "pattern": "*.rs" });
        let err = tool_files(&args).expect_err("singular `pattern` must be rejected");
        assert!(
            err.contains("missing required parameter: patterns"),
            "unexpected error: {err}"
        );
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

    fn services_with_tracker(tracker: Arc<ThreadTracker>) -> Services {
        Services {
            cache: Arc::new(OutlineCache::new()),
            session: Arc::new(Session::new()),
            bloom: Arc::new(BloomFilterCache::new()),
            tracker,
            edit_mode: false,
        }
    }

    #[test]
    fn abandoned_hard_cap_returns_server_busy() {
        let tracker = Arc::new(ThreadTracker::new());
        tracker.saturate();
        let services = services_with_tracker(tracker);

        let req = JsonRpcRequest {
            _jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "tilth_files",
                "arguments": { "patterns": ["*.rs"] }
            }),
        };

        let resp = handle_tool_call(&req, &services);

        let result = resp.result.expect("response must have a result field");
        let is_error = result
            .get("isError")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        assert!(is_error, "response must have isError: true");

        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("server busy"),
            "error message must contain 'server busy', got: {text}"
        );
        assert!(
            text.contains("TILTH_TIMEOUT"),
            "error message must include TILTH_TIMEOUT hint, got: {text}"
        );
    }

    #[test]
    fn tool_read_singular_path_rejected() {
        let args = serde_json::json!({ "path": "a.rs" });
        let cache = OutlineCache::new();
        let session = Session::new();
        let err = tool_read(&args, &cache, &session, false)
            .expect_err("singular `path` must be rejected");
        assert!(
            err.contains("missing required parameter: paths"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tool_read_paths_wrong_type_reports_type_error() {
        // A scalar (or any non-array) value for `paths` should produce a
        // type-specific error, not the generic "missing" message.
        let args = serde_json::json!({ "paths": "a.rs" });
        let cache = OutlineCache::new();
        let session = Session::new();
        let err = tool_read(&args, &cache, &session, false)
            .expect_err("scalar `paths` must be rejected as wrong type");
        assert!(
            err.contains("paths must be an array"),
            "unexpected error: {err}"
        );
        assert!(
            !err.contains("missing required parameter"),
            "wrong-type error must not claim the param is missing: {err}"
        );
    }

    #[test]
    fn tool_files_patterns_wrong_type_reports_type_error() {
        let args = serde_json::json!({ "patterns": "*.rs" });
        let err = tool_files(&args).expect_err("scalar `patterns` must be rejected as wrong type");
        assert!(
            err.contains("patterns must be an array"),
            "unexpected error: {err}"
        );
        assert!(
            !err.contains("missing required parameter"),
            "wrong-type error must not claim the param is missing: {err}"
        );
    }

    #[test]
    fn tool_read_section_with_multiple_paths_rejected() {
        let args = serde_json::json!({
            "paths": ["a.rs", "b.rs"],
            "section": "1-10",
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let err = tool_read(&args, &cache, &session, false)
            .expect_err("section + multi-path must be rejected");
        assert!(
            err.contains("single-element paths array"),
            "unexpected error: {err}"
        );
    }

    /// Batch reads must return the content of every submitted path — no file
    /// is dropped or reordered on the way through the tool handler.
    #[test]
    fn batch_read_returns_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let file_count = 5usize;

        let paths: Vec<PathBuf> = (0..file_count)
            .map(|i| {
                let p = dir.path().join(format!("file{i}.txt"));
                std::fs::write(&p, format!("content-of-file-{i}")).unwrap();
                p
            })
            .collect();

        let paths_json: Vec<serde_json::Value> = paths
            .iter()
            .map(|p| serde_json::json!(p.to_str().unwrap()))
            .collect();

        let args = serde_json::json!({ "paths": paths_json });
        let cache = OutlineCache::new();
        let session = Session::new();

        let result = tool_read(&args, &cache, &session, false).expect("batch read must succeed");

        for i in 0..file_count {
            assert!(
                result.contains(&format!("content-of-file-{i}")),
                "output must contain content of file {i}"
            );
        }
    }

    // -- batch tool_edit --------------------------------------------------------

    fn anchor_for(content: &str, line: usize) -> String {
        let lines: Vec<_> = content.lines().collect();
        let h = crate::format::line_hash(lines[line - 1].as_bytes());
        format!("{line}:{h:03x}")
    }

    /// Anchor with a hash guaranteed not to match the line's real hash.
    /// XOR-flipping a bit can't collide with the original — used to force
    /// hash-mismatch paths without depending on hardcoded sentinel values.
    fn wrong_anchor_for(content: &str, line: usize) -> String {
        let lines: Vec<_> = content.lines().collect();
        let real = crate::format::line_hash(lines[line - 1].as_bytes());
        let wrong = (real ^ 0x1) & 0xFFF;
        format!("{line}:{wrong:03x}")
    }

    fn edit_services() -> (Session, Arc<BloomFilterCache>) {
        (Session::new(), Arc::new(BloomFilterCache::new()))
    }

    #[test]
    fn batch_edit_two_files_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let a_content = "alpha\nbravo\ncharlie\n";
        let b_content = "uno\ndos\ntres\n";
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, a_content).unwrap();
        std::fs::write(&b, b_content).unwrap();

        let args = serde_json::json!({
            "files": [
                {
                    "path": a.to_str().unwrap(),
                    "edits": [{ "start": anchor_for(a_content, 2), "content": "BRAVO" }]
                },
                {
                    "path": b.to_str().unwrap(),
                    "edits": [{ "start": anchor_for(b_content, 1), "content": "UNO" }]
                }
            ]
        });

        let (session, bloom) = edit_services();
        let out = tool_edit(&args, &session, &bloom).expect("batch should succeed");

        assert!(
            out.contains(a.to_str().unwrap()),
            "must mention file a: {out}"
        );
        assert!(
            out.contains(b.to_str().unwrap()),
            "must mention file b: {out}"
        );
        assert!(
            !out.contains("error:") && !out.contains("hash mismatch"),
            "successful batch must not contain error markers: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&a).expect("file a should be readable"),
            "alpha\nBRAVO\ncharlie\n"
        );
        assert_eq!(
            std::fs::read_to_string(&b).expect("file b should be readable"),
            "UNO\ndos\ntres\n"
        );
    }

    /// A bad-hash failure on file B must not block file A from applying;
    /// the response is `Ok` because at least one file succeeded, and includes
    /// both sections separated by `---`.
    #[test]
    fn batch_edit_partial_failure_does_not_block_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let a_content = "first\nsecond\nthird\n";
        let b_content = "one\ntwo\nthree\n";
        let a = dir.path().join("first.txt");
        let b = dir.path().join("second.txt");
        std::fs::write(&a, a_content).unwrap();
        std::fs::write(&b, b_content).unwrap();

        let args = serde_json::json!({
            "files": [
                {
                    "path": a.to_str().unwrap(),
                    "edits": [{ "start": anchor_for(a_content, 2), "content": "SECOND" }]
                },
                {
                    "path": b.to_str().unwrap(),
                    "edits": [{ "start": "1:000", "content": "ONE" }]
                }
            ]
        });

        let (session, bloom) = edit_services();
        let out = tool_edit(&args, &session, &bloom).expect("batch is Ok if any file succeeds");

        assert_eq!(
            std::fs::read_to_string(&a).expect("file a should be readable"),
            "first\nSECOND\nthird\n",
            "first file must have applied"
        );
        assert_eq!(
            std::fs::read_to_string(&b).expect("file b should be readable"),
            b_content,
            "second file must remain untouched"
        );
        assert!(
            out.contains("---"),
            "must separate per-file sections: {out}"
        );
        let (a_section, b_section) = out.split_once("\n\n---\n\n").expect("two sections");
        assert!(
            !a_section.contains("hash mismatch"),
            "file a's section must not report hash mismatch: {a_section}"
        );
        assert!(
            b_section.contains("hash mismatch"),
            "file b's section must report hash mismatch: {b_section}"
        );
    }

    #[test]
    fn batch_edit_over_limit_rejected() {
        let tmp = std::env::temp_dir();
        let mut files = Vec::with_capacity(21);
        for i in 0..21 {
            files.push(serde_json::json!({
                "path": tmp.join(format!("tilth_nonexistent_{i}.txt")).to_str().unwrap(),
                "edits": [{ "start": "1:000", "content": "x" }]
            }));
        }
        let args = serde_json::json!({ "files": files });

        let (session, bloom) = edit_services();
        let err = tool_edit(&args, &session, &bloom).expect_err("21 files must be rejected");
        assert!(err.contains("limited to 20"), "must mention limit: {err}");
    }

    /// All-failed batch returns `Err` so the MCP response sets `isError: true`.
    #[test]
    fn batch_edit_all_failed_returns_err() {
        let tmp = std::env::temp_dir();
        let p1 = tmp.join("tilth_does_not_exist_xyz_1.txt");
        let p2 = tmp.join("tilth_does_not_exist_xyz_2.txt");
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);

        let args = serde_json::json!({
            "files": [
                {
                    "path": p1.to_str().unwrap(),
                    "edits": [{ "start": "1:000", "content": "x" }]
                },
                {
                    "path": p2.to_str().unwrap(),
                    "edits": [{ "start": "1:000", "content": "x" }]
                }
            ]
        });

        let (session, bloom) = edit_services();
        let err = tool_edit(&args, &session, &bloom).expect_err("all-failed → Err");
        assert!(
            err.contains("tilth_does_not_exist_xyz_1"),
            "must include file 1: {err}"
        );
        assert!(
            err.contains("tilth_does_not_exist_xyz_2"),
            "must include file 2: {err}"
        );
        assert!(err.contains("---"), "must separate sections: {err}");
    }

    #[test]
    fn batch_edit_empty_files_array_rejected() {
        let args = serde_json::json!({ "files": [] });
        let (session, bloom) = edit_services();
        let err =
            tool_edit(&args, &session, &bloom).expect_err("empty files array must be rejected");
        assert!(err.contains("empty"), "must mention empty: {err}");
    }

    /// Empty `edits` array must be rejected at parse time. Otherwise
    /// `apply_edits` short-circuits to `Applied` without writing the file,
    /// and the path would still flow into `BatchOutcome.applied` — inflating
    /// the session read counter for a file that was never touched.
    #[test]
    fn batch_edit_empty_edits_array_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        let untouched = dir.path().join("untouched.txt");
        std::fs::write(&good, "alpha\n").unwrap();
        std::fs::write(&untouched, "beta\n").unwrap();

        let args = serde_json::json!({
            "files": [
                {
                    "path": good.to_str().unwrap(),
                    "edits": [{ "start": anchor_for("alpha\n", 1), "content": "ALPHA" }],
                },
                {
                    "path": untouched.to_str().unwrap(),
                    "edits": [],
                },
            ]
        });

        let (session, bloom) = edit_services();
        let out = tool_edit(&args, &session, &bloom).expect("good half keeps batch alive");

        assert!(
            out.contains("'edits' array is empty"),
            "empty edits must surface as parse error: {out}"
        );
        assert_eq!(std::fs::read_to_string(&untouched).unwrap(), "beta\n");
        assert_eq!(
            session.reads_count(),
            1,
            "empty-edits file must not be counted as read"
        );
    }

    // -- record_read counter gating -------------------------------------------

    /// A batch with one good file and one file with a deliberate hash
    /// mismatch must record exactly one read — only the file whose edit
    /// actually committed. Guards against the prior bug where every `Ready`
    /// task counted as a read regardless of `apply_batch` outcome.
    #[test]
    fn tool_edit_records_read_only_for_applied_files() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        let bad = dir.path().join("bad.txt");
        std::fs::write(&good, "alpha\n").unwrap();
        std::fs::write(&bad, "beta\n").unwrap();

        let args = serde_json::json!({
            "files": [
                {
                    "path": good.to_str().unwrap(),
                    "edits": [{ "start": anchor_for("alpha\n", 1), "content": "ALPHA" }],
                },
                {
                    "path": bad.to_str().unwrap(),
                    // Derive a guaranteed-wrong hash so the mismatch is
                    // forced regardless of what `beta` actually hashes to.
                    "edits": [{ "start": wrong_anchor_for("beta\n", 1), "content": "BETA" }],
                },
            ]
        });

        let (session, bloom) = edit_services();
        let out = tool_edit(&args, &session, &bloom).expect("good half should keep batch alive");

        assert!(out.contains("hash mismatch"), "bad file reports mismatch");
        assert_eq!(std::fs::read_to_string(&good).unwrap(), "ALPHA\n");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "beta\n");
        assert_eq!(
            session.reads_count(),
            1,
            "only the applied file should be counted as read"
        );
    }

    /// Boundary: an IO failure on a `Ready` task (file doesn't exist) is a
    /// different code path than a hash mismatch — it never reaches the hash
    /// check. The applied-list gate must still exclude it from `record_read`.
    #[test]
    fn tool_edit_io_failure_excludes_from_reads_count() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        std::fs::write(&good, "alpha\n").unwrap();
        let missing = dir.path().join("nonexistent.txt"); // never created

        let args = serde_json::json!({
            "files": [
                {
                    "path": good.to_str().unwrap(),
                    "edits": [{ "start": anchor_for("alpha\n", 1), "content": "ALPHA" }],
                },
                {
                    "path": missing.to_str().unwrap(),
                    // Hash value is irrelevant — the file read fails first.
                    "edits": [{ "start": "1:000", "content": "X" }],
                },
            ]
        });

        let (session, bloom) = edit_services();
        let out = tool_edit(&args, &session, &bloom).expect("good half keeps batch alive");

        assert!(
            out.contains(&format!("## {}", missing.display())),
            "missing file should still get a section header: {out}"
        );
        assert_eq!(std::fs::read_to_string(&good).unwrap(), "ALPHA\n");
        assert_eq!(
            session.reads_count(),
            1,
            "IO failures must not inflate the read counter"
        );
    }

    /// Boundary: when every entry is a parse error, `applied` is empty so
    /// `apply_batch` returns `Err`. `tool_edit` must propagate that error AND
    /// leave the read counter at zero — no `Ready` task ever existed.
    #[test]
    fn tool_edit_all_parse_errors_returns_err_with_no_reads() {
        let args = serde_json::json!({
            "files": [
                { "path": "a.txt" }, // missing 'edits'
                { "path": "b.txt", "edits": [{ "no_start": "x" }] }, // malformed edit
            ]
        });

        let (session, bloom) = edit_services();
        let err = tool_edit(&args, &session, &bloom)
            .expect_err("an all-parse-error batch must surface as Err");

        assert!(err.contains("a.txt") || err.contains("b.txt"), "err: {err}");
        assert_eq!(
            session.reads_count(),
            0,
            "no Ready task means no read should be recorded"
        );
    }

    /// Boundary: a mixed parse-error + good-file batch at the wire layer.
    /// The record_read gate sits in `tool_edit`, not in `apply_batch`, so it
    /// needs explicit wire-level coverage.
    #[test]
    fn tool_edit_mixed_parse_error_and_good_file_records_only_good() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        std::fs::write(&good, "alpha\n").unwrap();

        let args = serde_json::json!({
            "files": [
                {
                    "path": good.to_str().unwrap(),
                    "edits": [{ "start": anchor_for("alpha\n", 1), "content": "ALPHA" }],
                },
                { "path": "malformed.txt" }, // parse error: missing 'edits'
            ]
        });

        let (session, bloom) = edit_services();
        let out = tool_edit(&args, &session, &bloom).expect("good half keeps batch alive");

        assert!(
            out.contains("missing 'edits'"),
            "parse error should surface in output: {out}"
        );
        assert_eq!(std::fs::read_to_string(&good).unwrap(), "ALPHA\n");
        assert_eq!(
            session.reads_count(),
            1,
            "parse errors must not inflate the read counter"
        );
    }
}
