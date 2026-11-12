use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::diag;
use crate::session::Session;
use crate::timeout::{spawn_with_timeout, SpawnFailure, ThreadTracker};
use tilth_core::cache::OutlineCache;
use tilth_core::index::bloom::BloomFilterCache;

mod tools;
pub(crate) mod tree;
pub(crate) mod write;

use tools::{
    tool_definitions, tool_deps, tool_diff, tool_grok, tool_list, tool_read, tool_search,
    tool_session, tool_write,
};

/// Shared dependencies passed through the request → dispatch pipeline.
#[derive(Clone)]
struct Services {
    cache: Arc<OutlineCache>,
    session: Arc<Session>,
    bloom: Arc<BloomFilterCache>,
    tracker: Arc<ThreadTracker>,
    config: Config,
    edit_mode: bool,
}

impl Services {
    fn new(config: Config, edit_mode: bool) -> Self {
        Self {
            cache: Arc::new(OutlineCache::new()),
            session: Arc::new(Session::new()),
            bloom: Arc::new(BloomFilterCache::new()),
            tracker: Arc::new(ThreadTracker::new()),
            config,
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

    fn config(&self) -> &Config {
        &self.config
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

/// MCP server over stdio, run with `config`. When `edit_mode` is true,
/// exposes `tilth_write` and switches `tilth_read` to hashline output format.
/// Read-only deployments (no `--edit`) omit `tilth_write` and its large
/// schema entirely, so they pay no edit-protocol context tax.
///
/// `scope` overrides the default search root. When provided, tilth chdir's to it
/// at startup so all tools, git commands, and searches use the correct project root.
/// This fixes MCP hosts that launch tilth with cwd=/ (e.g., Codex).
pub fn run(config: Config, edit_mode: bool, scope: Option<&Path>) -> io::Result<()> {
    let scope_is_explicit = scope.is_some();

    enter_project_root(scope)?;

    let services = Services::new(config, edit_mode);
    let stdin = io::stdin();
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: the MCP transport's one stdout writer, for JSON-RPC only"
    )]
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    // Track pending roots/list request (for MCP roots protocol)
    let mut pending_roots_id: Option<Value> = None;
    // Set once any tool has been dispatched — see `roots_handoff_allowed`.
    let mut tool_dispatched = false;

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
                if let Some(root_path) = extract_root_from_response(&msg) {
                    if roots_handoff_allowed(scope_is_explicit, tool_dispatched) {
                        if let Err(e) = std::env::set_current_dir(&root_path) {
                            diag::emit(format_args!(
                                "tilth: warning: cannot enter the client's root {}: {e}; \
                                 searches keep resolving against {}.",
                                root_path.display(),
                                search_dir()
                            ));
                        }
                    } else if !scope_is_explicit {
                        diag::emit(format_args!(
                            "tilth: warning: ignoring roots/list response that arrived after \
                             tools started; searches keep resolving against {}.",
                            search_dir()
                        ));
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

        tool_dispatched |= method == "tools/call";
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

/// Resolve the project root and chdir to it.
/// Priority: explicit `--scope` > MCP roots (handled later) >
/// `package_root(cwd)` > cwd.
/// A scope that cannot be entered is an error; a failure to find or enter
/// the project root is reported and the server starts where it was launched.
fn enter_project_root(scope: Option<&Path>) -> io::Result<()> {
    if let Some(s) = scope {
        // A file or a missing path fails here too, with its own cause.
        std::env::set_current_dir(s).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot enter scope {}: {e}", s.display()))
        })?;
    } else {
        match std::env::current_dir() {
            Ok(cwd) => {
                if let Some(root) = tilth_core::lang::package_root(&cwd) {
                    if let Err(e) = std::env::set_current_dir(root) {
                        diag::emit(format_args!(
                            "tilth: warning: cannot enter the project root {}: {e}; \
                             searches resolve against the launch directory.",
                            root.display()
                        ));
                    }
                }
            }
            Err(e) => diag::emit(format_args!(
                "tilth: warning: cannot read the current directory: {e}; \
                 relative paths will not resolve. Pass --scope or use absolute paths."
            )),
        }
    }
    Ok(())
}

/// The directory searches resolve against, for a warning that they stay
/// there. [`enter_project_root`] may have moved it from the launch directory.
fn search_dir() -> String {
    std::env::current_dir().map_or_else(
        |e| format!("a working directory that cannot be read ({e})"),
        |cwd| cwd.display().to_string(),
    )
}

/// Whether a roots/list response may still move the server's working
/// directory.
///
/// The handoff mutates process-global state that every in-flight tool reads,
/// and a tool that outlived its timeout keeps running after the main loop has
/// moved on — so it would resolve its relative paths against a directory that
/// changed underneath it. Honoring the handoff only before the first tool call
/// keeps the mutation inside initialization, where nothing else is running.
/// An explicit `--scope` outranks roots entirely.
fn roots_handoff_allowed(scope_is_explicit: bool, tool_dispatched: bool) -> bool {
    !scope_is_explicit && !tool_dispatched
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
    let edit_mode = services.edit_mode();
    match req.method.as_str() {
        "initialize" => {
            let overview = if services.config().overview {
                match std::env::current_dir() {
                    Ok(cwd) => crate::overview::fingerprint(&cwd),
                    Err(e) => {
                        diag::emit(format_args!(
                            "tilth: warning: no project overview: \
                             cannot read the current directory: {e}"
                        ));
                        String::new()
                    }
                }
            } else {
                String::new()
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

/// Execute a tool by name with the given arguments. Returns formatted output or error string.
/// No classifier involved — the caller specifies the tool explicitly.
fn dispatch_tool(tool: &str, args: &Value, services: &Services) -> Result<String, String> {
    // A call's `gitignore` argument overrides the server's setting for that
    // call alone.
    let mut config = *services.config();
    if let Some(value) = args.get("gitignore") {
        config.walk.respect_gitignore = value.as_bool().ok_or("gitignore must be a boolean")?;
    }
    let edit_mode = services.edit_mode();
    let walk = config.walk;
    match tool {
        "tilth_read" => tool_read(
            args,
            services.cache(),
            services.session(),
            edit_mode,
            &config,
        ),
        "tilth_search" => tool_search(
            args,
            services.cache(),
            services.session(),
            services.bloom(),
            walk,
        ),
        "tilth_list" => tool_list(args, walk),
        "tilth_deps" => tool_deps(args, services.bloom(), walk),
        "tilth_grok" => tool_grok(args, services.bloom(), services.session(), &config),
        "tilth_diff" => tool_diff(args, walk),
        "tilth_session" => tool_session(args, services.session()),
        "tilth_write" if edit_mode => tool_write(args, services.session(), services.bloom(), walk),
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
        run_tool_with_timeout(services, tool_name, args, services.config().request_timeout)
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
        Err(SpawnFailure::Timeout) => Err(format!(
            "tool timed out after {}s — the operation took too long. \
             Try: reduce scope, use section instead of full, or set \
             TILTH_TIMEOUT=<seconds> to increase the limit.",
            timeout.as_secs()
        )),
        Err(SpawnFailure::Panic) => Err("tool panicked during execution".into()),
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

    /// An explicit scope that is a file used to be skipped without a word, so
    /// the server answered for its launch directory. The failure path never
    /// changes directory, so this test leaves the process where it was.
    #[test]
    fn a_scope_that_is_not_a_directory_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("file.txt");
        std::fs::write(&file, "").unwrap();
        let err = enter_project_root(Some(&file)).unwrap_err();
        assert!(
            err.to_string().contains("cannot enter scope"),
            "the scope must be named as the cause, got: {err}"
        );
    }

    // -- extract_root_from_response -------------------------------------------

    #[test]
    fn roots_handoff_is_confined_to_initialization() {
        assert!(
            roots_handoff_allowed(false, false),
            "the handoff is the whole point before any tool runs"
        );
        assert!(
            !roots_handoff_allowed(false, true),
            "a chdir after tools start moves ground under an in-flight tool"
        );
        assert!(
            !roots_handoff_allowed(true, false),
            "an explicit --scope outranks roots"
        );
        assert!(!roots_handoff_allowed(true, true));
    }

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
        let root = tilth_core::lang::package_root(&subdir);
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
            1399,
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
        assert!(
            SERVER_INSTRUCTIONS.contains(
                "DO NOT pass a relative path or scope without also setting root (absolute)"
            ),
            "require-root path discipline must lead the file-I/O guidance"
        );
        assert!(
            SERVER_INSTRUCTIONS.contains("DO NOT pass a file as scope — scope is a directory"),
            "file-valued-scope steering (#195) must stay in the PATHS guidance"
        );
        // De-dup (R1) moved per-tool usage into the schemas. Lock that the
        // native-vs-tilth steering for weaker models stays verbatim, and that the
        // per-tool parameter manuals are gone from the always-on instructions field.
        assert!(SERVER_INSTRUCTIONS.contains("DO NOT use Grep, Read, or Glob."));
        assert!(SERVER_INSTRUCTIONS
            .contains("To check what changed, use tilth_diff instead of Bash(git diff/git log)."));
        assert!(SERVER_INSTRUCTIONS.contains("DO NOT use Bash(git diff) or Bash(git log --patch)."));
        assert!(
            !SERVER_INSTRUCTIONS.contains("expand (default 2)"),
            "per-tool parameter manuals belong in the tool schemas, not the instructions field"
        );
    }

    #[test]
    fn edit_mode_extra_byte_lock() {
        assert_eq!(
            EDIT_MODE_EXTRA.len(),
            314,
            "EDIT_MODE_EXTRA byte count drifted from refactor baseline"
        );
        assert!(
            EDIT_MODE_EXTRA.starts_with("\n\ntilth_write replaces the host Edit and Write tools."),
            "EDIT_MODE_EXTRA must keep its leading blank-line separator so format!(\"{{S}}{{E}}\") emits one blank line between sections"
        );
        assert!(EDIT_MODE_EXTRA
            .ends_with("DO NOT use the host Edit or Write tool. Use tilth_write for all writes."));
        assert!(
            !EDIT_MODE_EXTRA.contains("\n\n\n"),
            "EDIT_MODE_EXTRA must not introduce triple newlines"
        );
    }

    #[test]
    fn instructions_compose_with_single_blank_line_between_sections() {
        // Pre-refactor: format!("{S}{E}") relied on EDIT_MODE_EXTRA's leading
        // "\n\n" to produce one blank line between the base and edit sections.
        // This asserts the composition still has that shape.
        let combined = format!("{SERVER_INSTRUCTIONS}{EDIT_MODE_EXTRA}");
        assert!(combined.contains(
            "DO NOT re-read files already shown in expanded search results.\n\ntilth_write replaces"
        ));
    }

    // -- dispatch_tool: per-call gitignore ------------------------------------

    /// A call's `gitignore` argument decides for that call alone; without
    /// one, the server's setting decides.
    #[test]
    fn a_calls_gitignore_argument_wins_over_the_server_setting() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("ignored")).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(tmp.path().join("ignored/hidden.rs"), "fn needle() {}\n").unwrap();
        let scope = tmp.path().to_str().unwrap();
        let calls = [
            (
                "tilth_list",
                serde_json::json!({ "scope": scope, "patterns": ["*.rs"] }),
            ),
            (
                "tilth_search",
                serde_json::json!({ "scope": scope, "query": "needle" }),
            ),
        ];
        for (tool, args) in calls {
            let hidden_found = |server: bool, call: Option<bool>| {
                let mut config = Config::default();
                config.walk.respect_gitignore = server;
                let mut args = args.clone();
                if let Some(call) = call {
                    args["gitignore"] = call.into();
                }
                dispatch_tool(tool, &args, &Services::new(config, false))
                    .unwrap()
                    .contains("hidden.rs")
            };
            assert!(hidden_found(false, None), "{tool}");
            assert!(!hidden_found(true, None), "{tool}");
            assert!(hidden_found(true, Some(false)), "{tool}");
            assert!(!hidden_found(false, Some(true)), "{tool}");
        }
    }

    // -- dispatch_tool: removed tools ----------------------------------------

    /// The token-accounting tool was removed outright, with no alias: a call by
    /// its old name gets the generic unknown-tool error. Absence from
    /// tools/list is pinned by `tilth_savings_is_not_advertised`.
    /// `tilth_files` was folded into `tilth_list` in v0.10.0 and kept a
    /// dispatch alias for one release. The alias is gone; the old name gets the
    /// generic unknown-tool error.
    #[test]
    fn tilth_files_is_not_dispatched() {
        let args = serde_json::json!({ "patterns": ["*.rs"] });
        let services = Services::new(Config::default(), false);
        let err = dispatch_tool("tilth_files", &args, &services)
            .expect_err("tilth_files must not dispatch");
        assert!(
            err.contains("unknown tool"),
            "expected unknown-tool error: {err}"
        );
    }

    #[test]
    fn tilth_savings_is_not_dispatched() {
        let args = serde_json::json!({});
        for edit_mode in [false, true] {
            let services = Services::new(Config::default(), edit_mode);
            let err = dispatch_tool("tilth_savings", &args, &services)
                .expect_err("tilth_savings must not dispatch");
            assert!(
                err.contains("unknown tool"),
                "expected unknown-tool error (edit_mode={edit_mode}): {err}"
            );
        }
    }
}
