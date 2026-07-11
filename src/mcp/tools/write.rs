//! `tilth_write` — batch file writes in three per-file modes:
//!
//! * `hash` (default) — replace lines at hash anchors from `tilth_read`,
//!   delegated to [`crate::edit::apply_batch`].
//! * `overwrite` — whole-file write from `content`. Create-only by default
//!   (atomic `O_CREAT|O_EXCL`); pass `overwrite: true` to replace an existing
//!   file. See [`crate::mcp::write`] for the symlink guarantees.
//! * `append` — append `content`, creating the file if absent.
//!
//! Duplicate paths are rejected up front across **all** modes, so two entries
//! can never race a write against the same file.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;
use crate::session::Session;

/// Parse one `files[]` entry into a hash-mode edit task. Parse errors are
/// deferred onto the task so a malformed entry surfaces as a per-file failure
/// instead of aborting the whole batch.
pub(in crate::mcp) fn parse_file_edit(
    index: usize,
    val: &Value,
    root: Option<&Path>,
) -> crate::edit::FileEditTask {
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
            msg: "'edits' array is empty — omit this file or add at least one edit".into(),
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
        path: resolve_write_path(path_str, root),
        edits,
    }
}

/// Parse a single `edits[]` entry. Errors carry the edit index so the LLM
/// can fix exactly the right entry instead of guessing.
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

/// Strict boolean parse for the per-file `overwrite` flag. Absent → `false`;
/// a non-bool (e.g. the string `"true"`) → `None` so the caller can reject it
/// rather than silently coercing.
fn parse_overwrite_flag(f: &Value) -> Option<bool> {
    match f.get("overwrite") {
        None => Some(false),
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn render_text_diff(before: Option<&str>, after: &str) -> String {
    let mut out = String::from("── diff ──\n--- before\n+++ after\n");
    if let Some(before) = before {
        for line in before.lines() {
            let _ = writeln!(out, "- {line}");
        }
    }
    for line in after.lines() {
        let _ = writeln!(out, "+ {line}");
    }
    out
}

/// Returns true if `path` resolves under `scope` (canonical path containment).
/// For paths that don't yet exist, canonicalize the nearest existing ancestor
/// and append the remaining components.
fn path_within_scope(path: &Path, scope: &Path) -> bool {
    let Ok(scope_canon) = scope.canonicalize() else {
        return false;
    };
    let mut cursor: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        scope_canon.join(path)
    };
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let resolved = loop {
        if let Ok(p) = cursor.canonicalize() {
            break p;
        }
        match (cursor.file_name(), cursor.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                cursor = parent.to_path_buf();
            }
            _ => return false,
        }
    };
    let mut full = resolved;
    for component in tail.into_iter().rev() {
        full.push(component);
    }
    full.starts_with(&scope_canon)
}

/// Resolve a write path: a relative `path_str` is anchored under `root` when one
/// was supplied; absolute paths are used as-is regardless of `root`.
fn resolve_write_path(path_str: &str, root: Option<&Path>) -> PathBuf {
    let p = PathBuf::from(path_str);
    if p.is_absolute() {
        return p;
    }
    match root {
        Some(r) => r.join(&p),
        None => p,
    }
}

/// Walk up from `path` to the nearest directory containing a `.git` entry
/// (a directory for a normal clone, a file for a linked worktree's gitdir
/// pointer). Returns the containing directory, or `None` if none is found.
fn find_git_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.to_path_buf()
    };
    let mut dir = start.canonicalize().unwrap_or(start);
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        let p = dir.parent()?;
        dir = p.to_path_buf();
    }
}

