//! The CLI's stdout contract, checked on the built binary.
//!
//! A reader that goes away early (`tilth big.rs --full | head -1`) must end
//! the run quietly: exit 0 and nothing on stderr, as Unix tools do. Each test
//! hands the child a pipe whose read end is already closed, so its first
//! write fails with a broken pipe no matter how small the output is or how
//! fast the child runs.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn run_into_closed_pipe(args: &[&str], cwd: &Path) -> Output {
    let (reader, writer) = std::io::pipe().expect("create pipe");
    drop(reader);
    Command::new(env!("CARGO_BIN_EXE_tilth"))
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("run tilth")
}

fn assert_quiet_exit(out: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{what}: a closed stdout must exit 0, got {:?}; stderr: {stderr}",
        out.status
    );
    assert!(
        stderr.is_empty(),
        "{what}: a closed stdout must print nothing, got: {stderr}"
    );
}

/// A small project: a manifest for the overview, a few source files to
/// search, read and map, and a patch for the diff.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    let body: String = (0..400)
        .map(|i| format!("pub fn handler_{i}() -> usize {{ shared_helper() + {i} }}\n"))
        .collect();
    std::fs::write(
        root.join("src/lib.rs"),
        format!("pub fn shared_helper() -> usize {{ 1 }}\n{body}"),
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() { demo::shared_helper(); }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("change.patch"),
        "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-fn main() { demo::shared_helper(); }\n+fn main() { let _ = demo::shared_helper(); }\n",
    )
    .unwrap();
    dir
}

#[test]
fn read_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["src/lib.rs", "--full"], dir.path());
    assert_quiet_exit(&out, "read");
}

#[test]
fn search_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["shared_helper"], dir.path());
    assert_quiet_exit(&out, "search");
}

#[test]
fn json_search_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["shared_helper", "--json"], dir.path());
    assert_quiet_exit(&out, "json search");
}

#[test]
fn overview_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["overview"], dir.path());
    assert_quiet_exit(&out, "overview");
}

#[test]
fn map_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["--map"], dir.path());
    assert_quiet_exit(&out, "map");
}

#[test]
fn diff_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["diff", "--patch", "change.patch"], dir.path());
    assert_quiet_exit(&out, "diff");
}

#[test]
fn completions_into_a_closed_pipe_exits_quietly() {
    let dir = project();
    let out = run_into_closed_pipe(&["--completions", "bash"], dir.path());
    assert_quiet_exit(&out, "completions");
}
