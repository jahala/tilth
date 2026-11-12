//! Failures tilth used to drop in silence, checked on the built binary.
//!
//! Each case puts the binary where a working-directory call fails, and checks
//! that the failure is reported instead of being replaced by an empty path or
//! ignored. The cases rely on Unix permissions and on removing the directory a
//! process runs in, so they run on Unix only. A few cases check warnings and
//! the root next to these, where nothing fails.

#![expect(
    clippy::disallowed_methods,
    reason = "by design: tests read the files they check directly"
)]
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const TILTH: &str = env!("CARGO_BIN_EXE_tilth");

/// Run `tilth args` from `parent/gone`, a working directory that no longer
/// exists. The shell enters the directory, removes it, and then becomes
/// tilth, so tilth starts in a directory whose path cannot be read. A
/// relative path through `..` still reaches `parent`.
#[expect(
    clippy::expect_used,
    reason = "by design: a test helper, where a panic is the test failure"
)]
fn run_in_removed_dir(parent: &Path, args: &[&str], stdin: &str) -> Output {
    let gone = parent.join("gone");
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
        reason = "by design: a test helper, where a panic is the test failure and a skip says so"
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
    reason = "by design: a test helper, where a panic is the test failure"
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
    let tmp = tempfile::tempdir().expect("create temp dir");
    let out = run_in_removed_dir(tmp.path(), &["overview"], "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("cannot read the current directory"),
        "the cause must be named, got: {stderr}"
    );
}

/// The MCP server used to start without a word when it could not read its
/// working directory, and then fingerprint the empty path at initialize.
/// With no root there is no overview either, and one warning says why.
#[test]
fn mcp_warns_about_an_unreadable_working_directory() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let out = run_in_removed_dir(tmp.path(), &["--mcp"], &format!("{INITIALIZE}\n"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("cannot read the current directory") && stderr.contains("--scope"),
        "startup must say why paths will not resolve, got: {stderr}"
    );
    assert_eq!(
        stderr.matches("tilth: warning:").count(),
        1,
        "one missing root is one warning, got: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(r#""id":1"#),
        "the server must still answer"
    );
}

/// #307: the warning that relative paths will not resolve fired at startup
/// even when the client was about to send its root. It waits until no root
/// is coming, so a client that sends one gets no warning and its root works.
#[test]
fn mcp_no_root_warning_waits_for_the_clients_roots() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let client = tempfile::tempdir().expect("create temp dir");
    std::fs::write(client.path().join("lib.rs"), "fn in_client_zq() {}\n").expect("write lib.rs");
    let session = format!(
        "{INITIALIZE_WITH_ROOTS}\n{}\n{}",
        roots_response(client.path()),
        tool_call(
            2,
            "tilth_search",
            &serde_json::json!({ "query": "in_client_zq" })
        ),
    );
    let out = run_in_removed_dir(tmp.path(), &["--mcp"], &session);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("cannot read the current directory"),
        "the client's root arrived, so nothing is missing: {stderr}"
    );
    let text = tool_text(&out.stdout, 2);
    assert!(text.contains("lib.rs:1"), "{text}");
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

const INITIALIZE_WITH_ROOTS: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{"roots":{}}}}"#;

/// The client's answer to the server's roots/list request, naming `root`.
fn roots_response(root: &Path) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":"tilth_roots_1","result":{{"roots":[{{"uri":"file://{}"}}]}}}}"#,
        root.display()
    )
}

/// The directory an MCP server launched in `dir` searches: the package root
/// above it, if there is one.
#[expect(
    clippy::expect_used,
    reason = "by design: a test helper, where a panic is the test failure"
)]
fn search_dir(dir: &Path) -> PathBuf {
    let dir = dir.canonicalize().expect("resolve the launch directory");
    tilth_core::lang::package_root(&dir).map_or_else(|| dir.clone(), Path::to_path_buf)
}

