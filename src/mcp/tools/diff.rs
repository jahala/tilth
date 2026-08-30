use serde_json::Value;

use crate::diff::DiffSource;

use super::anchor_path;

pub(in crate::mcp) fn tool_diff(args: &Value) -> Result<String, String> {
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
    let root = args
        .get("root")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new);

    // `root` selects WHERE the diff happens: git-based sources run in it and
    // relative file/patch args anchor under it. Falling back to the server's
    // frozen cwd on a bad root would silently diff the wrong checkout — the
    // worktree hazard `root` exists to remove — so it fails closed instead.
    if let Some(r) = root {
        if !r.is_absolute() {
            return Err(format!(
                "relative root \"{}\" cannot anchor the diff: set \"root\" to an absolute \
                 checkout directory (the server cannot see your shell's cwd).",
                r.display()
            ));
        }
        if !r.is_dir() {
            return Err(format!(
                "root \"{}\" is not a valid directory. Set \"root\" to an absolute checkout \
                 directory.",
                r.display()
            ));
        }
    }

    let mut diff_source = crate::diff::resolve_source(source, a, b, patch, log)?;
    match &mut diff_source {
        DiffSource::Files(fa, fb) => {
            *fa = anchor_path(fa, root, "a")?;
            *fb = anchor_path(fb, root, "b")?;
        }
        DiffSource::Patch(p) => {
            *p = anchor_path(p, root, "patch")?;
        }
        _ => {}
    }
    crate::diff::diff(&diff_source, root, scope, search, blast, expand, budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Run a git command in the given test repo.
    fn git(dir: &Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("failed to run git");
    }

    /// A git repo whose working tree carries one uncommitted change: an added
    /// function named `marker`. The marker makes diff output attributable to
    /// exactly one repo in tests that involve two checkouts.
    fn repo_with_uncommitted_fn(marker: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let p = dir.path();
        git(p, &["init"]);
        git(p, &["config", "user.email", "test@test.com"]);
        git(p, &["config", "user.name", "Test"]);
        std::fs::write(p.join("f.rs"), "fn base() {}\n").unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-m", "initial"]);
        std::fs::write(
            p.join("f.rs"),
            format!("fn base() {{}}\n\nfn {marker}() {{}}\n"),
        )
        .unwrap();
        dir
    }

    /// Call `tool_diff` with the process cwd pinned to `cwd` (the "server
    /// checkout"), serialized via the crate-wide `CWD_LOCK` because cwd is
    /// process-global state shared with the diff module's own tests.
    fn run_tool_diff_in(cwd: &Path, args: &serde_json::Value) -> Result<String, String> {
        let _lock = crate::diff::CWD_LOCK.lock().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(cwd).unwrap();
        let result = tool_diff(args);
        std::env::set_current_dir(&prev).unwrap();
        result
    }

    #[test]
    fn tool_diff_root_selects_caller_worktree_not_server_cwd() {
        // The server's process cwd is frozen at spawn (repo A here); a caller
        // working in a different checkout passes `root` (repo B). The diff must
        // come from B — silently diffing A instead returns a wrong delta (or a
        // false "No changes.") that reads as positive evidence about the
        // caller's worktree. This is the worktree hazard of issue #201.
        let repo_a = repo_with_uncommitted_fn("change_x_marker");
        let repo_b = repo_with_uncommitted_fn("change_y_marker");

        let args = serde_json::json!({
            "source": "uncommitted",
            "root": repo_b.path().to_str().unwrap(),
        });
        let result = run_tool_diff_in(repo_a.path(), &args).unwrap();
        assert!(
            result.contains("change_y_marker"),
            "diff must show repo B's (root) change, got:\n{result}"
        );
        assert!(
            !result.contains("change_x_marker"),
            "diff must not leak repo A's (server cwd) change, got:\n{result}"
        );
    }

    #[test]
    fn tool_diff_absent_root_diffs_server_cwd() {
        // Absent `root` must keep the default flow exactly as on main: the
        // diff runs in the server's process cwd. The require-root discipline
        // fires only on explicit args, never on omitted ones.
        let repo_a = repo_with_uncommitted_fn("change_x_marker");
        let args = serde_json::json!({ "source": "uncommitted" });
        let result = run_tool_diff_in(repo_a.path(), &args).unwrap();
        assert!(
            result.contains("change_x_marker"),
            "absent root must diff the server cwd as before, got:\n{result}"
        );
    }

    #[test]
    fn tool_diff_relative_file_pair_without_root_refused() {
        // A relative `a`/`b` without an absolute root used to resolve silently
        // against the server cwd — the wrong-checkout hazard. The standard
        // refusal (anchor_path) applies, naming the arg and the escape hatch.
        let args = serde_json::json!({ "a": "rel/a.rs", "b": "rel/b.rs" });
        let err = tool_diff(&args).unwrap_err();
        assert!(
            err.contains("relative a") && err.contains("root"),
            "refusal must name the arg and the root escape hatch: {err}"
        );
    }

    #[test]
    fn tool_diff_relative_root_refused() {
        // A relative root re-resolves against the server cwd — the exact
        // hazard `root` exists to remove — so it is refused, never silently
        // ignored or defaulted.
        let args = serde_json::json!({ "source": "uncommitted", "root": "some/checkout" });
        let err = tool_diff(&args).unwrap_err();
        assert!(
            err.contains("root") && err.contains("absolute"),
            "refusal must direct the caller to an absolute root: {err}"
        );
    }

    #[test]
    fn tool_diff_missing_root_dir_refused() {
        // Fail closed: running git inside a nonexistent root would surface as
        // a cryptic spawn error (or fall back to the wrong checkout). Refuse
        // up front with the one-step recovery instead.
        let args = serde_json::json!({
            "source": "uncommitted",
            "root": "/nonexistent/checkout/zzz",
        });
        let err = tool_diff(&args).unwrap_err();
        assert!(
            err.contains("/nonexistent/checkout/zzz") && err.contains("not a valid directory"),
            "refusal must name the bad root: {err}"
        );
    }
}
