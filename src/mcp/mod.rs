#[cfg(test)]
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
const SERVER_INSTRUCTIONS: &str = include_str!("../../prompts/mcp-base.md");
const EDIT_MODE_EXTRA: &str = include_str!("../../prompts/mcp-edit.md");

mod iso;
mod path_suffix;
mod tools;
mod tree;
mod write;

#[cfg(test)]
use tools::{resolve_scope, tool_edit, tool_files};
use tools::{
    tool_definitions, tool_deps, tool_diff, tool_list, tool_read, tool_search, tool_session,
    tool_write,
};

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

/// MCP server over stdio. When `edit_mode` is true, exposes `tilth_write` and
/// switches write-ready reads/search expansions to hashline output format.
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
        "tilth_search" => tool_search(
            args,
            &services.cache,
            &services.session,
            &services.bloom,
            edit_mode,
        ),
        "tilth_list" => tool_list(args),
        "tilth_deps" => tool_deps(args, &services.bloom),
        "tilth_diff" => tool_diff(args),
        "tilth_session" => tool_session(args, &services.session),
        "tilth_write" if edit_mode => tool_write(args, &services.session, &services.bloom),
        _ => Err(format!("unknown tool: {tool}")),
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
            s.starts_with("tilth — AST-aware code intelligence MCP server."),
            "missing opening anchor: {:?}",
            &s[..60.min(s.len())]
        );
        assert!(
            s.contains("[+] added, [-] deleted, [~] body changed, [~:sig] signature changed"),
            "missing closing anchor"
        );
        assert!(
            !s.contains("tilth_write is exposed"),
            "edit-mode pointer leaked into base"
        );
    }

    #[test]
    fn build_instructions_edit_appends_thin_pointer() {
        let s = build_instructions(true, "");
        assert!(
            s.contains("tilth_write is exposed"),
            "expected thin tilth_write pointer in edit-mode instructions"
        );
        assert!(
            !s.contains("Legacy alias: tilth_edit"),
            "tilth_edit must not be advertised"
        );
        // Spec AC-12: server-level instructions stay thin. Detailed
        // request shape, modes, and batching rules live in the tilth_write
        // tool description, not in the server prompt.
        assert!(
            !s.contains("\"files\":"),
            "request-shape JSON leaked into server prompt: {s}"
        );
        assert!(
            !s.contains("ALWAYS group edits"),
            "batching rule leaked into server prompt: {s}"
        );
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
    fn tool_files_singular_pattern_accepted_transitionally() {
        // v2: singular `pattern:` is accepted as a transitional alias.
        let args = serde_json::json!({ "pattern": "*.rs" });
        let out = tool_files(&args).expect("singular pattern accepted");
        assert!(out.contains("Glob"), "expected glob output: {out}");
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
                "name": "tilth_list",
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
    fn tool_read_singular_path_accepted_transitionally() {
        // v2: singular `path:` is accepted as a transitional alias. Pass a
        // nonexistent file so we exercise the alias parsing path without
        // requiring real fixture setup. The expected outcome is an error from
        // read::read_file (file not found), not the missing-param error.
        let args = serde_json::json!({ "path": "this-file-does-not-exist-xyz.rs" });
        let cache = OutlineCache::new();
        let session = Session::new();
        let err = tool_read(&args, &cache, &session, false)
            .expect_err("nonexistent file should error out");
        assert!(
            !err.contains("missing required parameter"),
            "singular path must be accepted, got: {err}"
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
    fn tool_read_unknown_mode_errors() {
        let args = serde_json::json!({
            "paths": ["a.rs"],
            "mode": "banana"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let err =
            tool_read(&args, &cache, &session, false).expect_err("unknown mode must be rejected");
        assert!(err.contains("unknown read mode"), "unexpected error: {err}");
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

    #[test]
    fn batch_read_mode_full_applies_to_all_paths() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.rs");
        let p2 = dir.path().join("b.rs");
        let large = format!(
            "fn only_signature() {{}}\n{}",
            "// padding padding padding padding\n".repeat(1000)
        );
        std::fs::write(&p1, &large).unwrap();
        std::fs::write(&p2, "fn small() {}\n").unwrap();

        let args = serde_json::json!({
            "paths": [p1.to_str().unwrap(), p2.to_str().unwrap()],
            "mode": "full"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("batch full read ok");
        assert!(
            out.contains("padding padding"),
            "large body must be included: {out}"
        );
        assert!(
            out.contains("fn small"),
            "small body must be included: {out}"
        );
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

    // -- v2 MCP surface -----------------------------------------------------

    #[test]
    fn tool_definitions_expose_v2_surface_only() {
        let tools = tool_definitions(true);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(serde_json::Value::as_str))
            .collect();
        assert!(names.contains(&"tilth_search"));
        assert!(names.contains(&"tilth_read"));
        assert!(names.contains(&"tilth_list"));
        assert!(names.contains(&"tilth_write"));
        assert!(
            !names.contains(&"tilth_files"),
            "tilth_files must be hidden"
        );
        assert!(!names.contains(&"tilth_edit"), "tilth_edit must be hidden");

        let search = tools
            .iter()
            .find(|t| t["name"] == "tilth_search")
            .expect("search tool");
        let search_props = &search["inputSchema"]["properties"];
        assert!(search_props.get("query").is_none(), "singular query hidden");
        assert!(search_props.get("kind").is_none(), "top-level kind hidden");
        let query_kind_enum = &search_props["queries"]["items"]["properties"]["kind"]["enum"];
        assert!(
            !query_kind_enum
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "any"),
            "kind:any must not be advertised"
        );

        let read = tools
            .iter()
            .find(|t| t["name"] == "tilth_read")
            .expect("read tool");
        let read_props = &read["inputSchema"]["properties"];
        assert!(read_props.get("section").is_none(), "section hidden");
        assert!(read_props.get("sections").is_none(), "sections hidden");
        assert!(read_props.get("full").is_none(), "full hidden");

        let list = tools
            .iter()
            .find(|t| t["name"] == "tilth_list")
            .expect("list tool");
        assert!(
            list["inputSchema"]["properties"].get("pattern").is_none(),
            "singular pattern hidden"
        );
    }

    /// `tilth_read` accepts the `path#n-m` suffix grammar.
    #[test]
    fn tool_read_line_range_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let spec = format!("{}#2-4", p.to_str().unwrap());
        let args = serde_json::json!({ "paths": [spec] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("suffix accepted");
        assert!(out.contains("l2"), "expected l2 in output: {out}");
        assert!(out.contains("l4"), "expected l4 in output: {out}");
        assert!(!out.contains("l5"), "must not include l5: {out}");
    }

    /// `tilth_read` heading suffix `path## Heading` resolves to that section.
    #[test]
    fn tool_read_heading_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.md");
        std::fs::write(&p, "# Top\nintro\n## Foo\nfoo body\n## Bar\nbar body\n").unwrap();
        // Path-suffix grammar: `path#<heading text>` (with internal space)
        let spec = format!("{}#Foo", p.to_str().unwrap());
        // Without internal space, it's classified as symbol — for headings
        // use form with `##`. Use heading-style suffix instead:
        let spec_heading = format!("{}### Bar", p.to_str().unwrap());
        let _ = spec; // unused: the symbol form would fail on .md (no Code lang)
        let args = serde_json::json!({ "paths": [spec_heading] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("heading suffix");
        assert!(out.contains("bar body"), "expected heading content: {out}");
    }

    /// `tilth_search` accepts `queries: [{query}]` and dispatches each.
    #[test]
    fn tool_search_queries_array_form() {
        let args = serde_json::json!({
            "queries": [{ "query": "build_instructions" }],
            "expand": 0
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("queries form");
        assert!(
            out.contains("\"if_modified_since\""),
            "expected JSON cache-token header: {out}"
        );
        assert!(
            out.contains("query: build_instructions"),
            "expected per-query header: {out}"
        );
    }

    /// `tilth_search` empty queries array errors clearly.
    #[test]
    fn tool_search_queries_empty_errors() {
        let args = serde_json::json!({ "queries": [] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let err = tool_search(&args, &cache, &session, &bloom, false).expect_err("empty errors");
        assert!(err.contains("empty"), "unexpected error: {err}");
    }

    /// `tilth_list` emits a tree with rolled-up token counts.
    #[test]
    fn tool_list_produces_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}").unwrap();
        let args = serde_json::json!({
            "patterns": ["*.rs"],
            "scope": dir.path().to_str().unwrap()
        });
        let out = tool_list(&args).expect("list ok");
        assert!(out.contains("src/"), "expected src/ in tree: {out}");
        assert!(out.contains("a.rs"), "expected a.rs: {out}");
        assert!(out.contains("tokens"), "expected token rollup: {out}");
    }

    /// `tilth_write` overwrite mode creates a new file.
    #[test]
    fn tool_write_overwrite_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.txt");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "content": "hello world\n"
            }]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("overwrite ok");
        assert!(
            out.contains("overwrite"),
            "expected overwrite report: {out}"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello world\n");
    }

    /// `tilth_write` append mode appends to existing or creates.
    #[test]
    fn tool_write_append_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log.txt");
        std::fs::write(&p, "start\n").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "append",
                "content": "more\n"
            }]
        });
        let (session, bloom) = edit_services();
        let _ = tool_write(&args, &session, &bloom).expect("append ok");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "start\nmore\n");
    }

    // -- v2 hardening (press) ----------------------------------------------

    /// `tool_write` hash-mode happy path: anchored edit applies, session
    /// records exactly one read for the touched file.
    #[test]
    fn tool_write_hash_mode_applies_anchored_edit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("src.txt");
        std::fs::write(&p, "alpha\nbeta\ngamma\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "hash",
                "edits": [{ "start": anchor_for("alpha\nbeta\ngamma\n", 2), "content": "BETA" }]
            }]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("hash apply ok");
        assert!(!out.contains("hash mismatch"), "must not mismatch: {out}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "alpha\nBETA\ngamma\n");
        assert_eq!(session.reads_count(), 1, "applied file is recorded");
    }

    /// `tool_write` hash-mode mismatch triggers the auto-fix probe and surfaces
    /// the relocation candidate in the response (strict fingerprint, exactly-
    /// one-match path). Spec criterion 9.
    #[test]
    fn tool_write_hash_mismatch_emits_auto_fix_probe() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("src.txt");
        // "target unique line" appears exactly once; agent's anchor hash is
        // wrong so apply_batch returns Err and auto-fix runs.
        std::fs::write(&p, "prefix\ntarget unique line\ntail\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "hash",
                "edits": [{ "start": wrong_anchor_for("prefix\ntarget unique line\ntail\n", 2), "content": "NEW" }]
            }]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("mismatch is surfaced, not Err");
        assert!(
            out.contains("hash mismatch"),
            "mismatch marker missing: {out}"
        );
        assert!(
            out.contains("auto-fix"),
            "auto-fix probe must run on mismatch: {out}"
        );
        // Spec criterion 9: exactly one match → apply edit at that new
        // location with the verbatim `auto-fixed: <old> → <new>` signal.
        assert!(
            out.contains("auto-fixed: 2 → 2"),
            "verbatim auto-fixed line missing: {out}"
        );
        // File IS mutated when auto-fix succeeds (one-match relocation).
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "prefix\nNEW\ntail\n",
            "single-match relocation must re-apply the edit"
        );
    }

    /// `tool_write` mixed-mode batch: overwrite + append + bad-mode coexist;
    /// per-file independence preserved.
    #[test]
    fn tool_write_mixed_mode_batch_independent_failures() {
        let dir = tempfile::tempdir().unwrap();
        let ow = dir.path().join("ow.txt");
        let ap = dir.path().join("ap.txt");
        std::fs::write(&ap, "start\n").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [
                { "path": ow.to_str().unwrap(), "mode": "overwrite", "content": "X\n" },
                { "path": ap.to_str().unwrap(), "mode": "append", "content": "Y\n" },
                { "path": dir.path().join("bogus.txt").to_str().unwrap(), "mode": "bogus", "content": "" }
            ]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("mixed batch ok");
        assert_eq!(std::fs::read_to_string(&ow).unwrap(), "X\n");
        assert_eq!(std::fs::read_to_string(&ap).unwrap(), "start\nY\n");
        assert!(out.contains("unknown mode"), "bad mode must surface: {out}");
    }

    /// `tool_write` overwrite mode honors `diff: true` and includes a diff
    /// block in the response.
    #[test]
    fn tool_write_overwrite_with_diff_includes_diff_block() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.txt");
        std::fs::write(&p, "old\n").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{ "path": p.to_str().unwrap(), "mode": "overwrite", "content": "new\n" }],
            "diff": true
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("overwrite ok");
        assert!(out.contains("── diff ──"), "diff block expected: {out}");
        assert!(out.contains("--- before"), "before marker expected: {out}");
        assert!(out.contains("+++ after"), "after marker expected: {out}");
        assert!(out.contains("- old"), "removed line marker expected: {out}");
        assert!(out.contains("+ new"), "added line marker expected: {out}");
    }

    /// `tool_write` rejects empty `files` array clearly.
    #[test]
    fn tool_write_empty_files_rejected() {
        let args = serde_json::json!({ "files": [] });
        let (session, bloom) = edit_services();
        let err = tool_write(&args, &session, &bloom).expect_err("empty must error");
        assert!(err.contains("empty"), "unexpected error: {err}");
    }

    /// `tool_read` with `if_modified_since` in the future returns an
    /// `(unchanged)` stub rather than reading the file. Spec criterion 11.
    #[test]
    fn tool_read_if_modified_since_future_returns_unchanged_stub() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.txt");
        std::fs::write(&p, "contents you should NOT see\n").unwrap();
        // Pick a timestamp well in the future; file mtime <= ts ⇒ unchanged.
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "if_modified_since": "2099-01-01T00:00:00Z"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("stub ok");
        assert!(out.contains("unchanged"), "expected stub marker: {out}");
        assert!(
            !out.contains("contents you should NOT see"),
            "body must not leak on unchanged stub: {out}"
        );
        assert!(
            out.contains("\"if_modified_since\""),
            "JSON cache-token header missing: {out}"
        );
    }

    /// `tool_read` with `if_modified_since` in the past (epoch) returns the
    /// actual file content. Boundary partner for the unchanged-stub test.
    #[test]
    fn tool_read_if_modified_since_past_returns_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("now.txt");
        std::fs::write(&p, "hello world\n").unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "if_modified_since": "1970-01-01T00:00:00Z"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("content ok");
        assert!(out.contains("hello world"), "expected body: {out}");
    }

    /// `tilth_read` `path#n` (FromLine) suffix returns from line n to end.
    #[test]
    fn tool_read_from_line_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "l1\nl2\nl3\nl4\n").unwrap();
        let spec = format!("{}#3", p.to_str().unwrap());
        let args = serde_json::json!({ "paths": [spec] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("from-line suffix ok");
        assert!(out.contains("l3"), "line 3 expected: {out}");
        assert!(out.contains("l4"), "line 4 expected: {out}");
        assert!(!out.contains("l1"), "line 1 must be excluded: {out}");
    }

    /// `mode: signature` emits hash-prefixed signature lines, not full bodies.
    #[test]
    fn tool_read_signature_mode_emits_hash_prefixed_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lib.rs");
        std::fs::write(
            &p,
            "fn signature_target() {\n    let body_marker = 42;\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "mode": "signature"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("signature ok");
        assert!(
            out.contains("[signature]"),
            "signature header missing: {out}"
        );
        assert!(
            out.lines().any(
                |l| crate::format::parse_anchor(l.split('|').next().unwrap_or("")).is_some()
                    && l.contains("fn signature_target")
            ),
            "hash-prefixed signature line missing: {out}"
        );
        assert!(
            !out.contains("body_marker"),
            "signature mode must not include function body: {out}"
        );
    }

    /// Auto mode uses the same hash-prefixed signature output for large code.
    #[test]
    fn tool_read_auto_large_code_emits_hash_prefixed_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("large.rs");
        let mut src = String::from("fn large_signature_target() {\n    let body_marker = 42;\n}\n");
        src.push_str(&"// padding padding padding padding\n".repeat(1000));
        std::fs::write(&p, src).unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto signature ok");
        assert!(
            out.contains("[signature]"),
            "signature header missing: {out}"
        );
        assert!(
            out.lines().any(|l| l.contains("large_signature_target")
                && crate::format::parse_anchor(l.split('|').next().unwrap_or("")).is_some()),
            "hash-prefixed signature line missing: {out}"
        );
        assert!(
            !out.contains("body_marker"),
            "auto large-code signature must not include body: {out}"
        );
    }

    /// Auto mode on small code returns the full body (header `[full]`),
    /// covering row 1 / column 1 of the spec heuristic table.
    #[test]
    fn tool_read_auto_small_code_returns_full_body() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("small.rs");
        std::fs::write(&p, "fn small_target() {\n    let body_marker = 1;\n}\n").unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto small-code ok");
        assert!(out.contains("[full]"), "expected `[full]` header: {out}");
        assert!(
            out.contains("body_marker"),
            "small code must include the body, not just signatures: {out}"
        );
    }

    /// Auto mode on a small markdown file returns the full body (`[full]`),
    /// covering row 2 / column 1 of the spec heuristic table.
    #[test]
    fn tool_read_auto_small_markdown_returns_full_body() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.md");
        std::fs::write(&p, "# Title\n\nBody paragraph that must appear verbatim.\n").unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto small-md ok");
        assert!(out.contains("[full]"), "expected `[full]` header: {out}");
        assert!(
            out.contains("Body paragraph that must appear verbatim"),
            "small markdown must include body: {out}"
        );
    }

    /// Auto mode on a large markdown file returns the heading-and-preview
    /// outline (`[outline]`), covering row 2 / column 2 of the heuristic.
    #[test]
    fn tool_read_auto_large_markdown_returns_outline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("large.md");
        let mut src = String::from("# Top\n\n## Headline Marker\n\nBody preview line one.\n");
        src.push_str(&"filler line repeated for size.\n".repeat(2_000));
        std::fs::write(&p, src).unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto large-md ok");
        assert!(
            out.contains("[outline]"),
            "expected `[outline]` header: {out}"
        );
        assert!(
            out.contains("Headline Marker"),
            "large markdown outline must surface headings: {out}"
        );
        assert!(
            !out.contains("filler line repeated"),
            "large markdown outline must not dump filler body: {out}"
        );
    }

    /// Auto mode on a large structured (JSON) file returns the keys outline
    /// (`[keys]`), covering the structured row of the heuristic.
    #[test]
    fn tool_read_auto_large_structured_returns_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.json");
        let mut src = String::from("{\n  \"top_level_marker\": {\n");
        for i in 0..2_000 {
            let _ = writeln!(
                src,
                "    \"padding_key_{i}\": \"value-value-value-value-value-{i}\","
            );
        }
        src.push_str("    \"trailing_key\": null\n  }\n}\n");
        std::fs::write(&p, src).unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto structured ok");
        assert!(out.contains("[keys]"), "expected `[keys]` header: {out}");
        assert!(
            out.contains("top_level_marker"),
            "structured outline must surface top-level keys: {out}"
        );
    }

    /// Auto mode on a plain text file falls back to the file_type-specific
    /// outline branch (`[outline]`) — no signature path applies because
    /// `should_auto_signature` only fires for code, covering the "other
    /// text" row of the heuristic.
    #[test]
    fn tool_read_auto_other_text_does_not_signature() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        let body: String = "plain prose line that is not code.\n".repeat(2_000);
        std::fs::write(&p, body).unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto other-text ok");
        assert!(
            !out.contains("[signature]"),
            "non-code file must never use signature mode: {out}"
        );
    }

    /// `tilth_search` queries[] enforces the 10-entry cap.
    #[test]
    fn tool_search_queries_over_limit_rejected() {
        let mut qs = Vec::with_capacity(11);
        for _ in 0..11 {
            qs.push(serde_json::json!({ "query": "foo" }));
        }
        let args = serde_json::json!({ "queries": qs });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let err = tool_search(&args, &cache, &session, &bloom, false).expect_err(">10 must error");
        assert!(err.contains("limited to 10"), "unexpected error: {err}");
    }

    /// `tilth_search` queries[] entry missing `query` field returns a clear
    /// error naming the offending index.
    #[test]
    fn tool_search_queries_missing_query_field_errors() {
        let args = serde_json::json!({ "queries": [{ "glob": "*.rs" }] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let err = tool_search(&args, &cache, &session, &bloom, false).expect_err("missing query");
        assert!(err.contains("queries[0]"), "must name index: {err}");
        assert!(err.contains("query"), "must mention 'query': {err}");
    }

    /// `tilth_list` enforces the 20-pattern cap.
    #[test]
    fn tool_list_patterns_over_limit_rejected() {
        let mut ps = Vec::with_capacity(21);
        for _ in 0..21 {
            ps.push(serde_json::json!("*.rs"));
        }
        let args = serde_json::json!({ "patterns": ps });
        let err = tool_list(&args).expect_err(">20 must error");
        assert!(err.contains("limited to 20"), "unexpected: {err}");
    }

    /// `tilth_list` empty patterns rejected.
    #[test]
    fn tool_list_empty_patterns_rejected() {
        let args = serde_json::json!({ "patterns": [] });
        let err = tool_list(&args).expect_err("empty must error");
        assert!(err.contains("at least one"), "unexpected: {err}");
    }

    /// Default search merges symbol, content, and identifier-shaped caller
    /// results when `kind` is omitted.
    #[test]
    fn tool_search_default_merges_symbol_content_and_callers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lib.rs");
        std::fs::write(
            &p,
            "fn target_fn() {\n    let _marker = \"content branch\";\n}\n\nfn caller() {\n    target_fn();\n}\n",
        )
        .unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "target_fn"}],
            "scope": dir.path().to_str().unwrap(),
            "expand": 0
        });
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("merged search ok");
        assert!(
            out.contains("symbol results"),
            "symbol facet missing: {out}"
        );
        assert!(
            out.contains("content results"),
            "content facet missing: {out}"
        );
        assert!(
            out.contains("caller results"),
            "caller facet missing: {out}"
        );
        assert!(
            out.contains("[caller: caller]"),
            "caller result missing: {out}"
        );
    }

    /// `tilth_search` honors `if_modified_since` by stubbing unchanged files
    /// without leaking expanded source bodies.
    #[test]
    fn tool_search_if_modified_since_redacts_unchanged_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lib.rs");
        std::fs::write(
            &p,
            "fn demo() {\n    let needle_unique = \"secret body text\";\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "queries": [{"query": "needle_unique", "kind": "content"}],
            "scope": dir.path().to_str().unwrap(),
            "expand": 1,
            "if_modified_since": "2099-01-01T00:00:00Z"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("search ok");
        assert!(
            out.contains("\"if_modified_since\""),
            "JSON cache-token header missing: {out}"
        );
        assert!(out.contains("unchanged"), "stub missing: {out}");
        assert!(
            !out.contains("secret body text"),
            "unchanged search body must be redacted: {out}"
        );
    }

    /// Spec criterion 4: in edit_mode, expanded search source lines carry
    /// `<line>:<hash>` prefixes (no leading gutter), ready to round-trip
    /// through `tilth_write` hash anchors.
    #[test]
    fn tool_search_expand_emits_hashlines_in_edit_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.rs");
        std::fs::write(
            &p,
            "fn unique_symbol_for_hashline_test() {\n    1 + 1;\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "queries": [{"query": "unique_symbol_for_hashline_test"}],
            "expand": 1,
            "scope": dir.path().to_str().unwrap(),
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let out = tool_search(&args, &cache, &session, &bloom, true).expect("edit-mode search ok");
        // Expected: a line of the form `1:xxx|fn unique_symbol_for_hashline_test() {`.
        let has_hash_anchor = out
            .lines()
            .any(|l| crate::format::parse_anchor(l.split('|').next().unwrap_or("")).is_some());
        assert!(
            has_hash_anchor,
            "expected <line>:<hash>| anchor in expanded source: {out}"
        );
        // The gutter form must NOT appear when edit_mode is set.
        assert!(
            !out.contains("│ fn unique_symbol_for_hashline_test"),
            "gutter form must be suppressed under edit_mode: {out}"
        );
    }

    /// Correctness: `tool_list` must respect `SKIP_DIRS` so `target/`,
    /// `node_modules/`, `.git/` don't blow the budget.
    #[test]
    fn tool_list_walker_respects_skip_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/keep.rs"), "fn k(){}").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/skip.rs"), "fn s(){}").unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules/skip.js"), "x").unwrap();
        let args = serde_json::json!({
            "patterns": ["**/*"],
            "scope": dir.path().to_str().unwrap()
        });
        let out = tool_list(&args).expect("list ok");
        assert!(
            out.contains("keep.rs"),
            "expected src/keep.rs in tree: {out}"
        );
        assert!(!out.contains("target/"), "target/ must be skipped: {out}");
        assert!(
            !out.contains("node_modules"),
            "node_modules must be skipped: {out}"
        );
    }

    /// Tightened tree-shape assertion: the rendered tree carries the box-
    /// drawing connectors and a per-directory token rollup, not just the
    /// substring `src/`.
    #[test]
    fn tool_list_emits_tree_shape_with_connectors_and_rollups() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}").unwrap();
        let args = serde_json::json!({
            "patterns": ["**/*.rs"],
            "scope": dir.path().to_str().unwrap()
        });
        let out = tool_list(&args).expect("list ok");
        assert!(
            out.contains("├── ") || out.contains("└── "),
            "expected box-drawing connector: {out}"
        );
        // Per-file token annotation
        assert!(out.contains("a.rs"), "expected a.rs entry: {out}");
        assert!(out.contains("tokens"), "expected token rollup: {out}");
        // Files count on directory line
        assert!(
            out.lines()
                .any(|l| l.contains("src/") && l.contains("files")),
            "expected src/ line with files rollup: {out}"
        );
    }

    /// Spec criterion 9 / per-file independence: a batch with one applying
    /// file and one mismatching file must emit a per-file auto-fix probe for
    /// the mismatcher while leaving the applied file untouched. (No more all-
    /// or-nothing auto-fix.)
    #[test]
    fn tool_write_per_file_auto_fix_on_partial_batch() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        let bad = dir.path().join("bad.txt");
        std::fs::write(&good, "alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(&bad, "prefix\nunique relocatable anchor\ntail\n").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [
                {
                    "path": good.to_str().unwrap(),
                    "mode": "hash",
                    "edits": [{
                        "start": anchor_for("alpha\nbeta\ngamma\n", 2),
                        "content": "BETA"
                    }]
                },
                {
                    "path": bad.to_str().unwrap(),
                    "mode": "hash",
                    "edits": [{
                        "start": wrong_anchor_for("prefix\nunique relocatable anchor\ntail\n", 2),
                        "content": "NEW"
                    }]
                }
            ]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("partial batch ok");
        // Good file applied.
        assert_eq!(
            std::fs::read_to_string(&good).unwrap(),
            "alpha\nBETA\ngamma\n",
            "good file edit must land"
        );
        // Bad file: spec criterion 9 — single-match relocation re-applies
        // the edit at the new location (here the same line, since the body
        // hasn't moved but the agent's hash was stale).
        assert_eq!(
            std::fs::read_to_string(&bad).unwrap(),
            "prefix\nNEW\ntail\n",
            "single-match relocation must re-apply the edit on the bad file"
        );
        // Per-file probe block present, with the verbatim auto-fixed line.
        assert!(
            out.contains("── auto-fix probe ──"),
            "per-file auto-fix probe must appear: {out}"
        );
        assert!(
            out.contains("auto-fixed: "),
            "verbatim auto-fixed signal must appear: {out}"
        );
    }

    /// Spec criterion 9: when the agent's anchor hash is stale but the
    /// captured body fingerprint resolves to exactly one location, tilth
    /// re-applies the edit at that location and emits the verbatim
    /// `auto-fixed: <old> → <new>` signal (with the resolved new line, which
    /// equals the old when the body still sits at the agent's claimed line).
    #[test]
    fn tool_write_auto_fix_applies_on_single_match() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("src.txt");
        std::fs::write(&p, "prefix\nunique_body_token\ntail\n").unwrap();
        let stale = wrong_anchor_for("prefix\nunique_body_token\ntail\n", 2);
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "hash",
                "edits": [{ "start": stale, "content": "REPLACED" }]
            }]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("auto-fix path ok");
        assert!(
            out.contains("auto-fixed: 2 → 2"),
            "verbatim auto-fixed line missing: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "prefix\nREPLACED\ntail\n",
            "edit must be re-applied at the resolved single-match line"
        );
    }

    /// Realistic agent-retry: the file has shifted so the anchor body lives
    /// at a new line, while the agent's claimed line now holds different
    /// content. `capture_hash_original` reads the body from the CURRENT
    /// file at the agent's claimed line, so the captured body is whatever
    /// has shifted INTO that slot — never the body the agent intended. The
    /// auto-fix can't recover the original body from a 12-bit hash alone,
    /// so this scenario does not produce `auto-fixed: <old> → <new>`. The
    /// response instead surfaces a fresh hashlined region so the agent can
    /// retry in one turn. This test documents the actual contract so a
    /// future design change (per-session file snapshot, body in the
    /// request, …) that adds genuine relocation flips a red flag.
    #[test]
    fn tool_write_auto_fix_shift_returns_fresh_region_not_relocation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shift.txt");

        use std::fmt::Write as _;

        // C0: TARGET_BODY_TOKEN at line 10.
        let mut c0 = String::new();
        for i in 1..=9 {
            let _ = writeln!(c0, "orig{i}");
        }
        c0.push_str("TARGET_BODY_TOKEN\n");
        c0.push_str("after\n");
        std::fs::write(&p, &c0).unwrap();

        // Anchor captured from C0 — line 10 hashes the target line.
        let anchor = anchor_for(&c0, 10);

        // C1: insert 5 blank lines above the target so it now lives at 15.
        let mut c1 = String::new();
        for i in 1..=9 {
            let _ = writeln!(c1, "orig{i}");
        }
        for _ in 0..5 {
            c1.push('\n');
        }
        c1.push_str("TARGET_BODY_TOKEN\n");
        c1.push_str("after\n");
        std::fs::write(&p, &c1).unwrap();

        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "hash",
                "edits": [{ "start": anchor, "content": "REPLACED" }]
            }]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("response renders");

        // Hash mismatch must surface — agent's hash is stale against C1.
        assert!(
            out.contains("hash mismatch"),
            "shifted file must trip the hash mismatch path: {out}"
        );
        // The auto-fix probe must run …
        assert!(
            out.contains("── auto-fix probe ──"),
            "probe block must run even though it can't recover the old body: {out}"
        );
        // … but a body-relocation auto-fix is impossible without the
        // original body, so the verbatim signal must NOT fire.
        assert!(
            !out.contains("auto-fixed: 10 → 15"),
            "auto-fix must not pretend to relocate when the captured body is post-shift: {out}"
        );
        // Instead a fresh hashlined region is returned for the agent to
        // retry in one turn (per the prompt's narrower claim).
        assert!(
            out.contains("fresh region"),
            "shifted-body retry must surface a fresh hashlined region: {out}"
        );
        // The file content is left untouched — the edit did NOT silently
        // land on the wrong line.
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            after, c1,
            "file must be unchanged when auto-fix cannot recover"
        );
    }

    /// Security: overwrite/append outside the configured scope is refused.
    #[test]
    fn tool_write_overwrite_outside_scope_refused() {
        let scope = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let target = outside_dir.path().join("escape.txt");
        let args = serde_json::json!({
            "scope": scope.path().to_str().unwrap(),
            "files": [{
                "path": target.to_str().unwrap(),
                "mode": "overwrite",
                "content": "escaped\n"
            }]
        });
        let (session, bloom) = edit_services();
        let out = tool_write(&args, &session, &bloom).expect("refusal is in-band");
        assert!(
            out.contains("outside scope"),
            "expected refusal marker: {out}"
        );
        assert!(
            !target.exists(),
            "file outside scope must not be written: {}",
            target.display()
        );
    }

    // ── F1 hardening: the JSON cache-token must stand alone on the first
    // line so a trivial JSON-line parse pulls the field. The prose-header
    // baseline was 0 / 2,042 round-trips; the integration regression here
    // is "response shape changed but the field is no longer parseable."
    #[test]
    fn tool_search_first_line_is_parseable_cache_token_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn handleAuth() {}\n").unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "handleAuth"}],
            "scope": dir.path().to_str().unwrap()
        });
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("search ok");
        let first = out.lines().next().expect("response has a first line");
        let parsed: serde_json::Value =
            serde_json::from_str(first).expect("first line must be valid one-line JSON");
        let ts = parsed
            .get("if_modified_since")
            .and_then(|v| v.as_str())
            .expect("if_modified_since field present");
        assert!(
            crate::mcp::iso::parse_iso_utc(ts).is_some(),
            "ts must round-trip through parse_iso_utc: {ts}"
        );
    }

    // ── F3 hardening: zero-match search emits the new empty header with the
    // three counts and the per-kind hint, end-to-end through tool_search.
    // The unit tests in src/format.rs cover the helper in isolation; this
    // proves the wiring from search.rs → format_search_result actually
    // routes through the empty path on real walker results.
    #[test]
    fn tool_search_zero_matches_emits_empty_header_with_kind_hint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("only.rs"),
            "fn unrelated() {}\n", // nothing here will match "zZxQyN_no_such_symbol"
        )
        .unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "zZxQyN_no_such_symbol", "kind": "content"}],
            "scope": dir.path().to_str().unwrap()
        });
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("search ok");
        assert!(out.contains("0 matches"), "empty header missing: {out}");
        assert!(
            out.contains("Files matched glob:"),
            "files matched count missing: {out}"
        );
        assert!(
            out.contains("Files searched:"),
            "files searched count missing: {out}"
        );
        assert!(out.contains("Content hits:"), "hits count missing: {out}");
        // kind=content ⇒ literal-content hint (split from regex per Copilot review).
        assert!(
            out.contains("no content matches"),
            "content-kind hint missing: {out}"
        );
    }

    // ── F3 hardening: glob that excludes every file emits the dedicated
    // glob-mismatch hint, regardless of the requested kind. This is the
    // dispatch-table row most likely to silently regress if a future
    // refactor stops populating files_matched_glob.
    #[test]
    fn tool_search_glob_excludes_everything_emits_glob_hint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn anything() {}\n").unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        // Glob matches nothing in the scope → files_matched_glob == 0.
        let args = serde_json::json!({
            "queries": [{"query": "anything", "kind": "symbol", "glob": "*.bogus_ext_does_not_exist"}],
            "scope": dir.path().to_str().unwrap()
        });
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("search ok");
        assert!(out.contains("0 matches"), "empty header missing: {out}");
        assert!(
            out.contains("Files matched glob: 0"),
            "glob-mismatch count must be zero: {out}"
        );
        assert!(
            out.contains("glob matched no files"),
            "glob-zero hint must override the kind hint: {out}"
        );
    }

    // ── F5 hardening: a request that still carries the dropped `context`
    // field must NOT error. Old agents have the parameter cached in their
    // tool spec; tolerating it silently is the documented contract (the
    // F5 verifier says "or is silently ignored — implementer's call").
    #[test]
    fn tool_search_tolerates_stray_context_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn handleAuth() {}\n").unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "handleAuth"}],
            "scope": dir.path().to_str().unwrap(),
            "context": "src/old.rs"
        });
        let out = tool_search(&args, &cache, &session, &bloom, false)
            .expect("stray context must not fail the request");
        assert!(
            out.contains("handleAuth"),
            "search must still find the symbol despite the stray field: {out}"
        );
    }

    // ── view-meta + mode=stripped tests ──────────────────────────────────
    //
    // The `_meta` channel rides on the first line as a single JSON object so
    // agents pattern-match one line for both the cache token and shape signals.
    // These tests cover the explicit + implicit emission cases plus the new
    // `mode=stripped` behavior (broad strip via `strip_noise`).

    /// Helper: parse the first line of a tool_read response as JSON when the
    /// header is present. Returns `None` when the response body has no JSON
    /// header (full content with no since/view-meta).
    fn parse_first_line_json(out: &str) -> Option<serde_json::Value> {
        let first = out.lines().next()?;
        serde_json::from_str(first).ok()
    }

    /// `mode=stripped` on a code file removes plain comments + debug logs
    /// while preserving doc comments and TODO/FIXME markers, and emits
    /// `view: "stripped"` in the meta header along with `lines_stripped`.
    #[test]
    fn tool_read_stripped_mode_drops_comments_and_keeps_doc_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("strip_target.rs");
        // `dbg!` is on the Rust debug-log strip list; `println!` is not
        // (intentional — `println!` is often legitimate CLI output, not noise).
        std::fs::write(
            &p,
            "/// Doc comment that survives.\nfn target() {\n    // plain comment that goes\n    // TODO: keep this one\n    let kept = 1;\n    dbg!(\"debug log dropped\");\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "mode": "stripped"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("stripped ok");

        let meta = parse_first_line_json(&out).expect("JSON view-meta header expected");
        assert_eq!(meta.get("view").and_then(|v| v.as_str()), Some("stripped"));
        // Explicit `mode=stripped` is a deliberate shape request — like
        // `mode=signature`, it MUST NOT advertise `next_view`. The agent
        // already picked this view.
        assert!(
            meta.get("next_view").is_none(),
            "explicit mode=stripped must not emit next_view: {out}"
        );
        let lines_stripped = meta
            .get("lines_stripped")
            .and_then(serde_json::Value::as_u64)
            .expect("lines_stripped must be present");
        assert!(
            lines_stripped >= 2,
            "expected at least 2 lines stripped (plain comment + dbg!), got {lines_stripped}: {out}"
        );

        assert!(out.contains("[stripped]"), "header view tag: {out}");
        assert!(
            out.contains("Doc comment that survives"),
            "doc comments must be kept: {out}"
        );
        assert!(out.contains("TODO: keep this one"), "TODOs kept: {out}");
        assert!(out.contains("let kept = 1"), "real code kept: {out}");
        assert!(
            !out.contains("plain comment that goes"),
            "plain comment must be stripped: {out}"
        );
        assert!(
            !out.contains("debug log dropped"),
            "debug log must be stripped: {out}"
        );
    }

    /// Stripped output uses original 1-indexed line numbers in a left gutter
    /// so the agent can see which line numbers were dropped (gaps) without
    /// having to diff against the file.
    #[test]
    fn tool_read_stripped_preserves_original_line_numbers_in_gutter() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gutter.rs");
        std::fs::write(
            &p,
            "fn alpha() {}\n// stripped line 2\nfn beta() {}\n// stripped line 4\nfn gamma() {}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "mode": "stripped"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("stripped ok");
        // Lines 1, 3, 5 survive; gutter shows the original numbers.
        assert!(
            out.contains("1  fn alpha()")
                && out.contains("3  fn beta()")
                && out.contains("5  fn gamma()"),
            "expected original line numbers in gutter: {out}"
        );
    }

    /// Hashlines must NOT appear in stripped output even when the server is
    /// in edit mode — the line set is non-contiguous with the file on disk
    /// and would mislead the agent into trying to anchor a write.
    #[test]
    fn tool_read_stripped_suppresses_hashlines_in_edit_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nohash.rs");
        std::fs::write(&p, "fn keep() {}\n// stripped\nfn also() {}\n").unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "mode": "stripped"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        // edit_mode = true intentionally — stripped MUST still suppress hashlines.
        let out = tool_read(&args, &cache, &session, true).expect("stripped+edit ok");
        // Hashline format is `<line>:<3-hex>|<content>`. Check the body has
        // no such anchored line for our `fn keep()` content.
        assert!(
            !out.lines().any(
                |l| crate::format::parse_anchor(l.split('|').next().unwrap_or("")).is_some()
                    && l.contains("fn keep()")
            ),
            "no hashline anchors in stripped output: {out}"
        );
        assert!(
            out.contains("non-editable view"),
            "non-editable note expected in inline header: {out}"
        );
    }

    /// `mode=stripped` + path suffix → suffix wins, raw range returned with no
    /// strip pass. Suffix-takes-priority is the consistent rule across modes.
    #[test]
    fn tool_read_stripped_with_suffix_returns_raw_range() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("suffix_wins.rs");
        std::fs::write(
            &p,
            "fn a() {}\n// this comment must NOT be stripped\nfn b() {}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "paths": [format!("{}#1-3", p.to_str().unwrap())],
            "mode": "stripped"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("stripped+suffix ok");
        assert!(
            out.contains("this comment must NOT be stripped"),
            "suffix wins; comments survive in raw slice: {out}"
        );
        assert!(
            !out.contains("[stripped]"),
            "suffix slice must use [section] header, not [stripped]: {out}"
        );
    }

    /// Unknown mode error must mention `stripped` so agents discover the new mode.
    #[test]
    fn tool_read_unknown_mode_error_lists_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.rs");
        std::fs::write(&p, "fn x() {}\n").unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "mode": "minified_maybe"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let err = tool_read(&args, &cache, &session, false).expect_err("unknown mode rejected");
        assert!(err.contains("stripped"), "error must list new mode: {err}");
    }

    /// Auto-signature on large code emits `view: "signature"` and the
    /// `next_view: "full"` escalation hint (implicit promotion).
    #[test]
    fn tool_read_auto_signature_emits_view_meta_with_next_view() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.rs");
        // Push the file over the auto-signature threshold (~24KB → >6000 tokens).
        let mut src = String::from("fn implicit_target() {}\n");
        src.push_str(&"// padding padding padding padding padding\n".repeat(2000));
        std::fs::write(&p, src).unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto sig ok");
        let meta = parse_first_line_json(&out).expect("view-meta JSON header expected");
        assert_eq!(meta.get("view").and_then(|v| v.as_str()), Some("signature"));
        assert_eq!(
            meta.get("next_view").and_then(|v| v.as_str()),
            Some("full"),
            "auto promotion advertises escalation: {out}"
        );
        assert!(
            meta.get("original_line_count")
                .and_then(serde_json::Value::as_u64)
                .is_some(),
            "original_line_count required for 'showing N of M' rendering: {out}"
        );
    }

    /// Explicit `mode=signature` emits `view: "signature"` but NOT
    /// `next_view` — the LLM picked this view on purpose.
    #[test]
    fn tool_read_explicit_signature_omits_next_view() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("small.rs");
        std::fs::write(&p, "fn small_target() {\n    let x = 1;\n}\n").unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "mode": "signature"
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("explicit sig ok");
        let meta = parse_first_line_json(&out).expect("view-meta JSON header expected");
        assert_eq!(meta.get("view").and_then(|v| v.as_str()), Some("signature"));
        assert!(
            meta.get("next_view").is_none(),
            "explicit signature must not nag with next_view: {out}"
        );
    }

    /// `mode=auto` on a small code file returns full content and emits NO
    /// view-meta JSON header (the LLM has everything; no signal needed).
    #[test]
    fn tool_read_auto_small_code_omits_view_meta_header() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tiny.rs");
        std::fs::write(&p, "fn tiny() {\n    let body = 1;\n}\n").unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto small ok");
        // First line must NOT be a JSON header — the file's `# path` markdown header should lead.
        let first = out.lines().next().expect("at least one line");
        assert!(
            !first.starts_with('{'),
            "small full reads must not emit a JSON header: {out}"
        );
    }

    /// Auto-outline on a large markdown emits `view: "outline"` + `next_view`.
    #[test]
    fn tool_read_auto_large_markdown_emits_outline_view_meta() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.md");
        let mut src = String::from("# Title\n\n## Section A\n\n");
        src.push_str(&"Lorem ipsum padding line.\n".repeat(2000));
        std::fs::write(&p, src).unwrap();
        let args = serde_json::json!({ "paths": [p.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("auto large md ok");
        let meta = parse_first_line_json(&out).expect("view-meta JSON header expected");
        assert_eq!(meta.get("view").and_then(|v| v.as_str()), Some("outline"));
        assert_eq!(meta.get("next_view").and_then(|v| v.as_str()), Some("full"));
    }

    /// Budget truncation surfaces `truncated`, `truncated_at_line`, and
    /// `original_line_count` in the view-meta header so the host can render
    /// a "showing 1–N of M lines" hint without re-reading the file.
    #[test]
    fn tool_read_budget_truncation_emits_meta_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("clip.rs");
        // Many small functions separated by blank lines — `apply()` prefers
        // `\n\n` boundaries when truncating, so we need internal blank lines
        // for it to find a non-zero cut point.
        let mut src = String::new();
        for i in 0..100 {
            src.push_str(&format!("fn f{i}() {{\n    let l = {i};\n}}\n\n"));
        }
        std::fs::write(&p, src).unwrap();
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "budget": 400
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("budget read ok");
        let meta = parse_first_line_json(&out).expect("view-meta JSON header expected");
        assert_eq!(
            meta.get("truncated").and_then(serde_json::Value::as_bool),
            Some(true),
            "budget cut must set truncated=true: {out}"
        );
        let at_line = meta
            .get("truncated_at_line")
            .and_then(serde_json::Value::as_u64)
            .expect("truncated_at_line missing");
        let total = meta
            .get("original_line_count")
            .and_then(serde_json::Value::as_u64)
            .expect("original_line_count missing");
        assert!(
            at_line >= 2 && at_line < total,
            "N inside (1, M): at_line={at_line}, M={total}: {out}"
        );
    }

    #[test]
    fn tool_read_budget_truncation_stays_under_requested_budget() {
        // Regression: `finalize_response` prepends a JSON view-meta header AFTER
        // budgeting the body. The body budget must subtract the header's tokens
        // so the rendered response (header + body) fits inside the user's ask.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.rs");
        let mut src = String::new();
        for i in 0..400 {
            src.push_str(&format!("fn f{i}() {{\n    let l = {i};\n}}\n\n"));
        }
        std::fs::write(&p, src).unwrap();
        let budget = 500u64;
        let args = serde_json::json!({
            "paths": [p.to_str().unwrap()],
            "budget": budget
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("budget read ok");
        let meta = parse_first_line_json(&out).expect("view-meta JSON header expected");
        assert_eq!(
            meta.get("truncated").and_then(serde_json::Value::as_bool),
            Some(true),
            "test setup expected truncation: {out}"
        );
        let response_tokens = crate::types::estimate_tokens(out.len() as u64);
        assert!(
            response_tokens <= budget,
            "rendered response must fit in requested budget {budget} (got {response_tokens} tokens, {} bytes)",
            out.len()
        );
    }
}