/// A root from the client that the server cannot enter used to be ignored
/// without a word, leaving searches where they were. The warning names that
/// directory, which can be the package root rather than the launch directory,
/// and the next tool result says so too, since stderr often goes unread.
#[test]
fn mcp_warns_about_a_client_root_it_cannot_enter() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let Some(locked) = LockedDir::new(tmp.path()) else {
        return;
    };
    let list = |id| tool_call(id, "tilth_list", &serde_json::json!({ "patterns": ["*"] }));
    let session = format!(
        "{INITIALIZE_WITH_ROOTS}\n{}\n{}{}",
        roots_response(&locked.path),
        list(2),
        list(3),
    );
    let out = mcp(&["--mcp"], tmp.path(), &session);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    let kept = format!(
        "searches keep resolving against {}.",
        search_dir(tmp.path()).display()
    );
    assert!(
        stderr.contains("cannot enter the client's root"),
        "the refused root must be reported, got: {stderr}"
    );
    assert!(
        stderr.contains(&kept),
        "the directory searches use must be named, got: {stderr}"
    );
    let first = tool_text(&out.stdout, 2);
    assert!(
        first.starts_with("cannot enter the client's root") && first.contains(&kept),
        "the next tool result must say it first, got: {first}"
    );
    let second = tool_text(&out.stdout, 3);
    assert!(
        !second.contains("cannot enter the client's root"),
        "it is said once, got: {second}"
    );

    // With no readable working directory, there is no directory to name.
    let out = run_in_removed_dir(tmp.path(), &["--mcp"], &session);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot enter the client's root")
            && stderr.contains("the server has no project root."),
        "the missing root must be named, got: {stderr}"
    );
}

/// A roots/list answer that arrives after a tool ran used to be ignored,
/// because moving the process's working directory would have moved it under
/// a call still running. The root is a value each call copies now, so the
/// answer applies to every call after it.
#[test]
fn mcp_applies_a_roots_answer_that_arrives_after_a_tool_ran() {
    let launch = tempfile::tempdir().expect("create temp dir");
    let client = tempfile::tempdir().expect("create temp dir");
    std::fs::write(client.path().join("late.rs"), "fn late_root_zq() {}\n").expect("write late.rs");
    let search = |id| {
        tool_call(
            id,
            "tilth_search",
            &serde_json::json!({ "query": "late_root_zq" }),
        )
    };
    let session = format!(
        "{INITIALIZE_WITH_ROOTS}\n{}{}\n{}",
        search(2),
        roots_response(client.path()),
        search(3),
    );
    let out = mcp(&["--mcp"], launch.path(), &session);
    let before = tool_text(&out.stdout, 2);
    assert!(
        before.contains("0 matches"),
        "before the answer, the launch directory is searched: {before}"
    );
    let after = tool_text(&out.stdout, 3);
    assert!(
        after.contains("late.rs:1"),
        "after it, the client's root is: {after}"
    );
}

/// One `tools/call` request for `tool` with `arguments`, as a stdin line.
fn tool_call(id: u32, tool: &str, arguments: &serde_json::Value) -> String {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    });
    format!("{request}\n")
}

/// The text of the tool result with `id` in an MCP session's stdout.
#[expect(
    clippy::expect_used,
    reason = "by design: a test helper, where a panic is the test failure"
)]
fn tool_text(stdout: &[u8], id: u32) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|response| response["id"] == id)
        .and_then(|response| {
            response["result"]["content"][0]["text"]
                .as_str()
                .map(str::to_owned)
        })
        .expect("a tool result with this id")
}

/// The hash anchor `tilth_read` gives the line `text` of `file`.
#[expect(
    clippy::expect_used,
    reason = "by design: a test helper, where a panic is the test failure"
)]
fn hash_anchor(file: &Path, text: &str) -> String {
    let dir = file.parent().expect("the file has a parent");
    let read = tool_call(2, "tilth_read", &serde_json::json!({ "path": file }));
    let out = mcp(&["--mcp", "--edit"], dir, &format!("{INITIALIZE}\n{read}"));
    tool_text(&out.stdout, 2)
        .lines()
        .find_map(|line| line.strip_suffix(&format!("|{text}")))
        .expect("the hashline of the line")
        .to_owned()
}

