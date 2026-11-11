//! Failures tilth used to drop in silence, checked on the built binary.
//!
//! Each case puts the binary where a working-directory call fails, and checks
//! that the failure is reported instead of being replaced by an empty path or
//! ignored. The cases rely on Unix permissions and on removing the directory a
//! process runs in, so they run on Unix only.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const TILTH: &str = env!("CARGO_BIN_EXE_tilth");

/// Run `tilth args` from a working directory that no longer exists. The shell
/// enters the directory, removes it, and then becomes tilth, so tilth starts
/// in a directory whose path cannot be read.
#[expect(
    clippy::expect_used,
    reason = "test helper: a panic here is the test failure"
)]
fn run_in_removed_dir(args: &[&str], stdin: &str) -> Output {
    let parent = tempfile::tempdir().expect("create temp dir");
    let gone = parent.path().join("gone");
    std::fs::create_dir(&gone).expect("create dir to remove");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(r#"cd "$1" && rmdir "$1" && shift && exec "$@""#)
        .arg("sh")
        .arg(&gone)
        .arg(TILTH)
        .args(args)
        .env_remove("TILTH_NO_OVERVIEW")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run tilth");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for tilth");
    assert!(!gone.exists(), "the shell must remove the directory first");
    out
}

/// A directory nobody may enter. Dropping it restores its permissions, so
/// the temp dir can be removed on every path, a failed test included.
struct LockedDir {
    path: PathBuf,
}

impl LockedDir {
    /// Lock `parent/locked`, or `None` when permissions do not bind this user
    /// (root reads any directory), so the caller cannot run and returns.
    #[expect(
        clippy::expect_used,
        clippy::print_stderr,
        reason = "test helper: a panic is the test failure, and a skip says so"
    )]
    fn new(parent: &Path) -> Option<Self> {
        let dir = Self {
            path: parent.join("locked"),
        };
        std::fs::create_dir(&dir.path).expect("create locked dir");
        // `dir` exists before the chmod, so its drop restores the mode even
        // if a later step panics.
        std::fs::set_permissions(&dir.path, std::fs::Permissions::from_mode(0o000))
            .expect("lock dir");
        if std::fs::read_dir(&dir.path).is_ok() {
            eprintln!("skipped: permissions do not bind this user, so no directory can be locked");
            return None;
        }
        Some(dir)
    }
}

impl Drop for LockedDir {
    fn drop(&mut self) {
        // A destructor has no one to report to; if this fails, the temp dir
        // stays behind and nothing else is affected.
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o755));
    }
}

#[expect(
    clippy::expect_used,
    reason = "test helper: a panic here is the test failure"
)]
fn mcp(args: &[&str], cwd: &Path, stdin: &str) -> Output {
    let mut child = Command::new(TILTH)
        .args(args)
        .current_dir(cwd)
        .env("TILTH_NO_OVERVIEW", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run tilth --mcp");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for tilth")
}

const INITIALIZE: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;

/// `tilth overview` used to fingerprint the empty path and blame the
/// project ("No project fingerprint could be generated.").
#[test]
fn overview_reports_an_unreadable_working_directory() {
    let out = run_in_removed_dir(&["overview"], "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("cannot read the current directory"),
        "the cause must be named, got: {stderr}"
    );
}

/// The MCP server used to start without a word when it could not read its
/// working directory, and then fingerprint the empty path at initialize.
#[test]
fn mcp_warns_about_an_unreadable_working_directory() {
    let out = run_in_removed_dir(&["--mcp"], &format!("{INITIALIZE}\n"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("cannot read the current directory") && stderr.contains("--scope"),
        "startup must say why paths will not resolve, got: {stderr}"
    );
    assert!(
        stderr.contains("no project overview"),
        "initialize must say why the overview is missing, got: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(r#""id":1"#),
        "the server must still answer"
    );
}

/// An explicit `--scope` the server cannot enter used to be ignored, so the
/// server answered for whatever directory it was launched in.
#[test]
fn mcp_refuses_a_scope_it_cannot_enter() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let Some(locked) = LockedDir::new(tmp.path()) else {
        return;
    };
    let scope = locked.path.to_str().expect("utf-8 temp path");
    let out = mcp(&["--mcp", "--scope", scope], tmp.path(), "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("cannot enter scope"),
        "the scope must be named as the cause, got: {stderr}"
    );
}

/// A root from the client that the server cannot enter used to be ignored
/// without a word, leaving searches on the launch directory.
#[test]
fn mcp_warns_about_a_client_root_it_cannot_enter() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let Some(locked) = LockedDir::new(tmp.path()) else {
        return;
    };
    let initialize =
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{"roots":{}}}}"#;
    let roots = format!(
        r#"{{"jsonrpc":"2.0","id":"tilth_roots_1","result":{{"roots":[{{"uri":"file://{}"}}]}}}}"#,
        locked.path.display()
    );
    let out = mcp(&["--mcp"], tmp.path(), &format!("{initialize}\n{roots}\n"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("cannot enter the client's root"),
        "the refused root must be reported, got: {stderr}"
    );
}