/// Build a cross-worktree warning string. Called when a relative path write
/// (no `root` argument) resolves into a different git worktree than the
/// server's process cwd. Returns `None` when no warning is needed.
fn cross_worktree_warning(path: &Path, cwd: &Path) -> Option<String> {
    // Only warn for paths that actually resolve into a git repo (canonicalize
    // fails when the file was never written, which self-filters failed edits).
    let resolved = path.canonicalize().ok()?;
    let write_root = find_git_root(&resolved)?;
    let cwd_root = find_git_root(cwd)?;
    if write_root == cwd_root {
        return None;
    }
    Some(format!(
        "\n⚠️  cross-worktree write: resolved path is {} (git root: {}), \
         server cwd git root: {}. Pass `root` or use an absolute path to \
         make the target explicit.",
        resolved.display(),
        write_root.display(),
        cwd_root.display(),
    ))
}

/// Returns the resolved absolute path of `path` for success output, falling
/// back to `path.display()` if canonicalize fails. Callers must only invoke
/// this after a successful write so the file exists.
fn resolved_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

pub(in crate::mcp) fn tool_write(
    args: &Value,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let files_val = args
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("missing required parameter: files (array of {path, mode, ...})")?;

    if files_val.is_empty() {
        return Err("files array is empty".into());
    }
    if files_val.len() > 20 {
        return Err(format!(
            "batch write limited to 20 files (got {})",
            files_val.len()
        ));
    }

    // Up-front duplicate-path rejection across ALL modes. hash mode also
    // re-checks inside apply_batch as a defense-in-depth guarantee, but
    // overwrite/append are written inline and would otherwise escape that
    // check — two entries must never race a write against the same file.
    {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        for f in files_val {
            if let Some(path_str) = f.get("path").and_then(|v| v.as_str()) {
                if !seen.insert(crate::edit::normalize_path_key(Path::new(path_str))) {
                    return Err(format!(
                        "duplicate file path in batch: {path_str} — each path may appear at most once per call"
                    ));
                }
            }
        }
    }

    let show_diff = args
        .get("diff")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Optional per-call anchor root. When provided it must be absolute;
    // relative file paths in this call are anchored to it instead of the
    // server's process cwd.
    let root: Option<PathBuf> = match args.get("root").and_then(|v| v.as_str()) {
        Some(r) => {
            let p = PathBuf::from(r);
            if !p.is_absolute() {
                return Err(format!("'root' must be an absolute path (got: {r})"));
            }
            Some(p)
        }
        None => None,
    };

    // Containment root for the overwrite/append scope guard. This is the write
    // sandbox boundary, NOT a path-resolution channel: an explicit `scope`
    // anchors it. When `scope` is absent, default to `root` if one was
    // supplied — `root` names the caller's actual checkout, so it is the
    // correct containment boundary for a root-anchored write; falling back to
    // the server's process cwd here would refuse a legitimate root-only write
    // whenever the server was launched from a different directory than
    // `root` (the headline cross-worktree scenario `root` exists to solve).
    // Only when BOTH `scope` and `root` are absent does this fall back to the
    // server cwd, exactly as before.
    let scope_root: PathBuf = match args.get("scope").and_then(|v| v.as_str()) {
        Some(s) => PathBuf::from(s),
        None => root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into())),
    };
    // Canonicalize the scope once for the overwrite/append containment check.
    // Fail closed on canonicalize failure: an unresolvable scope refuses
    // writes rather than silently disabling the guard.
    let scope_canon: Result<PathBuf, std::io::Error> = scope_root.canonicalize();

    let mut hash_tasks: Vec<crate::edit::FileEditTask> = Vec::new();
    // Resolved paths for hash-mode files that were relative with no `root`, so
    // the cross-worktree warning can fire after apply_batch.
    let mut hash_relative_paths: Vec<PathBuf> = Vec::new();
    let mut direct_results: Vec<String> = Vec::new();
    let mut direct_applied: Vec<PathBuf> = Vec::new();

    for (i, f) in files_val.iter().enumerate() {
        let mode = f.get("mode").and_then(|v| v.as_str()).unwrap_or("hash");
        let Some(path_str) = f.get("path").and_then(|v| v.as_str()) else {
            direct_results.push(format!("## files[{i}]\nerror: missing 'path'"));
            continue;
        };
        // A relative path is anchored under `root` when supplied; absolute
        // paths are used as-is. With no `root`, a relative path falls back to
        // the server cwd but a cross-worktree warning fires below if that
        // resolves into a different git worktree than the server.
        let path = resolve_write_path(path_str, root.as_deref());

        // Scope guard for ALL modes. hash mode delegates to apply_batch, which
        // computes a package_root only for blast-radius reporting — it does NOT
        // enforce write containment, so a `../../etc/passwd` hash entry would
        // otherwise traverse out of scope exactly like a direct write. Run the
        // canonical-containment check uniformly before dispatching any mode.
        match scope_canon.as_ref() {
            Ok(root) => {
                if !path_within_scope(&path, root) {
                    direct_results.push(format!(
                        "## {}\nerror: refusing write outside scope ({})",
                        path.display(),
                        root.display()
                    ));
                    continue;
                }
            }
            Err(e) => {
                direct_results.push(format!(
                    "## {}\nerror: scope unresolvable ({e}); refusing write",
                    path.display(),
                ));
                continue;
            }
        }

        match mode {
            "hash" | "h" => {
                if root.is_none() && !PathBuf::from(path_str).is_absolute() {
                    hash_relative_paths.push(path.clone());
                }
                hash_tasks.push(parse_file_edit(i, f, root.as_deref()));
            }
            "overwrite" | "w" => {
                let Some(content) = f.get("content").and_then(|v| v.as_str()) else {
                    direct_results.push(format!(
                        "## {}\nerror: 'content' must be a string",
                        path.display()
                    ));
                    continue;
                };
                let Some(overwrite) = parse_overwrite_flag(f) else {
                    direct_results.push(format!(
                        "## {}\nerror: 'overwrite' must be a boolean",
                        path.display()
                    ));
                    continue;
                };
                let pre_existed = path.try_exists().unwrap_or(false);
                let before = (show_diff && pre_existed)
                    .then(|| std::fs::read_to_string(&path).ok())
                    .flatten();
                match crate::mcp::write::write_overwrite(&path, content, overwrite) {
                    Ok(()) => {
                        let line_count = content.lines().count();
                        let verb = if pre_existed { "overwrote" } else { "created" };
                        let mut block = format!(
                            "## {}\n{verb}: {} bytes, {line_count} lines\n{}",
                            resolved_display(&path),
                            content.len(),
                            crate::format::hashlines(content, 1),
                        );
                        if show_diff {
                            block.push_str(&render_text_diff(before.as_deref(), content));
                        }
                        // Warn when a relative path (no `root`) crosses a worktree boundary.
                        if root.is_none() && !PathBuf::from(path_str).is_absolute() {
                            let cwd =
                                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                            if let Some(warn) = cross_worktree_warning(&path, &cwd) {
                                block.push_str(&warn);
                            }
                        }
                        direct_results.push(block);
                        direct_applied.push(path);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        direct_results.push(format!(
                            "## {}\nerror: {}",
                            path.display(),
                            crate::error::TilthError::AlreadyExists { path: path.clone() }
                        ));
                    }
                    Err(e) => direct_results.push(format!("## {}\nerror: {e}", path.display())),
                }
            }
            "append" | "a" => {
                let Some(content) = f.get("content").and_then(|v| v.as_str()) else {
                    direct_results.push(format!(
                        "## {}\nerror: 'content' must be a string",
                        path.display()
                    ));
                    continue;
                };
                // Snapshot the file BEFORE appending. The echo is computed from
                // `before + content` (both known here), never from a post-write
                // re-read: re-reading would race a concurrent appender, whose
                // extra lines inflate the count and mis-number the echoed hash
                // anchors — the agent then hash-edits against wrong line numbers.
                let before = std::fs::read_to_string(&path).ok();
                match crate::mcp::write::write_append(&path, content) {
                    Ok(()) => {
                        let before_str = before.as_deref().unwrap_or("");
                        let after = format!("{before_str}{content}");
                        // No trailing newline on the prior content means the first
                        // appended line merges onto the last existing line, so the
                        // echoed region starts one line earlier.
                        let merged = !before_str.is_empty() && !before_str.ends_with('\n');
                        let pre_count = before_str.lines().count();
                        let after_lines: Vec<&str> = after.lines().collect();
                        let total = after_lines.len();
                        let start_idx = if merged {
                            pre_count.saturating_sub(1)
                        } else {
                            pre_count
                        }
                        .min(total);
                        let tail = after_lines[start_idx..].join("\n");
                        let start_line = (start_idx + 1) as u32;
                        // Echo only the appended region's hashlines so log-shaped
                        // append targets don't balloon the response.
                        let mut block = format!(
                            "## {}\nappend: {} bytes (echoing last {} of {total} lines)\n{}",
                            resolved_display(&path),
                            content.len(),
                            total - start_idx,
                            crate::format::hashlines(&tail, start_line),
                        );
                        if show_diff {
                            block.push_str(&render_text_diff(before.as_deref(), &after));
                        }
                        // Warn when a relative path (no `root`) crosses a worktree boundary.
                        if root.is_none() && !PathBuf::from(path_str).is_absolute() {
                            let cwd =
                                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                            if let Some(warn) = cross_worktree_warning(&path, &cwd) {
                                block.push_str(&warn);
                            }
                        }
                        direct_results.push(block);
                        direct_applied.push(path);
                    }
                    Err(e) => direct_results.push(format!("## {}\nerror: {e}", path.display())),
                }
            }
            other => direct_results.push(format!(
                "## {}\nerror: unknown mode '{other}' (use hash, overwrite, append)",
                path.display()
            )),
        }
    }

    let mut output = String::new();
    if !hash_tasks.is_empty() {
        // Record reads up front: record_read counts attempts, not just
        // committed edits.
        for task in &hash_tasks {
            if let crate::edit::FileEditTask::Ready { path, .. } = task {
                session.record_read(path);
            }
        }
        match crate::edit::apply_batch(hash_tasks, bloom, show_diff) {
            Ok(combined) => {
                output.push_str(&combined);
                // Cross-worktree warning for hash-mode relative paths. apply_batch
                // returns only the combined text (not the applied path list), so
                // warn over the relative paths we tracked. cross_worktree_warning
                // self-filters paths whose file was never written (canonicalize
                // fails). Pre-canonicalize into a HashSet to de-duplicate.
                if !hash_relative_paths.is_empty() {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    let canon_relative: std::collections::HashSet<PathBuf> = hash_relative_paths
                        .iter()
                        .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()))
                        .collect();
                    for p in &canon_relative {
                        if let Some(warn) = cross_worktree_warning(p, &cwd) {
                            output.push_str(&warn);
                        }
                    }
                }
            }
            // apply_batch returns Err only when every hash file failed. Hard-
            // fail only if there were no direct writes either; otherwise fold
            // the failure text in so successful direct writes aren't discarded.
            Err(msg) => {
                if direct_results.is_empty() {
                    return Err(msg);
                }
                output.push_str(&msg);
            }
        }
    }
    if !direct_results.is_empty() {
        if !output.is_empty() {
            output.push_str("\n\n---\n\n");
        }
        output.push_str(&direct_results.join("\n\n---\n\n"));
        for p in &direct_applied {
            session.record_read(p);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_bloom() -> Arc<BloomFilterCache> {
        Arc::new(BloomFilterCache::new())
    }

    #[test]
    fn parse_file_edit_rejects_empty_edits_array() {
        let val = serde_json::json!({ "path": "noop.txt", "edits": [] });
        match parse_file_edit(0, &val, None) {
            crate::edit::FileEditTask::ParseError { label, msg } => {
                assert_eq!(label, "noop.txt");
                assert!(msg.contains("empty"), "unexpected msg: {msg}");
            }
            crate::edit::FileEditTask::Ready { .. } => {
                panic!("empty edits array should produce a ParseError, not Ready");
            }
        }
    }

    #[test]
    fn parse_overwrite_flag_strict_bool() {
        assert_eq!(parse_overwrite_flag(&serde_json::json!({})), Some(false));
        assert_eq!(
            parse_overwrite_flag(&serde_json::json!({"overwrite": true})),
            Some(true)
        );
        // String "true" is NOT a bool — reject rather than coerce.
        assert_eq!(
            parse_overwrite_flag(&serde_json::json!({"overwrite": "true"})),
            None
        );
    }

    #[test]
    fn overwrite_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.rs");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "fn main(){}\n"}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(out.contains("created"), "expected 'created' verb: {out}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fn main(){}\n");
    }

    #[test]
    fn overwrite_existing_without_flag_errors_and_preserves() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.rs");
        std::fs::write(&p, "original").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "new"}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(
            out.contains("already exists") && out.contains("overwrite: true"),
            "expected create-only guard message: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "original",
            "create-only must not clobber"
        );
    }

    #[test]
    fn overwrite_existing_with_flag_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.rs");
        std::fs::write(&p, "original").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "replaced", "overwrite": true}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(
            out.contains("overwrote"),
            "expected 'overwrote' verb: {out}"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "replaced");
    }

    #[test]
    fn append_creates_and_echoes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.log");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "append", "content": "line1\n"}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(out.contains("append"), "expected 'append' summary: {out}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "line1\n");
    }

    #[test]
    fn duplicate_path_rejected_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("dup.rs");
        let ps = p.to_str().unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [
                {"path": ps, "mode": "overwrite", "content": "a"},
                {"path": ps, "mode": "overwrite", "content": "b"},
            ],
        });
        let err = tool_write(&args, &Session::new(), &fresh_bloom())
            .expect_err("duplicate paths must be rejected");
        assert!(err.contains("duplicate file path"), "unexpected err: {err}");
        // Nothing written: the batch is refused before any file op.
        assert!(!p.exists(), "no file should be created on a rejected batch");
    }

    #[test]
    fn unknown_mode_errors_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.rs");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "bogus", "content": "x"}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(
            out.contains("unknown mode"),
            "expected unknown-mode error: {out}"
        );
    }

    #[test]
    fn mixed_batch_direct_success_survives_hash_failure() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.rs");
        let missing = dir.path().join("missing.rs");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [
                {"path": ok.to_str().unwrap(), "mode": "overwrite", "content": "fn ok(){}\n"},
                {"path": missing.to_str().unwrap(), "mode": "hash",
                 "edits": [{"start": "1:000", "content": "x"}]},
            ],
        });
        // hash task fails (file absent) but the overwrite succeeds, so the
        // call returns Ok with both sections rather than hard-failing.
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(
            out.contains("created"),
            "direct write should report success: {out}"
        );
        assert!(
            out.contains("missing.rs"),
            "hash failure should be surfaced: {out}"
        );
        assert_eq!(std::fs::read_to_string(&ok).unwrap(), "fn ok(){}\n");
    }

    #[test]
    fn refuses_write_outside_scope() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let p = outside.path().join("escape.rs");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "x"}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(
            out.contains("outside scope"),
            "expected scope refusal: {out}"
        );
        assert!(!p.exists(), "out-of-scope path must not be written");
    }

    #[test]
    fn refuses_hash_write_outside_scope() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let p = outside.path().join("secret.rs");
        std::fs::write(&p, "secret\n").unwrap();
        // hash mode must honor the scope guard too — apply_batch does not
        // enforce containment, so a path traversing out of scope would write
        // an arbitrary existing file if the guard skipped hash entries.
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "hash",
                       "edits": [{"start": "1:000", "content": "hacked"}]}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        assert!(
            out.contains("outside scope"),
            "hash mode must honor the scope guard: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "secret\n",
            "out-of-scope file must be untouched"
        );
    }

    #[test]
    fn append_echoes_appended_region_with_correct_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("multi.log");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "append", "content": "d\n"}],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).unwrap();
        // 3 pre-existing lines → the appended line is numbered 4, and only it is
        // echoed. Anchors come from before+content, not a post-write re-read.
        assert!(
            out.contains("echoing last 1 of 4 lines"),
            "echo summary should reflect pre-append count: {out}"
        );
        assert!(
            out.contains("4:"),
            "appended line should be hashlined as line 4: {out}"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\nb\nc\nd\n");
    }

    // -- root parameter tests (issue #73) --

    #[test]
    fn root_param_anchors_relative_path_to_root_not_cwd() {
        // A relative path + explicit `root` must land under `root`, not cwd.
        let root_dir = tempfile::tempdir().unwrap();
        let root_path = root_dir.path();
        let args = serde_json::json!({
            "root": root_path.to_str().unwrap(),
            "files": [{
                "path": "relative/file.txt",
                "mode": "overwrite",
                "content": "hello root\n",
            }],
            "scope": root_path.to_str().unwrap(),
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).expect("create succeeds");
        let expected = root_path.join("relative/file.txt");
        assert!(
            expected.exists(),
            "file must be created under root: {}",
            expected.display()
        );
        assert_eq!(std::fs::read_to_string(&expected).unwrap(), "hello root\n");
        // Output must echo the resolved absolute path.
        let abs_str = expected.canonicalize().unwrap();
        assert!(
            out.contains(abs_str.to_str().unwrap()),
            "output must echo resolved absolute path; got: {out}"
        );
    }

    #[test]
    fn root_param_absolute_path_unaffected_by_root() {
        // An absolute path must be used as-is regardless of `root`.
        let root_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("abs.txt");
        let args = serde_json::json!({
            "root": root_dir.path().to_str().unwrap(),
            "files": [{
                "path": target.to_str().unwrap(),
                "mode": "overwrite",
                "content": "absolute\n",
            }],
            "scope": target_dir.path().to_str().unwrap(),
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).expect("create succeeds");
        assert!(
            target.exists(),
            "absolute path file must exist at its own location: {}",
            target.display()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "absolute\n");
        // The absolute path must not be placed under root.
        assert!(
            !root_dir.path().join("abs.txt").exists(),
            "absolute path must not be placed under root"
        );
        // The output header must contain the target's absolute path.
        let abs_str = target.canonicalize().unwrap();
        assert!(
            out.contains(abs_str.to_str().unwrap()),
            "output must echo resolved absolute path; got: {out}"
        );
    }

    #[test]
    fn result_contains_resolved_absolute_path() {
        // overwrite success output must include the resolved absolute path.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("check.txt");
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "content": "content\n",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).expect("create succeeds");
        let abs_path = p.canonicalize().unwrap();
        assert!(
            out.contains(abs_path.to_str().unwrap()),
            "output must echo resolved absolute path; got: {out}"
        );
    }

    #[test]
    fn root_relative_rejects_non_absolute_root() {
        // A relative `root` value must be rejected.
        let args = serde_json::json!({
            "root": "relative/root",
            "files": [{
                "path": "file.txt",
                "mode": "overwrite",
                "content": "x",
            }],
        });
        let err = tool_write(&args, &Session::new(), &fresh_bloom())
            .expect_err("relative root must be rejected");
        assert!(
            err.contains("must be an absolute path"),
            "error must mention absolute path requirement; got: {err}"
        );
    }

    #[test]
    fn root_only_no_scope_write_succeeds_into_root() {
        // KNOWN HIGH (review on #158): `root` reaches path resolution
        // (resolve_write_path) but NOT the containment guard — `scope_root`
        // fell back to `current_dir()` whenever `scope` was omitted, ignoring
        // `root` entirely. Headline scenario: server process cwd is one
        // directory (call it dirA — here, whatever `current_dir()` naturally
        // is under `cargo test`), the caller passes `root` = a DIFFERENT
        // directory (dirB) plus a relative path under dirB, and supplies NO
        // `scope`. Before the fix, `scope_root` defaulted to dirA, the write
        // resolved into dirB (correctly, via resolve_write_path), and
        // `path_within_scope(dirB_path, dirA)` refused it — a false-positive
        // containment failure for a legitimate root-anchored write.
        //
        // No `set_current_dir` here — the codebase's own tests document that
        // mutating process cwd inside a test races other parallel tests (see
        // edit.rs's `normalize_path_key_is_cwd_independent` comment). Using
        // the ambient `current_dir()` as the implicit "dirA" and a fresh
        // tempdir as "dirB" reproduces the divergence without mutating global
        // state.
        let dir_a = std::env::current_dir().expect("ambient cwd (\"dirA\") must be readable");
        let dir_b = tempfile::tempdir().expect("dirB tempdir");
        assert_ne!(
            dir_a.canonicalize().unwrap(),
            dir_b.path().canonicalize().unwrap(),
            "test setup requires dirA (ambient cwd) and dirB (tempdir) to differ"
        );

        let args = serde_json::json!({
            "root": dir_b.path().to_str().unwrap(),
            "files": [{
                "path": "nested/file.txt",
                "mode": "overwrite",
                "content": "root-only write\n",
            }],
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom())
            .expect("root-only write (no scope) must succeed into root, not refuse");
        assert!(
            !out.contains("error: refusing write outside scope"),
            "root-only write must not be refused by the containment guard: {out}"
        );
        assert!(
            !out.contains("error: scope unresolvable"),
            "root-only write must not hit the scope-unresolvable branch: {out}"
        );

        let expected = dir_b.path().join("nested/file.txt");
        assert!(
            expected.exists(),
            "file must land under root (dirB), not under the server's cwd (dirA): {}",
            expected.display()
        );
        assert_eq!(
            std::fs::read_to_string(&expected).unwrap(),
            "root-only write\n"
        );
    }

    #[test]
    fn cross_worktree_warning_fires_for_different_git_roots() {
        // Two fake worktrees (each with a .git dir): write path git root differs
        // from the cwd-analog git root.
        let server_wt = tempfile::tempdir().unwrap();
        let write_wt = tempfile::tempdir().unwrap();
        std::fs::create_dir(server_wt.path().join(".git")).unwrap();
        std::fs::create_dir(write_wt.path().join(".git")).unwrap();

        let target = write_wt.path().join("target.txt");
        // target doesn't exist yet, so canonicalize fails → None.
        assert!(
            cross_worktree_warning(&target, server_wt.path()).is_none(),
            "no warning before the file is written (canonicalize fails)"
        );
        std::fs::write(&target, "x").unwrap();
        let warn = cross_worktree_warning(&target, server_wt.path());
        assert!(
            warn.is_some(),
            "cross-worktree warning must fire when write and cwd are in different git roots"
        );
        assert!(
            warn.unwrap().contains("cross-worktree"),
            "warning must mention cross-worktree"
        );
    }

    #[test]
    fn cross_worktree_warning_no_false_positive_for_same_root() {
        // Both paths under the same git root — no warning.
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir(wt.path().join(".git")).unwrap();
        let subdir = wt.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        let target = subdir.join("file.txt");
        std::fs::write(&target, "x").unwrap();

        assert!(
            cross_worktree_warning(&target, wt.path()).is_none(),
            "no cross-worktree warning for a write within the same git root"
        );
    }

    #[test]
    fn cross_worktree_warning_fires_for_git_file_worktree() {
        // A linked worktree has a `.git` FILE (gitdir pointer), not a directory.
        // find_git_root uses `.exists()`, which is true for files too — confirm.
        let server_wt = tempfile::tempdir().unwrap();
        let write_wt = tempfile::tempdir().unwrap();
        std::fs::create_dir(server_wt.path().join(".git")).unwrap();
        std::fs::write(
            write_wt.path().join(".git"),
            "gitdir: /some/other/.git/worktrees/issue-foo\n",
        )
        .unwrap();

        let target = write_wt.path().join("target.txt");
        std::fs::write(&target, "x").unwrap();

        let warn = cross_worktree_warning(&target, server_wt.path());
        assert!(
            warn.is_some(),
            "cross-worktree warning must fire for a .git-file linked worktree"
        );
        assert!(
            warn.unwrap().contains("cross-worktree"),
            "warning must mention cross-worktree"
        );
    }

    // -- hash-mode root tests (issue #73) --

    #[test]
    fn hash_mode_root_anchors_relative_path() {
        // hash-mode relative path + explicit `root` → file lands under root,
        // output echoes the resolved absolute path.
        let root_dir = tempfile::tempdir().unwrap();
        let root_path = root_dir.path();
        let subdir = root_path.join("src");
        std::fs::create_dir(&subdir).unwrap();
        let content = "line one\nline two\n";
        let target = subdir.join("edit_me.rs");
        std::fs::write(&target, content).unwrap();

        let h = crate::format::line_hash(b"line one");
        let anchor = format!("1:{h:03x}");

        let args = serde_json::json!({
            "root": root_path.to_str().unwrap(),
            "scope": root_path.to_str().unwrap(),
            "files": [{
                "path": "src/edit_me.rs",
                "mode": "hash",
                "edits": [{
                    "start": anchor,
                    "end": anchor,
                    "content": "line ONE"
                }]
            }]
        });
        let out = tool_write(&args, &Session::new(), &fresh_bloom()).expect("hash edit succeeds");

        assert!(
            target.exists(),
            "file must remain under root: {}",
            target.display()
        );
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(
            written.contains("line ONE"),
            "edit must have applied: {written}"
        );
        let abs = target.canonicalize().unwrap();
        assert!(
            out.contains(abs.to_str().unwrap()),
            "output must echo resolved absolute path; got: {out}"
        );
    }

    #[test]
    fn hash_mode_cross_worktree_warning_fires() {
        // The hash-mode branch calls cross_worktree_warning over the tracked
        // relative paths. Exercise the helper directly (triggering it through
        // tool_write needs a relative path that traverses git roots, which is
        // environment-dependent).
        let wt1 = tempfile::tempdir().unwrap();
        let wt2 = tempfile::tempdir().unwrap();
        std::fs::create_dir(wt1.path().join(".git")).unwrap();
        std::fs::create_dir(wt2.path().join(".git")).unwrap();

        let target = wt2.path().join("file.rs");
        std::fs::write(&target, "x\n").unwrap();

        let warn = cross_worktree_warning(&target, wt1.path());
        assert!(
            warn.is_some(),
            "hash-mode cross-worktree warning must fire when write lands in a different git root"
        );
        let warn_str = warn.unwrap();
        assert!(
            warn_str.contains("cross-worktree write"),
            "warning text must mention cross-worktree write: {warn_str}"
        );
        assert!(
            warn_str.contains("Pass `root`"),
            "warning must suggest passing root: {warn_str}"
        );
    }

    #[test]
    fn hash_mode_no_warning_for_in_tree_write() {
        // hash-mode write within the same git root as cwd → no warning.
        let wt = tempfile::tempdir().unwrap();
        std::fs::create_dir(wt.path().join(".git")).unwrap();
        let target = wt.path().join("inplace.rs");
        std::fs::write(&target, "x\n").unwrap();

        assert!(
            cross_worktree_warning(&target, wt.path()).is_none(),
            "no cross-worktree warning expected for an in-tree hash-mode write"
        );
    }
}