/// A write with neither `scope` nor `root` is contained in the server's
/// root. When the launch directory could not be read, the refusal blamed the
/// stand-in path "." instead of naming the cause: there is no root.
#[test]
fn write_names_an_unreadable_working_directory_as_the_refusal_cause() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let target = tmp.path().join("new.txt");
    let write = tool_call(
        2,
        "tilth_write",
        &serde_json::json!({
            "files": [{ "path": target, "mode": "overwrite", "content": "x\n" }],
        }),
    );
    let out = run_in_removed_dir(
        tmp.path(),
        &["--mcp", "--edit"],
        &format!("{INITIALIZE}\n{write}"),
    );
    let text = tool_text(&out.stdout, 2);
    assert!(
        text.contains("refusing write") && text.contains("the server has no project root"),
        "the refusal must name the missing root, got: {text}"
    );
    assert!(!target.exists(), "the write must be refused");
}

/// A relative `scope` resolves against the server's root. When the launch
/// directory could not be read, the refusal blamed the scope ("No such file
/// or directory") instead of naming the cause: there is no root.
#[test]
fn write_names_an_unreadable_working_directory_for_a_relative_scope() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let target = tmp.path().join("new.txt");
    let write = tool_call(
        2,
        "tilth_write",
        &serde_json::json!({
            "scope": "..",
            "files": [{ "path": target, "mode": "overwrite", "content": "x\n" }],
        }),
    );
    let out = run_in_removed_dir(
        tmp.path(),
        &["--mcp", "--edit"],
        &format!("{INITIALIZE}\n{write}"),
    );
    let text = tool_text(&out.stdout, 2);
    assert!(
        text.contains("refusing write") && text.contains("the server has no project root"),
        "the refusal must name the missing root, got: {text}"
    );
    assert!(!target.exists(), "the write must be refused");
}

/// The blast radius of an edit to a file outside any package searched the
/// server's working directory. When that could not be read, it searched the
/// stand-in path "." and reported no callers without a word. It searches the
/// call's scope now, which needs no working directory (#307).
///
/// The file must sit outside any package, but a package manifest in any
/// directory above the temp dir puts it inside one, and no fixture path can
/// rule that out. So the test looks for one first, and skips with the path
/// it found.
#[test]
fn edit_blast_radius_outside_a_package_searches_the_calls_scope() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let file = tmp.path().join("lib.rs");
    std::fs::write(&file, "fn old() {}\n").expect("write the file to edit");
    std::fs::write(tmp.path().join("user.rs"), "fn user() {\n    old();\n}\n")
        .expect("write the caller");
    let canonical = file.canonicalize().expect("resolve the file to edit");
    if let Some(package) = tilth_core::lang::package_root(&canonical) {
        eprintln!(
            "skipped: {} holds a package manifest, so the file is not outside every package",
            package.display()
        );
        return;
    }
    let anchor = hash_anchor(&file, "fn old() {}");

    let edit = tool_call(
        2,
        "tilth_write",
        &serde_json::json!({
            "scope": tmp.path(),
            "files": [{
                "path": file,
                "edits": [{ "start": anchor, "content": "fn old(x: u32) {}" }],
            }],
        }),
    );
    let out = run_in_removed_dir(
        tmp.path(),
        &["--mcp", "--edit"],
        &format!("{INITIALIZE}\n{edit}"),
    );
    let text = tool_text(&out.stdout, 2);
    assert_eq!(
        std::fs::read_to_string(&file).expect("read the edited file"),
        "fn old(x: u32) {}\n",
        "the edit must apply, tool result: {text}"
    );
    assert!(
        text.contains("user.rs"),
        "the caller in the call's scope must be listed, tool result: {text}"
    );
}

