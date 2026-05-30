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
pub(in crate::mcp) fn parse_file_edit(index: usize, val: &Value) -> crate::edit::FileEditTask {
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
        path: PathBuf::from(path_str),
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

    let (scope_root, _scope_warn) = super::resolve_scope(args);
    // Canonicalize the scope once for the overwrite/append containment check.
    // Fail closed on canonicalize failure: an unresolvable scope refuses
    // direct writes rather than silently disabling the guard.
    let scope_canon: Result<PathBuf, std::io::Error> = scope_root.canonicalize();

    let mut hash_tasks: Vec<crate::edit::FileEditTask> = Vec::new();
    let mut direct_results: Vec<String> = Vec::new();
    let mut direct_applied: Vec<PathBuf> = Vec::new();

    for (i, f) in files_val.iter().enumerate() {
        let mode = f.get("mode").and_then(|v| v.as_str()).unwrap_or("hash");
        let Some(path_str) = f.get("path").and_then(|v| v.as_str()) else {
            direct_results.push(format!("## files[{i}]\nerror: missing 'path'"));
            continue;
        };
        let path = PathBuf::from(path_str);

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
            "hash" | "h" => hash_tasks.push(parse_file_edit(i, f)),
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
                            path.display(),
                            content.len(),
                            crate::format::hashlines(content, 1),
                        );
                        if show_diff {
                            block.push_str(&render_text_diff(before.as_deref(), content));
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
                            path.display(),
                            content.len(),
                            total - start_idx,
                            crate::format::hashlines(&tail, start_line),
                        );
                        if show_diff {
                            block.push_str(&render_text_diff(before.as_deref(), &after));
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
            Ok(combined) => output.push_str(&combined),
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
        match parse_file_edit(0, &val) {
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
}
