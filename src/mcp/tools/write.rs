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

use crate::format::noun;
use crate::session::Session;
use tilth_core::index::bloom::BloomFilterCache;
use tilth_core::search::walk::WalkConfig;

/// Parse one `files[]` entry into a hash-mode edit task. Parse errors are
/// deferred onto the task so a malformed entry surfaces as a per-file failure
/// instead of aborting the whole batch.
pub(in crate::mcp) fn parse_file_edit(
    index: usize,
    val: &Value,
    path: PathBuf,
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

    FileEditTask::Ready { path, edits }
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

/// The line diff of a direct write, every line removed then every line
/// added. `None` is a binary side: its header says so, and it shows no lines.
fn render_text_diff(before: Option<&str>, after: Option<&str>) -> String {
    let binary = |text: Option<&str>| if text.is_some() { "" } else { " (binary)" };
    let mut out = format!(
        "── diff ──\n--- before{}\n+++ after{}\n",
        binary(before),
        binary(after)
    );
    for line in before.unwrap_or_default().lines() {
        let _ = writeln!(out, "- {line}");
    }
    for line in after.unwrap_or_default().lines() {
        let _ = writeln!(out, "+ {line}");
    }
    out
}

/// What an append of `content` reports, given the file's text `before` it
/// (`None` for a binary file). A text file echoes the appended region's
/// hashlines alone, so a log-shaped file does not balloon the response; a
/// binary file has no lines to number, so it echoes none.
fn append_echo(before: Option<&str>, content: &str, show_diff: bool) -> String {
    let bytes = format!("{} {}", content.len(), noun(content.len(), "byte", "bytes"));
    let Some(before) = before else {
        let mut echo = format!("append: {bytes} to a binary file, not echoed\n");
        if show_diff {
            echo.push_str(&render_text_diff(None, None));
        }
        return echo;
    };
    let after = format!("{before}{content}");
    // No trailing newline on the prior content means the first appended line
    // merges onto the last existing line, so the echoed region starts one
    // line earlier.
    let merged = !before.is_empty() && !before.ends_with('\n');
    let pre_count = before.lines().count();
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
    let mut echo = format!(
        "append: {bytes} (echoing last {} of {total} {})\n{}",
        total - start_idx,
        noun(total, "line", "lines"),
        crate::format::hashlines(&tail, start_line),
    );
    if show_diff {
        echo.push_str(&render_text_diff(Some(before), Some(&after)));
    }
    echo
}

/// The text of the file at `path` before a write, by the one rule of
/// [`tilth_core::source::into_text`], for the echo and the diff alone:
/// `None` for a binary file. A file that is absent or cannot be read has
/// no earlier text, so it reads as empty.
fn earlier_text(path: &Path) -> Option<String> {
    tilth_core::source::read(path)
        .map_or_else(|_| Some(String::new()), tilth_core::source::into_text)
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

/// Resolve a write path: a relative `path_str` is anchored under `base` (the
/// call's `root`, else the server's root); absolute paths are used as-is.
fn resolve_write_path(path_str: &str, base: Option<&Path>) -> Result<PathBuf, String> {
    let p = PathBuf::from(path_str);
    if p.is_absolute() {
        return Ok(p);
    }
    Ok(super::require_root(base)?.join(p))
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
        dir = dir.parent()?.to_path_buf();
    }
}

/// Build a cross-worktree warning string. Called when a relative path write
/// (no `root` argument) resolves into a different git worktree than the
/// server's root. Returns `None` when no warning is needed.
fn cross_worktree_warning(path: &Path, server_root: &Path) -> Option<String> {
    // Callers pass only paths a write landed at. Only warn for those that
    // resolve into a git repo.
    let resolved = path.canonicalize().ok()?;
    let write_root = find_git_root(&resolved)?;
    let server_git_root = find_git_root(server_root)?;
    if write_root == server_git_root {
        return None;
    }
    Some(format!(
        "\n⚠️  cross-worktree write: resolved path is {} (git root: {}), \
         server root's git root: {}. Pass `root` or use an absolute path to \
         make the target explicit.",
        resolved.display(),
        write_root.display(),
        server_git_root.display(),
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

#[expect(clippy::too_many_lines, reason = "split by responsibility in #315")]
pub(in crate::mcp) fn tool_write(
    args: &Value,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
    walk: WalkConfig,
    server_root: Option<&Path>,
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
    // server's root.
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
    // the server's root here would refuse a legitimate root-only write
    // whenever the server was started for a different directory than `root`
    // (the headline cross-worktree scenario `root` exists to solve). Only
    // when BOTH `scope` and `root` are absent does this fall back to the
    // server's root. A relative `scope` resolves against the server's root
    // too. A server without a root leaves those without a boundary, so their
    // writes are refused with that cause.
    let scope_arg = args.get("scope").and_then(Value::as_str).map(Path::new);
    let scope_root: Result<PathBuf, String> = match (scope_arg, &root) {
        (Some(scope), _) if scope.is_relative() => {
            super::require_root(server_root).map(|r| r.join(scope))
        }
        (Some(scope), _) => Ok(scope.to_path_buf()),
        (None, Some(root)) => Ok(root.clone()),
        (None, None) => super::require_root(server_root).map(Path::to_path_buf),
    };
    // Canonicalize the scope once for the overwrite/append containment check.
    // Fail closed on canonicalize failure: an unresolvable scope refuses
    // writes rather than silently disabling the guard.
    let scope_canon: Result<PathBuf, String> =
        scope_root.and_then(|scope| scope.canonicalize().map_err(|e| e.to_string()));
    // A relative path is anchored under `root` when supplied, else under the
    // server's root.
    let base = root.as_deref().or(server_root);
    // The cross-worktree warning for a write that landed at `path`: only a
    // relative path with no `root` can land somewhere the caller did not
    // name, so only those are checked, against the server's root.
    let crossed_worktree = |path_str: &str, path: &Path| -> Option<String> {
        if root.is_some() || Path::new(path_str).is_absolute() {
            return None;
        }
        cross_worktree_warning(path, server_root?)
    };
    // A refusal names the parameter that moves the boundary: an explicit
    // `scope` sets it, otherwise `root` does.
    let widen_hint = if scope_arg.is_some() {
        "pass a scope that contains this path"
    } else {
        "pass root: <dir containing this path> to write outside it"
    };

    let mut hash_tasks: Vec<crate::edit::FileEditTask> = Vec::new();
    // The path each hash-mode file was given as, by resolved path, so the
    // cross-worktree warning can fire after apply_batch.
    let mut hash_given: Vec<(PathBuf, &str)> = Vec::new();
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
        // the server's root, and a cross-worktree warning fires below if it
        // resolves into a different git worktree than the server's root.
        let path = match resolve_write_path(path_str, base) {
            Ok(path) => path,
            Err(e) => {
                direct_results.push(format!("## {path_str}\nerror: {e}"));
                continue;
            }
        };

        // Scope guard for ALL modes. hash mode delegates to apply_batch, which
        // computes a package_root only for blast-radius reporting — it does NOT
        // enforce write containment, so a `../../etc/passwd` hash entry would
        // otherwise traverse out of scope exactly like a direct write. Run the
        // canonical-containment check uniformly before dispatching any mode.
        match scope_canon.as_ref() {
            Ok(root) => {
                if !path_within_scope(&path, root) {
                    direct_results.push(format!(
                        "## {}\nerror: refusing write outside scope ({}); {widen_hint}",
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
                hash_given.push((path.clone(), path_str));
                hash_tasks.push(parse_file_edit(i, f, path));
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
                let before = if show_diff && pre_existed {
                    earlier_text(&path)
                } else {
                    Some(String::new())
                };
                match crate::mcp::write::write_overwrite(&path, content, overwrite) {
                    Ok(()) => {
                        let line_count = content.lines().count();
                        let verb = if pre_existed { "overwrote" } else { "created" };
                        let mut block = format!(
                            "## {}\n{verb}: {} {}, {line_count} {}\n{}",
                            resolved_display(&path),
                            content.len(),
                            noun(content.len(), "byte", "bytes"),
                            noun(line_count, "line", "lines"),
                            crate::format::hashlines(content, 1),
                        );
                        if show_diff {
                            block.push_str(&render_text_diff(before.as_deref(), Some(content)));
                        }
                        // Warn when a relative path (no `root`) crosses a worktree boundary.
                        if let Some(warn) = crossed_worktree(path_str, &path) {
                            block.push_str(&warn);
                        }
                        direct_results.push(block);
                        direct_applied.push(path);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        direct_results.push(format!(
                            "## {}\nerror: {}",
                            path.display(),
                            tilth_core::error::TilthError::AlreadyExists { path: path.clone() }
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
                let before = earlier_text(&path);
                match crate::mcp::write::write_append(&path, content) {
                    Ok(()) => {
                        let mut block = format!(
                            "## {}\n{}",
                            resolved_display(&path),
                            append_echo(before.as_deref(), content, show_diff)
                        );
                        // Warn when a relative path (no `root`) crosses a worktree boundary.
                        if let Some(warn) = crossed_worktree(path_str, &path) {
                            block.push_str(&warn);
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
    // A hash task exists only once its path passed the containment check
    // against `scope_canon`, so there are none unless that resolved.
    if let (false, Ok(scope)) = (hash_tasks.is_empty(), &scope_canon) {
        // Record reads up front: record_read counts attempts, not just
        // committed edits.
        for task in &hash_tasks {
            if let crate::edit::FileEditTask::Ready { path, .. } = task {
                session.record_read(path);
            }
        }
        match crate::edit::apply_batch(hash_tasks, bloom, walk, show_diff, scope) {
            Ok(batch) => {
                output.push_str(&batch.output);
                // Cross-worktree warning for the hash-mode relative paths whose
                // edits applied.
                for path in &batch.applied {
                    let given = hash_given.iter().find(|(p, _)| p == path);
                    if let Some(warn) = given.and_then(|(_, s)| crossed_worktree(s, path)) {
                        output.push_str(&warn);
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
#[expect(
    clippy::disallowed_methods,
    reason = "by design: tests read the files they check directly"
)]
mod tests {
    use super::*;

    fn fresh_bloom() -> Arc<BloomFilterCache> {
        Arc::new(BloomFilterCache::new())
    }

    #[test]
    fn parse_file_edit_rejects_empty_edits_array() {
        let val = serde_json::json!({ "path": "noop.txt", "edits": [] });
        match parse_file_edit(0, &val, PathBuf::from("noop.txt")) {
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let err = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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
    fn refusal_outside_launch_folder_names_root() {
        // #319: with neither `scope` nor `root`, the boundary is the server's
        // root. An absolute path outside it is refused, and the refusal must
        // name the parameter that moves the boundary.
        let server_root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let p = outside.path().join("probe.txt");
        let args = serde_json::json!({
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "probe\n"}],
        });
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            Some(server_root.path()),
        )
        .unwrap();
        assert!(
            out.contains("refusing write outside scope")
                && out.contains("pass root: <dir containing this path>"),
            "refusal must say to pass root: {out}"
        );
        assert!(!p.exists(), "out-of-scope path must not be written");

        // Following the advice writes the file.
        let args = serde_json::json!({
            "root": outside.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "probe\n"}],
        });
        tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "probe\n");
    }

    #[test]
    fn refusal_outside_explicit_scope_names_scope() {
        // An explicit `scope` wins over `root` as the boundary, so the advice
        // for it is a wider scope, not a root.
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let p = outside.path().join("escape.rs");
        let args = serde_json::json!({
            "scope": dir.path().to_str().unwrap(),
            "files": [{"path": p.to_str().unwrap(), "mode": "overwrite", "content": "x"}],
        });
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
        assert!(
            out.contains("pass a scope that contains this path"),
            "refusal under an explicit scope must say to widen it: {out}"
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
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

    /// One `tilth_write` call with `diff: true` on the files in `dir`.
    fn write_with_diff(dir: &Path, files: &Value) -> String {
        let args = serde_json::json!({
            "scope": dir.to_str().unwrap(),
            "diff": true,
            "files": files,
        });
        tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap()
    }

    /// A file that is not UTF-8 is text by the one rule, so an append
    /// numbers its lines from the file's real line count and the diff shows
    /// its earlier lines, each Latin-1 byte as U+FFFD. The file keeps its
    /// bytes.
    #[test]
    fn append_to_latin1_file_numbers_from_its_line_count() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"one\ncaf\xe9\nthree\n").unwrap();
        let out = write_with_diff(
            dir.path(),
            &serde_json::json!([{"path": p.to_str().unwrap(), "mode": "append", "content": "four\n"}]),
        );
        assert!(out.contains("echoing last 1 of 4 lines"), "{out}");
        assert!(out.contains("\n4:"), "the appended line is line 4: {out}");
        assert!(
            out.contains("- one\n- caf\u{fffd}\n- three\n+ one\n+ caf\u{fffd}\n"),
            "{out}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"one\ncaf\xe9\nthree\nfour\n");
    }

    /// An append onto a Latin-1 line with no newline echoes the merged line
    /// as text, and writes only the appended bytes after the file's own.
    #[test]
    fn append_merging_onto_latin1_line_keeps_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"one\ncaf\xe9").unwrap();
        let out = write_with_diff(
            dir.path(),
            &serde_json::json!([{"path": p.to_str().unwrap(), "mode": "append", "content": "s\n"}]),
        );
        assert!(out.contains("echoing last 1 of 2 lines"), "{out}");
        assert!(out.contains("|caf\u{fffd}s\n"), "{out}");
        assert_eq!(std::fs::read(&p).unwrap(), b"one\ncaf\xe9s\n");
    }

    /// An overwrite's diff shows a Latin-1 file's earlier lines, and the file
    /// then holds exactly the new content.
    #[test]
    fn overwrite_diff_of_latin1_file_shows_its_old_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"old caf\xe9\nold two\n").unwrap();
        let out = write_with_diff(
            dir.path(),
            &serde_json::json!([{
                "path": p.to_str().unwrap(), "mode": "overwrite", "overwrite": true, "content": "new\n",
            }]),
        );
        assert!(
            out.contains("--- before\n+++ after\n- old caf\u{fffd}\n- old two\n+ new\n"),
            "{out}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"new\n");
    }

    /// A binary file has no text by the one rule: an append to one echoes
    /// no lines, and a diff names the binary side instead of showing it as
    /// empty. The file keeps its bytes.
    #[test]
    fn writes_to_binary_file_show_no_text_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("blob.bin");
        let ovr = dir.path().join("old.bin");
        std::fs::write(&app, b"\x89PNG\0\xff\n").unwrap();
        std::fs::write(&ovr, b"\x89PNG\0\xff\n").unwrap();
        let out = write_with_diff(
            dir.path(),
            &serde_json::json!([
                {"path": app.to_str().unwrap(), "mode": "append", "content": "tail\n"},
                {"path": ovr.to_str().unwrap(), "mode": "overwrite", "overwrite": true, "content": "text\n"},
            ]),
        );
        assert!(
            out.contains(
                "append: 5 bytes to a binary file, not echoed\n── diff ──\n--- before (binary)\n+++ after (binary)\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("--- before (binary)\n+++ after\n+ text\n"),
            "{out}"
        );
        assert_eq!(std::fs::read(&app).unwrap(), b"\x89PNG\0\xff\ntail\n");
        assert_eq!(std::fs::read(&ovr).unwrap(), b"text\n");
    }

    // -- root parameter tests (issue #73) --

    #[test]
    fn root_param_anchors_relative_path_to_root_not_server_root() {
        // A relative path + explicit `root` must land under `root`, not the server root.
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .expect("create succeeds");
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .expect("create succeeds");
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .expect("create succeeds");
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
        let err = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
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
        // fell back to the server's directory whenever `scope` was omitted,
        // ignoring `root` entirely. Headline scenario: the server's root is
        // one directory (dirA), the caller passes `root` = a DIFFERENT
        // directory (dirB) plus a relative path under dirB, and supplies NO
        // `scope`. Before the fix, `scope_root` defaulted to dirA, the write
        // resolved into dirB (correctly, via resolve_write_path), and
        // `path_within_scope(dirB_path, dirA)` refused it — a false-positive
        // containment failure for a legitimate root-anchored write.
        let dir_a = tempfile::tempdir().expect("dirA tempdir");
        let dir_b = tempfile::tempdir().expect("dirB tempdir");

        let args = serde_json::json!({
            "root": dir_b.path().to_str().unwrap(),
            "files": [{
                "path": "nested/file.txt",
                "mode": "overwrite",
                "content": "root-only write\n",
            }],
        });
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            Some(dir_a.path()),
        )
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
            "file must land under root (dirB), not under the server's root (dirA): {}",
            expected.display()
        );
        assert_eq!(
            std::fs::read_to_string(&expected).unwrap(),
            "root-only write\n"
        );
    }

    /// A relative path with no `root` lands under the server's root, never
    /// under the process's working directory.
    #[test]
    fn relative_path_without_root_lands_under_the_server_root() {
        let server_root = tempfile::tempdir().unwrap();
        let args = serde_json::json!({
            "files": [{ "path": "nested/new.txt", "mode": "overwrite", "content": "x\n" }],
        });
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            Some(server_root.path()),
        )
        .unwrap();
        assert!(out.contains("created"), "{out}");
        assert!(server_root.path().join("nested/new.txt").exists());
    }

    /// With no root anywhere, a relative path has nowhere to land, and the
    /// result says why instead of writing under the process's directory.
    #[test]
    fn relative_path_without_any_root_is_refused() {
        let args = serde_json::json!({
            "files": [{ "path": "tilth-no-root-probe.txt", "mode": "overwrite", "content": "x\n" }],
        });
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .unwrap();
        assert!(
            out.contains("## tilth-no-root-probe.txt") && out.contains(super::super::NO_ROOT),
            "{out}"
        );
        assert!(!Path::new("tilth-no-root-probe.txt").exists());
    }

    #[test]
    fn cross_worktree_warning_fires_for_different_git_roots() {
        // Two fake worktrees (each with a .git dir): write path git root differs
        // from the server root's git root.
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
            "cross-worktree warning must fire when write and server root are in different git roots"
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
        let out = tool_write(
            &args,
            &Session::new(),
            &fresh_bloom(),
            WalkConfig::default(),
            None,
        )
        .expect("hash edit succeeds");

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
        // hash-mode write within the same git root as the server root → no warning.
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
