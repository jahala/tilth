//! How the MCP server finds its project root, checked on the built binary.
//!
//! The server resolves its root once at startup, from `--scope` or the
//! directory it was launched in, and again when the client answers
//! roots/list. Each test launches the binary in a directory of its own, so
//! the launch directory is a child process's and never this process's. The
//! cases without any root are in `dropped_errors.rs`.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// What one server run printed: its responses.
struct Run {
    responses: Vec<Value>,
}

impl Run {
    /// The text of the tool result with `id`.
    #[expect(
        clippy::expect_used,
        reason = "by design: a test helper, where a panic is the test failure"
    )]
    fn text(&self, id: u64) -> &str {
        self.responses
            .iter()
            .find(|r| r["id"] == id)
            .and_then(|r| r["result"]["content"][0]["text"].as_str())
            .expect("a tool result with this id")
    }
}

/// Run `tilth --mcp <flags>` from `launch`, send it `messages`, and close its
/// stdin.
#[expect(
    clippy::expect_used,
    reason = "by design: a test helper, where a panic is the test failure"
)]
fn serve(launch: &Path, flags: &[&str], messages: &[Value]) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilth"))
        .arg("--mcp")
        .args(flags)
        .current_dir(launch)
        .env("TILTH_NO_OVERVIEW", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start tilth");
    let mut stdin = child.stdin.take().expect("stdin");
    for message in messages {
        writeln!(stdin, "{message}").expect("write a message");
    }
    drop(stdin);
    let out = child.wait_with_output().expect("run tilth");
    assert!(out.status.success(), "tilth exited with {:?}", out.status);
    Run {
        responses: String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON-RPC line"))
            .collect(),
    }
}

fn initialize(roots: bool) -> Value {
    let capabilities = if roots {
        json!({ "roots": {} })
    } else {
        json!({})
    };
    json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "capabilities": capabilities },
    })
}

/// The client's answer to the server's roots/list request.
fn roots_answer(root: &Path) -> Value {
    json!({
        "jsonrpc": "2.0", "id": "tilth_roots_1",
        "result": { "roots": [{ "uri": format!("file://{}", root.display()) }] },
    })
}

fn call(id: u64, tool: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
}

/// A directory holding one source file that defines `name`.
#[expect(
    clippy::unwrap_used,
    reason = "by design: a test helper, where a panic is the test failure"
)]
fn defining(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), format!("fn {name}() {{}}\n")).unwrap();
    dir
}

/// A server launched in one directory, whose client names another as its
/// root, answers for the client's root: a search with no scope, and a
/// relative write with no `root`.
#[test]
fn the_clients_root_replaces_the_launch_directory() {
    let launch = defining("in_launch_zq");
    let client = defining("in_client_zq");
    let run = serve(
        launch.path(),
        &["--edit"],
        &[
            initialize(true),
            roots_answer(client.path()),
            call(1, "tilth_search", &json!({ "query": "in_client_zq" })),
            call(2, "tilth_search", &json!({ "query": "in_launch_zq" })),
            call(
                3,
                "tilth_write",
                &json!({ "files": [{ "path": "new.txt", "mode": "overwrite", "content": "x\n" }] }),
            ),
        ],
    );
    assert!(run.text(1).contains("lib.rs:1"), "{}", run.text(1));
    assert!(run.text(2).contains("0 matches"), "{}", run.text(2));
    assert!(run.text(3).contains("created"), "{}", run.text(3));
    assert!(client.path().join("new.txt").exists());
    assert!(!launch.path().join("new.txt").exists());
}

/// `--scope` outranks the client's roots.
#[test]
fn an_explicit_scope_outranks_the_clients_root() {
    let launch = tempfile::tempdir().unwrap();
    let scope = defining("in_scope_zq");
    let client = defining("in_client_zq");
    let run = serve(
        launch.path(),
        &["--scope", scope.path().to_str().unwrap()],
        &[
            initialize(true),
            roots_answer(client.path()),
            call(1, "tilth_search", &json!({ "query": "in_scope_zq" })),
        ],
    );
    assert!(run.text(1).contains("lib.rs:1"), "{}", run.text(1));
}
