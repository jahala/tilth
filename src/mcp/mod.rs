use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::OutlineCache;
use crate::index::bloom::BloomFilterCache;
use crate::session::Session;
use crate::timeout::{self, spawn_with_timeout, SpawnFailure, ThreadTracker};

mod iso;
mod path_suffix;
mod tools;
mod tree;
mod write;

use tools::{
    tool_definitions, tool_deps, tool_diff, tool_grok, tool_list, tool_read, tool_search,
    tool_write,
};

/// Shared dependencies passed through the request → dispatch pipeline.
#[derive(Clone)]
struct Services {
    cache: Arc<OutlineCache>,
    session: Arc<Session>,
    bloom: Arc<BloomFilterCache>,
    tracker: Arc<ThreadTracker>,
    edit_mode: bool,
}

impl Services {
    fn new(edit_mode: bool) -> Self {
        Self {
            cache: Arc::new(OutlineCache::new()),
            session: Arc::new(Session::new()),
            bloom: Arc::new(BloomFilterCache::new()),
            tracker: Arc::new(ThreadTracker::new()),
            edit_mode,
        }
    }

    fn cache(&self) -> &OutlineCache {
        &self.cache
    }

    fn session(&self) -> &Session {
        &self.session
    }

    fn bloom(&self) -> &Arc<BloomFilterCache> {
        &self.bloom
    }

    fn tracker(&self) -> &Arc<ThreadTracker> {
        &self.tracker
    }

    fn edit_mode(&self) -> bool {
        self.edit_mode
    }
}

// Sent to the LLM via the MCP `instructions` field during initialization.
// The strings live in prompts/mcp-base.md and prompts/mcp-edit.md so they can
// be versioned and rendered as Markdown. AGENTS.md is regenerated from the
// same files via scripts/regen-agents-md.sh, keeping the human-facing copy in
// lockstep with what MCP hosts receive in the `instructions` field.
const SERVER_INSTRUCTIONS: &str = include_str!("../../prompts/mcp-base.md");
const EDIT_MODE_EXTRA: &str = include_str!("../../prompts/mcp-edit.md");

/// Compose the MCP `instructions` field: optional overview, the base prompt,
/// and (in edit mode) the edit-mode addendum, separated by single blank lines
/// with no trailing whitespace.
fn build_instructions(edit_mode: bool, overview: &str) -> String {
    let base = SERVER_INSTRUCTIONS.trim_end();
    let mut out = String::with_capacity(SERVER_INSTRUCTIONS.len() + EDIT_MODE_EXTRA.len() + 64);
    if !overview.is_empty() {
        out.push_str(overview);
        out.push_str("\n\n");
    }
    out.push_str(base);
    if edit_mode {
        // EDIT_MODE_EXTRA owns the separator: it opens with "\n\n" (locked by
        // edit_mode_extra_byte_lock), so appending it directly yields exactly
        // one blank line between sections. A manual "\n\n" here doubles it.
        out.push_str(EDIT_MODE_EXTRA.trim_end());
    }
    out
}

/// Change the process working directory, logging failures to stderr.
///
/// A swallowed chdir leaves the server searching the wrong root while every
/// later tool call still looks successful, so the operator needs a grep-able
/// line when the configured root is unusable.
fn chdir_or_log(path: &Path) {
    if let Err(e) = std::env::set_current_dir(path) {
        eprintln!(
            "tilth: failed to set working directory to {}: {e}",
            path.display()
        );
    }
}

/// The current working directory, logging to stderr and falling back to an
/// empty path when `current_dir` fails (rare, but previously swallowed silently).
fn current_dir_or_log() -> PathBuf {
    match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("tilth: failed to read current dir: {e}");
            PathBuf::new()
        }
    }
}