/// A relative write without `root` lands under the server's root and is
/// checked there for a landing in another worktree. When the launch
/// directory could not be read, relative writes resolved against it anyway
/// and the check was skipped with a warning on stderr only. With no root at
/// all, a relative write has nowhere to land: it is refused in the tool
/// result, and the absolute writes of the batch go ahead.
#[test]
fn relative_writes_without_any_root_are_refused_in_the_result() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let files = tmp.path().join("files");
    std::fs::create_dir(&files).expect("create the files dir");
    std::fs::write(files.join("a.txt"), "old a\n").expect("write a.txt");
    std::fs::write(files.join("abs.txt"), "old abs\n").expect("write abs.txt");
    let anchor = hash_anchor(&files.join("a.txt"), "old a");
    let abs_anchor = hash_anchor(&files.join("abs.txt"), "old abs");
    let batch = tool_call(
        2,
        "tilth_write",
        &serde_json::json!({
            "scope": files,
            "files": [
                { "path": "../files/a.txt", "edits": [{ "start": anchor, "content": "new" }] },
                { "path": "../files/c.txt", "mode": "overwrite", "content": "new\n" },
                { "path": files.join("abs.txt"), "edits": [{ "start": abs_anchor, "content": "new" }] },
            ],
        }),
    );
    let out = run_in_removed_dir(
        tmp.path(),
        &["--mcp", "--edit"],
        &format!("{INITIALIZE}\n{batch}"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = tool_text(&out.stdout, 2);
    assert_eq!(
        text.matches("the server has no project root").count(),
        2,
        "each relative path is refused with the cause, tool result: {text}"
    );
    assert_eq!(
        std::fs::read_to_string(files.join("a.txt")).expect("read a.txt"),
        "old a\n",
        "a refused edit writes nothing, tool result: {text}"
    );
    assert!(!files.join("c.txt").exists(), "tool result: {text}");
    assert_eq!(
        std::fs::read_to_string(files.join("abs.txt")).expect("read abs.txt"),
        "new\n",
        "the absolute edit must apply, tool result: {text}"
    );
    assert!(
        !stderr.contains("cross-worktree"),
        "nothing goes unchecked, so stderr has nothing to add: {stderr}"
    );
}

/// The cross-worktree check counted any relative hash-edit path that existed
/// on disk as written, so an edit that failed on a stale anchor still warned
/// that it had landed in another worktree.
#[test]
fn a_failed_relative_edit_gets_no_cross_worktree_warning() {
    const CROSSED: &str = "cross-worktree write";
    let tmp = tempfile::tempdir().expect("create temp dir");
    // A worktree nested in the launch directory's, so the relative path stays
    // inside the write scope, which defaults to the launch directory.
    let here = tmp.path().join("here");
    let there = here.join("there");
    for worktree in [&here, &there] {
        std::fs::create_dir_all(worktree.join(".git")).expect("create a worktree");
    }
    std::fs::write(there.join("target.txt"), "old target\n").expect("write target.txt");
    std::fs::write(there.join("abs.txt"), "old abs\n").expect("write abs.txt");
    // A package manifest above the temp dir would move the server there, away
    // from the directory the relative path is written for.
    let canonical = here.canonicalize().expect("resolve the launch directory");
    if let Some(package) = tilth_core::lang::package_root(&canonical) {
        eprintln!(
            "skipped: {} holds a package manifest, so the server would not stay in the launch directory",
            package.display()
        );
        return;
    }
    let target_anchor = hash_anchor(&there.join("target.txt"), "old target");
    let abs_anchor = hash_anchor(&there.join("abs.txt"), "old abs");
    let relative = |anchor: &str| {
        serde_json::json!({
            "path": "there/target.txt",
            "edits": [{ "start": anchor, "content": "new" }],
        })
    };
    let write = |entries: Vec<serde_json::Value>| {
        let call = tool_call(2, "tilth_write", &serde_json::json!({ "files": entries }));
        let out = mcp(
            &["--mcp", "--edit"],
            &here,
            &format!("{INITIALIZE}\n{call}"),
        );
        tool_text(&out.stdout, 2)
    };

    // The absolute edit lands; the relative one fails on a stale anchor.
    let text = write(vec![
        relative(&abs_anchor),
        serde_json::json!({
            "path": there.join("abs.txt"),
            "edits": [{ "start": abs_anchor, "content": "new" }],
        }),
    ]);
    assert_eq!(
        std::fs::read_to_string(there.join("abs.txt")).expect("read abs.txt"),
        "new\n",
        "the absolute edit must apply, tool result: {text}"
    );
    assert!(
        text.contains("hash mismatch"),
        "the relative edit must fail on its anchor, tool result: {text}"
    );
    assert!(
        !text.contains(CROSSED),
        "a failed edit landed nowhere, tool result: {text}"
    );

    // The same relative edit with a fresh anchor lands, and is warned about.
    let text = write(vec![relative(&target_anchor)]);
    assert_eq!(
        std::fs::read_to_string(there.join("target.txt")).expect("read target.txt"),
        "new\n",
        "the edit must apply, tool result: {text}"
    );
    assert_eq!(
        text.matches(CROSSED).count(),
        1,
        "a landed edit in another worktree is warned about, tool result: {text}"
    );
}