/// MCP server over stdio. When `edit_mode` is true, exposes `tilth_write` and
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
            chdir_or_log(s);
        }
    } else {
        let cwd = current_dir_or_log();
        if let Some(root) = crate::lang::package_root(&cwd) {
            chdir_or_log(root);
        }
    }

    let services = Services::new(edit_mode);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    // Track pending roots/list request (for MCP roots protocol)
    let mut pending_roots_id: Option<Value> = None;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("tilth: stdin read error, shutting down: {e}");
                return Err(e);
            }
        };
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
                        chdir_or_log(&root_path);
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
        // Drop authority (host) component: file://host/path → /path.
        // file:///abs already starts with '/' after stripping "file://".
        let raw_path = if raw_path.starts_with('/') {
            raw_path
        } else {
            // Authority-only forms (file://host with no '/' after the host)
            // have no path component — skip them rather than fall back to the
            // bare authority as a relative path.
            match raw_path.find('/') {
                Some(i) => &raw_path[i..],
                None => continue,
            }
        };
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
    let edit_mode = services.edit_mode();
    match req.method.as_str() {
        "initialize" => {
            let overview = if std::env::var("TILTH_NO_OVERVIEW").is_ok() {
                String::new()
            } else {
                let cwd = current_dir_or_log();
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

/// Execute a tool by name with the given arguments. Returns formatted output or error string.
/// No classifier involved — the caller specifies the tool explicitly.
fn dispatch_tool(tool: &str, args: &Value, services: &Services) -> Result<String, String> {
    let edit_mode = services.edit_mode();
    // Budget validation only applies to tools that honour the budget param.
    // tilth_list and tilth_write ignore budget; rejecting budget:0 for them
    // produces a confusing read-oriented error on non-read operations.
    let budget_aware = matches!(
        tool,
        "tilth_read" | "tilth_search" | "tilth_deps" | "tilth_diff" | "tilth_grok"
    );
    if budget_aware {
        if let Some(b) = args.get("budget") {
            if !matches!(b.as_u64(), Some(n) if n >= 1) {
                return Err(format!(
                    "budget must be a positive integer ≥ 1 (got {b}); omit it for the default {}",
                    crate::budget::DEFAULT_BUDGET
                ));
            }
        }
    }
    match tool {
        "tilth_read" => tool_read(args, services.cache(), services.session(), edit_mode),
        "tilth_search" => tool_search(
            args,
            services.cache(),
            services.session(),
            services.bloom(),
            edit_mode,
        ),
        "tilth_list" => tool_list(args),
        "tilth_deps" => tool_deps(args, services.bloom()),
        "tilth_grok" => tool_grok(args, services.bloom(), services.session()),
        "tilth_diff" => tool_diff(args),
        "tilth_write" if edit_mode => tool_write(args, services.session(), services.bloom()),
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

    let result = if services.tracker().is_at_cap() {
        Err(
            "server busy: too many prior operations still running after timeout. \
             Wait or set TILTH_TIMEOUT=<seconds> higher."
                .into(),
        )
    } else {
        run_tool_with_timeout(services, tool_name, args, timeout::request_timeout())
    };

    build_tool_response(req.id.clone(), result)
}

fn run_tool_with_timeout(
    services: &Services,
    tool_name: &str,
    args: &Value,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let services_worker = services.clone();
    let tool_name_owned = tool_name.to_string();
    let args_owned = args.clone();

    let outcome = spawn_with_timeout(services.tracker(), timeout, move || {
        dispatch_tool(&tool_name_owned, &args_owned, &services_worker)
    });

    match outcome {
        Ok(inner) => inner,
        Err(SpawnFailure::Timeout) => {
            eprintln!(
                "tilth: tool '{tool_name}' timed out after {}s",
                timeout.as_secs()
            );
            Err(format!(
                "tool timed out after {}s — the operation took too long. \
                 Try: reduce scope, use section instead of full, or set \
                 TILTH_TIMEOUT=<seconds> to increase the limit.",
                timeout.as_secs()
            ))
        }
        Err(SpawnFailure::Panic) => {
            eprintln!("tilth: tool '{tool_name}' panicked during execution");
            Err("tool panicked during execution".into())
        }
    }
}

fn build_tool_response(id: Option<Value>, result: Result<String, String>) -> JsonRpcResponse {
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
        id,
        result: Some(payload),
        error: None,
    }
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
    use std::fmt::Write as _;

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

    #[test]
    fn extract_root_authority_form_strips_host() {
        // file://host/path (RFC 8089 authority) must resolve to /path, not host/path.
        let tmp = tempfile::tempdir().unwrap();
        let uri = format!("file://localhost{}", tmp.path().display());
        let msg = serde_json::json!({
            "result": { "roots": [{ "uri": uri }] }
        });
        let path = extract_root_from_response(&msg);
        assert_eq!(path, Some(tmp.path().to_path_buf()));
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

    // -- prompt extraction byte locks ------------------------------------------
    //
    // These tests pin the MCP `instructions` strings to their pre-refactor byte
    // shapes so the prompts/*.md extraction is provably a no-op. They flag
    // future drift loudly: any prompt edit must update both the markdown source
    // and the assertions below.

    #[test]
    fn server_instructions_byte_lock() {
        assert_eq!(
            SERVER_INSTRUCTIONS.len(),
            6107,
            "SERVER_INSTRUCTIONS byte count drifted from baseline"
        );
        assert!(SERVER_INSTRUCTIONS
            .starts_with("tilth — code intelligence MCP server. Replaces grep, cat, find, ls"));
        assert!(SERVER_INSTRUCTIONS
            .ends_with("DO NOT re-read files already shown in expanded search results."));
        assert!(
            !SERVER_INSTRUCTIONS.contains("\n\n\n"),
            "SERVER_INSTRUCTIONS must not introduce triple newlines (likely a trailing-newline drift in prompts/mcp-base.md)"
        );
        assert!(SERVER_INSTRUCTIONS.contains("Comma-OR is for kind any/symbol/callers"));
        assert!(
            SERVER_INSTRUCTIONS
                .contains("DO NOT pass a relative path/scope without an absolute `root`"),
            "require-root path discipline must lead the file-I/O guidance"
        );
        assert!(SERVER_INSTRUCTIONS
            .contains("Re-expanding a previously shown definition returns [shown earlier]"));
        assert!(
            SERVER_INSTRUCTIONS.contains("tilth_grok: Everything structural about a symbol"),
            "tilth_grok description must remain in SERVER_INSTRUCTIONS"
        );
        // Regression guard for #58: the instructions must advertise the
        // MCP-qualified tool names so models stop calling bare `tilth_read`
        // (rejected as "No such tool available").
        assert!(
            SERVER_INSTRUCTIONS.contains("mcp__tilth__tilth_search")
                && SERVER_INSTRUCTIONS.contains("mcp__tilth__tilth_read"),
            "qualified-name guidance (mcp__tilth__<tool>) must remain in SERVER_INSTRUCTIONS"
        );
    }

    #[test]
    fn edit_mode_extra_byte_lock() {
        assert_eq!(
            EDIT_MODE_EXTRA.len(),
            2478,
            "EDIT_MODE_EXTRA byte count drifted from refactor baseline"
        );
        assert!(
            EDIT_MODE_EXTRA.starts_with("\n\ntilth_write: Batch edit files"),
            "EDIT_MODE_EXTRA must keep its leading blank-line separator so format!(\"{{S}}{{E}}\") emits one blank line between sections"
        );
        assert!(EDIT_MODE_EXTRA
            .ends_with("DO NOT use the host Edit or Write tool. Use tilth_write for all writes."));
        assert!(
            !EDIT_MODE_EXTRA.contains("\n\n\n"),
            "EDIT_MODE_EXTRA must not introduce triple newlines"
        );
        assert!(EDIT_MODE_EXTRA.contains("SWAP a.=b:"));
    }

    /// AGENTS.md is the human-facing copy of the embedded prompts, generated by
    /// scripts/regen-agents-md.sh. Reproduce the regen composition here and fail
    /// on drift so an edit to prompts/ without a regen (or vice versa) is caught.
    #[test]
    fn agents_md_matches_prompt_sources() {
        const AGENTS_MD: &str = include_str!("../../AGENTS.md");
        let expected = format!(
            "<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->\n{SERVER_INSTRUCTIONS}{EDIT_MODE_EXTRA}\n"
        );
        assert_eq!(
            AGENTS_MD, expected,
            "AGENTS.md is out of sync with prompts/ — run ./scripts/regen-agents-md.sh"
        );
    }

    #[test]
    fn instructions_compose_with_single_blank_line_between_sections() {
        // Pre-refactor: format!("{S}{E}") relied on EDIT_MODE_EXTRA's leading
        // "\n\n" to produce one blank line between the base and edit sections.
        // This asserts the composition still has that shape.
        let combined = format!("{SERVER_INSTRUCTIONS}{EDIT_MODE_EXTRA}");
        assert!(combined.contains(
            "DO NOT re-read files already shown in expanded search results.\n\ntilth_write: Batch edit files"
        ));
    }

    // -- tilth_read tool: batch reads, suffix grammar, view modes ----------
    // Restored from pre-merge 3801a4c (dropped by the #35 upstream merge).
    // These guard every behavior the batch-only read revert restored.

    /// Helper: parse the first line of a `tool_read` response as JSON when the
    /// header is present. Returns `None` when the response body has no JSON
    /// header (full content with no since/view-meta).
    fn parse_first_line_json(out: &str) -> Option<serde_json::Value> {
        let first = out.lines().next()?;
        serde_json::from_str(first).ok()
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

    /// Batch reads must surface every requested path: existing files inline,
    /// missing files in a trailing `── not found ──` section.
    #[test]
    fn batch_read_not_found_section_lists_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.py");
        std::fs::write(&real, "x = 1\ny = 2\n").unwrap();
        let missing = dir.path().join("test_name_function");

        let args = serde_json::json!({
            "paths": [real.to_str().unwrap(), missing.to_str().unwrap()],
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false)
            .expect("batch read must succeed with mixed valid/missing");

        assert!(
            out.contains("x = 1"),
            "valid file content must be included: {out}"
        );
        assert!(
            out.contains("── not found ──"),
            "not-found section must appear: {out}"
        );
        let nf_idx = out
            .find("── not found ──")
            .expect("not-found header present");
        let nf_section = &out[nf_idx..];
        assert!(
            nf_section.contains("test_name_function"),
            "missing path must be listed in not-found section: {out}"
        );
        assert!(
            !nf_section.contains("real.py"),
            "valid path must not be in not-found section: {out}"
        );
    }

    /// Spec: "Don't error the whole call." An all-missing batch must still
    /// return Ok with only the `── not found ──` section — no inline file blocks.
    #[test]
    fn batch_read_all_missing_returns_section_only() {
        let dir = tempfile::tempdir().unwrap();
        let m1 = dir.path().join("ghost_a");
        let m2 = dir.path().join("ghost_b");

        let args = serde_json::json!({
            "paths": [m1.to_str().unwrap(), m2.to_str().unwrap()],
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false)
            .expect("all-missing batch must succeed (Ok), not error the whole call");

        assert!(
            out.contains("── not found ──"),
            "not-found section must appear: {out}"
        );
        assert!(out.contains("ghost_a"), "first missing listed: {out}");
        assert!(out.contains("ghost_b"), "second missing listed: {out}");
    }

    /// Locks completeness (every missing path appears) and ordering (input
    /// order preserved), plus the structural invariant that valid file
    /// content comes before the not-found section.
    #[test]
    fn batch_read_missing_paths_listed_in_order_after_valid_content() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.py");
        std::fs::write(&real, "x = 1\n").unwrap();
        let m1 = dir.path().join("aaa_missing");
        let m2 = dir.path().join("zzz_missing");

        let args = serde_json::json!({
            "paths": [
                m1.to_str().unwrap(),
                real.to_str().unwrap(),
                m2.to_str().unwrap(),
            ],
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("mixed batch succeeds");

        let content_idx = out.find("x = 1").expect("real file content present");
        let nf_idx = out
            .find("── not found ──")
            .expect("not-found section present");
        assert!(
            content_idx < nf_idx,
            "valid content must appear before the not-found section: {out}"
        );

        let nf = &out[nf_idx..];
        let i1 = nf.find("aaa_missing").expect("first missing listed");
        let i2 = nf.find("zzz_missing").expect("second missing listed");
        assert!(
            i1 < i2,
            "missing paths must appear in input order, not sorted or reversed: {nf}"
        );
    }

    /// Boundary check: the not-found section is batch-specific. A single
    /// missing path keeps the prior Err behaviour, so callers that depend
    /// on the explicit error code path still see it.
    #[test]
    fn single_missing_path_does_not_use_not_found_section() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("ghost_solo");
        let args = serde_json::json!({ "paths": [missing.to_str().unwrap()] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let result = tool_read(&args, &cache, &session, false);
        assert!(
            result.is_err(),
            "single missing path must surface as Err, not as a not-found section"
        );
    }

    /// A `#symbol` suffix that doesn't resolve in an otherwise-readable file
    /// is the symbol-equivalent of a missing path: it must land in the
    /// `── not found ──` footer using the qualified `<path>#<symbol>` form,
    /// not as an inline error mixed into the content stream.
    #[test]
    fn batch_read_symbol_miss_listed_in_not_found_section() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("real.rs");
        let p2 = dir.path().join("other.rs");
        std::fs::write(&p1, "fn real_fn() {}\n").unwrap();
        std::fs::write(&p2, "fn other_fn() {}\n").unwrap();

        // Mix: file exists + symbol exists, file exists + symbol missing.
        let target_real = format!("{}#real_fn", p1.to_str().unwrap());
        let target_miss = format!("{}#ghost_symbol", p2.to_str().unwrap());

        let args = serde_json::json!({ "paths": [target_real, target_miss] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false)
            .expect("batch with symbol miss must succeed (Ok)");

        let nf_idx = out
            .find("── not found ──")
            .expect("not-found section must be present");
        let nf_section = &out[nf_idx..];

        // Found symbol's body must appear before the footer, not in it.
        let body_idx = out
            .find("fn real_fn")
            .expect("resolved symbol body present");
        assert!(
            body_idx < nf_idx,
            "resolved symbol body must precede the not-found section: {out}"
        );

        // Miss must use the qualified `path#symbol` form in the footer.
        let qualified = format!("{}#ghost_symbol", p2.display());
        assert!(
            nf_section.contains(&qualified),
            "missing symbol must appear as `<path>#<symbol>` in footer: {nf_section}"
        );

        // The old inline error string must no longer appear anywhere.
        assert!(
            !out.contains("error: symbol 'ghost_symbol' not found in outline"),
            "symbol miss must not surface as an inline error in the content stream: {out}"
        );
    }

    /// Precondition-failure boundary: a `#symbol` suffix on a non-code file
    /// (no tree-sitter grammar) must NOT be routed to `── not found ──` —
    /// that would misrepresent "wrong file type for symbol grammar" as
    /// "you typed the wrong symbol name." Falls through to the existing
    /// inline error path instead.
    #[test]
    fn batch_read_symbol_on_non_code_file_falls_through_to_inline_error() {
        let dir = tempfile::tempdir().unwrap();
        let code = dir.path().join("real.rs");
        let txt = dir.path().join("notes.txt");
        std::fs::write(&code, "fn real_fn() {}\n").unwrap();
        std::fs::write(&txt, "just some prose, no grammar\n").unwrap();

        let target_real = format!("{}#real_fn", code.to_str().unwrap());
        let target_precondition = format!("{}#anything", txt.to_str().unwrap());

        let args = serde_json::json!({ "paths": [target_real, target_precondition] });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false)
            .expect("batch with non-code symbol target must succeed (Ok)");

        // The non-code path must NOT appear in the not-found footer — if it
        // does, we're misclassifying "wrong file type" as "missing symbol".
        if let Some(nf_idx) = out.find("── not found ──") {
            let nf_section = &out[nf_idx..];
            assert!(
                !nf_section.contains(&format!("{}#anything", txt.display())),
                "non-code file symbol target must not appear in not-found footer: {nf_section}"
            );
        }
    }

    // -- batch tool_read --------------------------------------------------------

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

    /// `tilth_read` `path#n` (`FromLine`) suffix returns from line n to end.
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
            write!(src, "fn f{i}() {{\n    let l = {i};\n}}\n\n").unwrap();
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
            write!(src, "fn f{i}() {{\n    let l = {i};\n}}\n\n").unwrap();
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

    // -- tilth_write tool: whole-file-tag op-grammar dispatch --------------
    // PR2 swapped tilth_write to the op-grammar blob surface; the deep
    // behaviour (round-trip, drift recovery, confinement, file ops) is covered
    // in `mcp::tools::write::tests`. These lock the dispatch seam.

    fn edit_services() -> (Session, Arc<BloomFilterCache>) {
        (Session::new(), Arc::new(BloomFilterCache::new()))
    }

    #[test]
    fn tool_write_applies_op_grammar_blob_after_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let p = root.join("disp.rs");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let (session, bloom) = edit_services();
        let cache = OutlineCache::new();

        // Read in edit mode records the snapshot and emits the tag header.
        let read_out = tool_read(
            &serde_json::json!({"paths": [p.to_str().unwrap()], "mode": "full"}),
            &cache,
            &session,
            true,
        )
        .expect("edit-mode read");
        let tag =
            crate::edit::tag::format_tag(crate::edit::tag::compute_file_hash("one\ntwo\nthree\n"));
        assert!(
            read_out.contains(&format!("#{tag}]")),
            "read must emit [path#TAG]: {read_out}"
        );

        let blob = format!("[{}#{tag}]\nSWAP 2:\n+TWO\n", p.display());
        let out = tool_write(
            &serde_json::json!({"edits": blob, "root": root.to_str().unwrap()}),
            &session,
            &bloom,
        )
        .expect("write ok");
        assert!(out.contains("applied"), "expected applied: {out}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\nTWO\nthree\n");
    }

    #[test]
    fn tool_write_missing_edits_blob_is_top_level_error() {
        let (session, bloom) = edit_services();
        let err = tool_write(&serde_json::json!({}), &session, &bloom)
            .expect_err("missing edits → error");
        assert!(err.contains("edits"), "error must name the param: {err}");
    }

    // -- tilth_search / tilth_list / build_instructions: restored from pre-merge 3801a4c (dropped by #35)

    #[test]
    fn build_instructions_base_has_expected_anchors() {
        let s = build_instructions(false, "");
        // Adapted: pre-merge opening anchor was "tilth — AST-aware code
        // intelligence MCP server."; current prompts/mcp-base.md opens with
        // "tilth — code intelligence MCP server. Replaces grep, cat, find, ls".
        assert!(
            s.starts_with("tilth — code intelligence MCP server. Replaces grep, cat, find, ls"),
            "missing opening anchor: {:?}",
            &s[..60.min(s.len())]
        );
        assert!(
            s.contains("[+] added, [-] deleted, [~] body changed, [~:sig] signature changed"),
            "missing closing anchor"
        );
        // Adapted: pre-merge edit-mode marker was "tilth_write is exposed";
        // current EDIT_MODE_EXTRA opens with "tilth_write: Batch write".
        assert!(
            !s.contains("tilth_write: Batch write"),
            "edit-mode pointer leaked into base"
        );
    }

    #[test]
    fn build_instructions_edit_appends_thin_pointer() {
        let s = build_instructions(true, "");
        // Adapted: pre-merge asserted the marker "tilth_write is exposed";
        // current EDIT_MODE_EXTRA opens with "tilth_write: Batch write".
        assert!(
            s.contains("tilth_write: Batch edit files"),
            "expected tilth_write addendum in edit-mode instructions"
        );
        assert!(
            !s.contains("Legacy alias: tilth_edit"),
            "tilth_edit must not be advertised"
        );
        // The edit-mode prompt embeds the op grammar and the drift/tag model,
        // so the edit-mode build must contain them.
        assert!(
            s.contains("[path#TAG]"),
            "tag-header grammar missing from edit-mode prompt: {s}"
        );
        assert!(
            s.contains("SWAP a.=b:"),
            "op grammar missing from edit-mode prompt: {s}"
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

    #[test]
    fn build_instructions_edit_single_blank_line_and_byte_lock() {
        // Regression guard for the composed edit-mode string. A prior manual
        // "\n\n" was pushed on top of EDIT_MODE_EXTRA's own leading "\n\n",
        // producing a four-newline (double blank) junction that broke the
        // byte-identical invariant the revival claimed. The piece-wise locks
        // (edit_mode_extra_byte_lock, SERVER_INSTRUCTIONS checks) do not guard
        // the *composed* output, so lock it here.
        let edit = build_instructions(true, "");
        assert!(
            edit.contains(
                "DO NOT re-read files already shown in expanded search results.\n\ntilth_write: Batch edit files"
            ),
            "edit-mode section junction must be a single blank line"
        );
        assert!(
            !edit.contains("\n\n\n"),
            "edit-mode composition must not contain a triple newline (double blank line)"
        );
        assert_eq!(
            build_instructions(false, "").len(),
            6107,
            "non-edit composed instructions byte count drifted"
        );
        assert_eq!(
            edit.len(),
            8585,
            "edit-mode composed instructions byte count drifted (double-blank-line regression?)"
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

    /// `tilth_list` empty patterns rejected.
    #[test]
    fn tool_list_empty_patterns_rejected() {
        let cwd = std::env::current_dir().unwrap();
        let args = serde_json::json!({ "patterns": [], "scope": cwd.to_str().unwrap() });
        let err = tool_list(&args).expect_err("empty must error");
        assert!(err.contains("at least one"), "unexpected: {err}");
    }

    /// `tilth_list` enforces the 20-pattern cap.
    #[test]
    fn tool_list_patterns_over_limit_rejected() {
        let mut ps = Vec::with_capacity(21);
        for _ in 0..21 {
            ps.push(serde_json::json!("*.rs"));
        }
        let cwd = std::env::current_dir().unwrap();
        let args = serde_json::json!({ "patterns": ps, "scope": cwd.to_str().unwrap() });
        let err = tool_list(&args).expect_err(">20 must error");
        assert!(err.contains("limited to 20"), "unexpected: {err}");
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

    #[test]
    fn tool_list_ignores_ignore_files_except_tilthignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
        std::fs::write(dir.path().join(".ignore"), "hidden.rs\n").unwrap();
        std::fs::write(dir.path().join(".git/info/exclude"), "excluded.rs\n").unwrap();
        std::fs::write(dir.path().join("hidden.rs"), "fn hidden() {}\n").unwrap();
        std::fs::write(dir.path().join("excluded.rs"), "fn excluded() {}\n").unwrap();
        std::fs::write(dir.path().join("denied.rs"), "fn denied() {}\n").unwrap();
        std::fs::write(dir.path().join(".tilthignore"), "denied.rs\n").unwrap();
        let args = serde_json::json!({
            "patterns": ["*.rs"],
            "scope": dir.path().to_str().unwrap()
        });

        let out = tool_list(&args).expect("list ok");

        assert!(
            out.contains("hidden.rs"),
            ".ignore must not hide files: {out}"
        );
        assert!(
            out.contains("excluded.rs"),
            ".git/info/exclude must not hide files: {out}"
        );
        assert!(
            !out.contains("denied.rs"),
            ".tilthignore must deny files: {out}"
        );
    }

    /// `tilth_search` accepts `queries: [{query}]` and dispatches each.
    #[test]
    fn tool_search_queries_array_form() {
        let scope = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp");
        let args = serde_json::json!({
            "queries": [{ "query": "build_instructions" }],
            "expand": 0,
            "scope": scope.to_str().unwrap()
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

    /// Dispatch rejects a non-positive `budget` (0, negative, non-integer)
    /// across all tools instead of silently defaulting — a sub-1 budget used
    /// to collapse batch output to useless stubs.
    #[test]
    fn dispatch_rejects_non_positive_budget() {
        let services = Services::new(false);
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(0.5),
        ] {
            let args = serde_json::json!({ "queries": [{ "query": "foo" }], "budget": bad });
            let err = dispatch_tool("tilth_search", &args, &services)
                .expect_err("non-positive budget must be rejected");
            assert!(
                err.contains("positive integer"),
                "expected budget validation error for {bad}, got: {err}"
            );
        }
        // A valid budget still dispatches. Use CARGO_MANIFEST_DIR (compile-time-baked)
        // rather than current_dir() (process-global runtime state): parallel diff tests
        // call set_current_dir() and can race with this test, handing us a deleted
        // tmpdir path that fails canonicalize and then is_dir().
        let ok = serde_json::json!({
            "queries": [{ "query": "foo" }],
            "budget": 5000,
            "expand": 0,
            "scope": env!("CARGO_MANIFEST_DIR")
        });
        assert!(
            dispatch_tool("tilth_search", &ok, &services).is_ok(),
            "a valid budget must still dispatch"
        );
    }

    /// tilth_write and tilth_list don't consume budget; passing budget:0 must
    /// not produce a budget error — the error should come from their own
    /// parameter validation, not the budget gate.
    #[test]
    fn budget_validation_skipped_for_non_budget_tools() {
        // tilth_write in edit_mode=true, budget:0 → own files-array error, not budget error.
        let services = Services::new(true);
        let args = serde_json::json!({ "budget": 0, "files": [] });
        let err = dispatch_tool("tilth_write", &args, &services)
            .expect_err("empty files array must be rejected");
        assert!(
            !err.contains("positive integer"),
            "budget gate must not fire for tilth_write: {err}"
        );

        // tilth_list, budget:0 → own patterns error, not budget error.
        let services = Services::new(false);
        let args = serde_json::json!({ "budget": 0, "patterns": [] });
        let err = dispatch_tool("tilth_list", &args, &services)
            .expect_err("empty patterns must be rejected");
        assert!(
            !err.contains("positive integer"),
            "budget gate must not fire for tilth_list: {err}"
        );
    }

    /// Batch search splits the budget per query so every query is
    /// represented even under a tight budget — no silent trailing drops.
    #[test]
    fn batch_budget_represents_every_query() {
        let tmp = tempfile::tempdir().unwrap();
        let body: String = (0..400)
            .map(|i| format!("fn f_{i}() {{ let u_{i} = use_it({i}); }}\n"))
            .collect();
        std::fs::write(tmp.path().join("lib.rs"), body).unwrap();
        let args = serde_json::json!({
            "queries": [
                { "query": "fn", "kind": "content" },
                { "query": "let", "kind": "content" },
                { "query": "use", "kind": "content" }
            ],
            "scope": tmp.path().to_str().unwrap(),
            "budget": 300
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("batch search");
        for q in ["fn", "let", "use"] {
            assert!(
                out.contains(&format!("query: {q}")),
                "query '{q}' was silently dropped under a tight budget:\n{out}"
            );
        }
        assert!(
            out.contains("truncated"),
            "expected truncation marker:\n{out}"
        );
        assert!(
            out.contains("raise `budget`"),
            "truncation must name the budget lever:\n{out}"
        );
    }

    /// Multi-file read splits the budget per file so every file is
    /// represented even under a tight budget — no silent trailing drops.
    #[test]
    fn read_batch_budget_represents_every_file() {
        let tmp = tempfile::tempdir().unwrap();
        let names = ["a.rs", "b.rs", "c.rs"];
        let mut paths = Vec::new();
        for name in names {
            let p = tmp.path().join(name);
            let body: String = (0..400).map(|i| format!("let x_{i} = {i};\n")).collect();
            std::fs::write(&p, format!("fn main() {{\n{body}}}\n")).unwrap();
            paths.push(p.to_str().unwrap().to_string());
        }
        let args = serde_json::json!({
            "paths": paths,
            "mode": "full",
            "budget": 600
        });
        let cache = OutlineCache::new();
        let session = Session::new();
        let out = tool_read(&args, &cache, &session, false).expect("batch read");
        for name in names {
            assert!(
                out.contains(name),
                "file '{name}' was silently dropped under a tight budget:\n{out}"
            );
        }
        assert!(
            out.contains("truncated"),
            "expected truncation marker:\n{out}"
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

    /// A per-query `kind` overrides the top-level `kind`. Entry 1 overrides to
    /// `content` and queries a string literal whose enclosing fn name
    /// (`enclosing_alpha`) surfaces only when content search matches — a
    /// top-level `symbol` search would never match a string literal, so the
    /// name appearing proves the override took effect. Entry 2 omits `kind`
    /// and must inherit the top-level `symbol`, finding `other_beta`. Both
    /// discriminator names are queried by neither entry, so the header echo
    /// of the query strings cannot satisfy the assertions.
    #[test]
    fn tool_search_per_query_kind_overrides_top_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn enclosing_alpha() {\n    let _ = \"QUERYTOKEN_A\";\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other_beta() {}\n").unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [
                {"query": "QUERYTOKEN_A", "kind": "content"},
                {"query": "other_beta"}
            ],
            "kind": "symbol",
            "scope": dir.path().to_str().unwrap(),
            "expand": 1
        });
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("override search ok");
        assert!(
            out.contains("enclosing_alpha"),
            "entry 1 must override to kind=content — the string literal's \
             enclosing fn appears only via a content match, never via the \
             top-level symbol kind: {out}"
        );
        assert!(
            out.contains("other_beta"),
            "entry 2 must inherit top-level kind=symbol and find the fn def: {out}"
        );
    }

    /// Batch redaction: `if_modified_since` stubs the unchanged file's section
    /// while leaving the changed file's body intact across a multi-query batch.
    #[test]
    fn tool_search_batch_redacts_only_unchanged_query_sections() {
        use std::time::{Duration, UNIX_EPOCH};
        let dir = tempfile::tempdir().unwrap();
        let path_old = dir.path().join("old.rs");
        let path_new = dir.path().join("new.rs");
        // Query the fn name; assert on a body-only token (SECRET_*_BODY) so the
        // `## query:` header echo of the query string can't mask redaction.
        std::fs::write(
            &path_old,
            "fn old_target() {\n    let _ = \"SECRET_OLD_BODY\";\n}\n",
        )
        .unwrap();
        std::fs::write(
            &path_new,
            "fn new_target() {\n    let _ = \"SECRET_NEW_BODY\";\n}\n",
        )
        .unwrap();

        // since sits between the two files' mtimes: old < since < new.
        let since = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path_old)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_hours(250_000))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path_new)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_100_000_000))
            .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [
                {"query": "old_target", "kind": "content"},
                {"query": "new_target", "kind": "content"}
            ],
            "scope": dir.path().to_str().unwrap(),
            "expand": 1,
            "if_modified_since": crate::mcp::iso::iso_ts(since)
        });
        let out = tool_search(&args, &cache, &session, &bloom, false).expect("batch redaction ok");
        assert!(
            out.contains("unchanged"),
            "unchanged file must be stubbed: {out}"
        );
        assert!(
            !out.contains("SECRET_OLD_BODY"),
            "unchanged file body must be redacted: {out}"
        );
        assert!(
            out.contains("SECRET_NEW_BODY"),
            "changed file body must remain intact: {out}"
        );
    }

    /// Spec criterion 4: in `edit_mode`, expanded search source lines carry
    /// `<line>:<hash>` prefixes (no leading gutter), ready to round-trip
    /// through `tilth_write` hash anchors.
    #[test]
    fn tool_search_expand_emits_hashlines_in_edit_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hello.rs");
        // Consecutive blank lines (3, 4) exercise the blank-line collapse /
        // noise-stripping path. The marker on line 5 must keep its correct
        // absolute anchor number even though lines above it are collapsed.
        std::fs::write(
            &p,
            "fn unique_symbol_for_hashline_test() {\n    let a = 1;\n\n\n    let marker_xyz = a + 1;\n    marker_xyz\n}\n",
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
        // The marker is on source line 5; its anchor must read `5:` despite the
        // collapsed blank run above it. Proves stripping preserves absolute line
        // numbers (anchors stay valid for round-tripping into tilth_write).
        assert!(
            out.lines()
                .any(|l| l.starts_with("5:") && l.contains("marker_xyz")),
            "expected marker line to keep absolute anchor 5: {out}"
        );
        // The gutter form must NOT appear when edit_mode is set.
        assert!(
            !out.contains("│ fn unique_symbol_for_hashline_test"),
            "gutter form must be suppressed under edit_mode: {out}"
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
}
