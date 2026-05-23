//! `tilth_write` — hash / overwrite / append modes per file, plus the
//! strict fingerprint auto-fix used when a hash anchor drifts.
//!
//! The `overwrite` mode is **create-only by default** (atomic
//! `O_CREAT|O_EXCL` open). Pass a per-file `overwrite: true` flag to
//! swallow `AlreadyExists` and replace the file. Successful overwrite/append
//! results echo back the hashlined contents so the agent can chain anchored
//! edits in the next call without a re-read.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;
use crate::session::Session;

pub(crate) fn tool_write(
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
    let show_diff = args
        .get("diff")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Partition into hash-mode tasks (delegate to existing apply_batch) and
    // direct overwrite/append tasks (handled inline).
    let mut hash_tasks: Vec<crate::edit::FileEditTask> = Vec::new();
    let mut direct_results: Vec<String> = Vec::new();
    let mut direct_applied: Vec<PathBuf> = Vec::new();

    let (scope_root, _scope_warn) = super::resolve_scope(args);
    // resolve_scope returns `.` (PathBuf) when scope == cwd; canonicalize for
    // the containment check below. Fail closed on canonicalize failure: an
    // unresolvable scope must refuse overwrite/append rather than silently
    // disabling the guard (the symmetric behavior in `path_within_scope`).
    let scope_canon: Result<PathBuf, std::io::Error> = scope_root.canonicalize();
    for (i, f) in files_val.iter().enumerate() {
        let mode = f.get("mode").and_then(|v| v.as_str()).unwrap_or("hash");
        let Some(path_str) = f.get("path").and_then(|v| v.as_str()) else {
            direct_results.push(format!("## files[{i}]\nerror: missing 'path'"));
            continue;
        };
        let path = PathBuf::from(path_str);
        // Scope guard for overwrite/append: hash mode goes through
        // `edit::apply_batch` which canonicalizes + roots to `package_root`.
        // overwrite/append accept any path the client sends, so we refuse
        // writes that resolve outside the configured scope OR when the scope
        // itself cannot be canonicalized (fail closed).
        if matches!(mode, "overwrite" | "w" | "append" | "a") {
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
                            "## {}\nerror: file already exists — pass `overwrite: true` to replace it",
                            path.display()
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
                let before = show_diff
                    .then(|| std::fs::read_to_string(&path).ok())
                    .flatten();
                match crate::mcp::write::write_append(&path, content) {
                    Ok(()) => {
                        // Echo only the appended region's hashlines so
                        // log-shaped append targets don't balloon the
                        // response. The agent can tilth_read the file
                        // separately if it needs anchors for pre-existing
                        // content.
                        let after = std::fs::read_to_string(&path)
                            .unwrap_or_else(|_| before.clone().unwrap_or_default() + content);
                        let after_lines: Vec<&str> = after.lines().collect();
                        let total = after_lines.len();
                        let appended = content.lines().count().max(1);
                        let start_idx = total.saturating_sub(appended);
                        let tail = after_lines[start_idx..].join("\n");
                        let start_line = (start_idx + 1) as u32;
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
        // Record session reads for every Ready task up front (matches legacy
        // tilth_edit semantics: record_read counts attempts, not just
        // successful commits, so dedup logic doesn't re-recommend a file the
        // agent already touched even when hashes drifted).
        for task in &hash_tasks {
            if let crate::edit::FileEditTask::Ready { path, .. } = task {
                session.record_read(path);
            }
        }
        // Pre-run strict auto-fix on hash-mode tasks. Capture original
        // anchor-range bodies, then try the standard apply_batch. If the
        // outcome reports hash mismatches per file, attempt auto-fix.
        let originals: Vec<Option<HashOriginal>> =
            hash_tasks.iter().map(capture_hash_original).collect();
        match crate::edit::apply_batch(hash_tasks, bloom, show_diff) {
            Ok(combined) => {
                // Per-file independence: when a file's section reports a hash
                // mismatch, append a per-file auto-fix probe so spec criterion 9
                // (strict auto-fix on mismatch, per file) holds even on partial
                // batch success. The probe re-applies on a single-match
                // relocation, so any path it touches is recorded as read.
                let (augmented, reapplied) = append_per_file_auto_fix(&combined, &originals, bloom);
                for p in &reapplied {
                    session.record_read(p);
                }
                output.push_str(&augmented);
            }
            Err(msg) => {
                // All-failed path. Try auto-fix for each captured original.
                let (fixed, reapplied) = try_auto_fix(&msg, &originals, bloom);
                for p in &reapplied {
                    session.record_read(p);
                }
                output.push_str(&fixed);
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

/// Returns true if `path` resolves under `scope` (canonical path containment).
/// For paths that don't yet exist, canonicalize the nearest existing ancestor
/// and append the remaining components.
fn path_within_scope(path: &Path, scope: &Path) -> bool {
    let Ok(scope_canon) = scope.canonicalize() else {
        return false;
    };
    // Walk up until a component canonicalizes.
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

#[derive(Clone)]
struct HashOriginal {
    path: PathBuf,
    body: String,
    start: usize,
    end: usize,
    /// Full edit list captured pre-apply so a relocation probe can rebuild
    /// the batch at the new line with freshly-computed hashes and re-invoke
    /// `apply_batch` (spec criterion 9: "exactly one match → apply edit at
    /// that new location").
    edits: Vec<crate::edit::Edit>,
}

fn capture_hash_original(task: &crate::edit::FileEditTask) -> Option<HashOriginal> {
    let crate::edit::FileEditTask::Ready { path, edits } = task else {
        return None;
    };
    let first = edits.first()?;
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    if first.start_line == 0 || first.start_line > lines.len() {
        return None;
    }
    let s = first.start_line - 1;
    let e = first.end_line.min(lines.len());
    let body = lines[s..e].join("\n");
    Some(HashOriginal {
        path: path.clone(),
        body,
        start: first.start_line,
        end: first.end_line,
        edits: edits.clone(),
    })
}

/// Rebuild the captured edits at the relocated `new_line`, recomputing hashes
/// against the current file content, and apply via `crate::edit::apply_batch`.
/// Returns the per-file section emitted by `apply_batch` on success, or `None`
/// when the relocation cannot be reapplied (file gone, line out of bounds,
/// apply failed). The first edit anchors the offset; subsequent edits in the
/// same file shift by the same delta so multi-edit batches survive the move.
fn reapply_at_relocation(
    orig: &HashOriginal,
    new_line: usize,
    bloom: &Arc<BloomFilterCache>,
) -> Option<String> {
    use crate::edit::{apply_batch, Edit, FileEditTask};

    let old_first = orig.edits.first()?.start_line;
    let delta: isize = new_line as isize - old_first as isize;
    let content = std::fs::read_to_string(&orig.path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    let mut shifted: Vec<Edit> = Vec::with_capacity(orig.edits.len());
    for e in &orig.edits {
        let new_start = (e.start_line as isize) + delta;
        let new_end = (e.end_line as isize) + delta;
        if new_start < 1 || new_end < 1 {
            return None;
        }
        let (s, en) = (new_start as usize, new_end as usize);
        if s == 0 || en == 0 || s > total || en > total {
            return None;
        }
        let start_hash = crate::format::line_hash(lines[s - 1].as_bytes());
        let end_hash = crate::format::line_hash(lines[en - 1].as_bytes());
        shifted.push(Edit {
            start_line: s,
            start_hash,
            end_line: en,
            end_hash,
            content: e.content.clone(),
        });
    }

    let task = FileEditTask::Ready {
        path: orig.path.clone(),
        edits: shifted,
    };
    apply_batch(vec![task], bloom, false).ok()
}

/// Probe one captured original for a strict-fingerprint relocation and,
/// on exactly one match, re-apply the original edit at the new location
/// (spec criterion 9). Returns the formatted line(s) describing the outcome
/// (relocated+applied / relocated-only / ambiguous / err).
fn probe_one_auto_fix(orig: &HashOriginal, bloom: &Arc<BloomFilterCache>) -> String {
    use crate::mcp::write::{auto_fix_locate, fresh_region, AutoFixResult};
    let mut out = String::new();
    match auto_fix_locate(&orig.path, &orig.body) {
        Ok(AutoFixResult::Relocated { new_line }) => {
            // Emit the verbatim prompt-promised line first so agents that
            // pattern-match on `auto-fixed: <old> → <new>` (prompts/mcp-edit.md)
            // see the literal signal.
            let _ = writeln!(out, "auto-fixed: {} → {}", orig.start, new_line);
            match reapply_at_relocation(orig, new_line, bloom) {
                Some(section) => {
                    let _ = writeln!(
                        out,
                        "{}: auto-fixed — edit re-applied at line {} (was {})",
                        orig.path.display(),
                        new_line,
                        orig.start
                    );
                    out.push_str(&section);
                    out.push('\n');
                }
                None => {
                    let _ = writeln!(
                        out,
                        "{}: auto-fixed candidate — original anchor body found at line {} (was {}); re-apply failed",
                        orig.path.display(),
                        new_line,
                        orig.start
                    );
                }
            }
        }
        Ok(AutoFixResult::Ambiguous { matches }) => {
            let _ = writeln!(
                out,
                "{}: {matches} matches for original body — fresh region below; retry with new anchors",
                orig.path.display(),
            );
            if let Ok(fresh) = fresh_region(&orig.path, orig.start, orig.end) {
                out.push_str(&fresh);
                out.push('\n');
            }
        }
        Err(e) => {
            let _ = writeln!(out, "{}: auto-fix failed: {e}", orig.path.display());
        }
    }
    out
}

/// Scan `apply_batch` output for `## <path>` sections that report a hash
/// mismatch and append a per-file auto-fix probe to each one. Sections that
/// applied cleanly are left untouched. Returns the augmented output plus the
/// list of paths whose edits were re-applied at a relocated anchor — callers
/// use the second value to extend session bookkeeping (`record_read`) so a
/// successful auto-fix is treated as a write.
fn append_per_file_auto_fix(
    output: &str,
    originals: &[Option<HashOriginal>],
    bloom: &Arc<BloomFilterCache>,
) -> (String, Vec<PathBuf>) {
    let needs_probe = output.contains("hash mismatch");
    if !needs_probe {
        return (output.to_string(), Vec::new());
    }
    let by_path: std::collections::HashMap<String, &HashOriginal> = originals
        .iter()
        .flatten()
        .map(|o| (o.path.display().to_string(), o))
        .collect();
    let sections: Vec<&str> = output.split("\n\n---\n\n").collect();
    let mut rendered: Vec<String> = Vec::with_capacity(sections.len());
    let mut reapplied: Vec<PathBuf> = Vec::new();
    for section in sections {
        if !section.contains("hash mismatch") {
            rendered.push(section.to_string());
            continue;
        }
        // First line is `## <path>` — extract path key.
        let path_str = section
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("## "))
            .unwrap_or("")
            .trim();
        let Some(orig) = by_path.get(path_str) else {
            rendered.push(section.to_string());
            continue;
        };
        let probe = probe_one_auto_fix(orig, bloom);
        if probe.contains("auto-fixed —") {
            reapplied.push(orig.path.clone());
        }
        let mut s = section.to_string();
        s.push_str("\n\n── auto-fix probe ──\n");
        s.push_str(&probe);
        rendered.push(s);
    }
    (rendered.join("\n\n---\n\n"), reapplied)
}

fn try_auto_fix(
    original_msg: &str,
    originals: &[Option<HashOriginal>],
    bloom: &Arc<BloomFilterCache>,
) -> (String, Vec<PathBuf>) {
    let mut out = String::from("hash mismatch — attempted auto-fix:\n\n");
    out.push_str(original_msg);
    out.push_str("\n\n── auto-fix probe ──\n");
    let mut reapplied: Vec<PathBuf> = Vec::new();
    for orig in originals.iter().flatten() {
        let probe = probe_one_auto_fix(orig, bloom);
        if probe.contains("auto-fixed —") {
            reapplied.push(orig.path.clone());
        }
        out.push_str(&probe);
    }
    (out, reapplied)
}

/// Parse one `files[]` entry. Parse errors are deferred onto the task so a
/// malformed entry surfaces as a per-file failure instead of aborting the
/// whole batch.
fn parse_file_edit(index: usize, val: &Value) -> crate::edit::FileEditTask {
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
            msg: "'edits' array is empty".into(),
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

/// Per-file `overwrite` flag (strict boolean). Missing ⇒ false. Returns
/// `None` if the field is present but not a JSON boolean — the caller surfaces
/// that as a per-file error rather than silently coercing.
fn parse_overwrite_flag(f: &Value) -> Option<bool> {
    match f.get("overwrite") {
        None => Some(false),
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// Parse a single `edits[]` entry. Flat early-returns keep nesting shallow.
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::index::bloom::BloomFilterCache;
    use crate::session::Session;

    fn services() -> (Session, Arc<BloomFilterCache>) {
        (Session::new(), Arc::new(BloomFilterCache::new()))
    }

    #[test]
    fn overwrite_new_file_creates_and_returns_hashlines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.rs");
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "content": "fn main() {}\n",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out = tool_write(&args, &session, &bloom).expect("create succeeds");
        assert!(out.contains("created:"), "verb should be `created`: {out}");
        assert!(
            out.contains("1:") && out.contains("|fn main() {}"),
            "hashlined output for new file missing: {out}"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fn main() {}\n");
    }

    #[test]
    fn overwrite_existing_file_without_flag_errors_with_helpful_message() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.rs");
        std::fs::write(&p, "old\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "content": "new\n",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out = tool_write(&args, &session, &bloom).expect("partial-failure returns Ok");
        assert!(
            out.contains("already exists") && out.contains("overwrite: true"),
            "expected guidance-bearing AlreadyExists error: {out}"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");
    }

    #[test]
    fn overwrite_true_swallows_already_exists_and_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.rs");
        std::fs::write(&p, "old contents\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "overwrite": true,
                "content": "replaced\n",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out = tool_write(&args, &session, &bloom).expect("overwrite succeeds");
        assert!(
            out.contains("overwrote:"),
            "verb should be `overwrote`: {out}"
        );
        assert!(out.contains("|replaced"), "hashlined output missing: {out}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "replaced\n");
    }

    #[test]
    fn overwrite_non_bool_flag_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.rs");
        std::fs::write(&p, "old\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "overwrite": "true",
                "content": "x",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out =
            tool_write(&args, &session, &bloom).expect("error reported per file, not at top level");
        assert!(
            out.contains("'overwrite' must be a boolean"),
            "expected strict-bool rejection: {out}"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");
    }

    #[test]
    fn overwrite_non_string_content_rejected_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.rs");
        std::fs::write(&p, "old\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
                "overwrite": true,
                "content": 123,
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out =
            tool_write(&args, &session, &bloom).expect("error reported per file, not at top level");
        assert!(
            out.contains("'content' must be a string"),
            "expected content-type rejection: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "old\n",
            "non-string content must not clobber under overwrite: true"
        );
    }

    #[test]
    fn overwrite_missing_content_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.rs");
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "overwrite",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out = tool_write(&args, &session, &bloom).expect("partial-failure returns Ok");
        assert!(
            out.contains("'content' must be a string"),
            "expected content-required rejection: {out}"
        );
        assert!(!p.exists(), "no file created when content missing");
    }

    #[test]
    fn append_echoes_only_appended_region_not_full_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log.txt");
        // Pre-existing 50-line log.
        let mut pre = String::new();
        for n in 1..=50 {
            use std::fmt::Write as _;
            let _ = writeln!(pre, "pre-line-{n}");
        }
        std::fs::write(&p, &pre).unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "append",
                "content": "new1\nnew2\n",
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out = tool_write(&args, &session, &bloom).expect("append succeeds");
        assert!(out.contains("append:"), "missing append verb: {out}");
        assert!(
            out.contains("|new1") && out.contains("|new2"),
            "appended lines must appear in hashline echo: {out}"
        );
        for n in 1..=48 {
            assert!(
                !out.contains(&format!("|pre-line-{n}\n"))
                    && !out.contains(&format!("|pre-line-{n}$"))
                    && !out.contains(&format!("|pre-line-{n} ")),
                "pre-existing line {n} must NOT appear in echo (bounded to appended region): {out}"
            );
        }
        // Echo header reports how much was echoed vs total.
        assert!(
            out.contains("of 52 lines"),
            "echo header should report total line count: {out}"
        );
    }

    #[test]
    fn append_non_string_content_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log.txt");
        std::fs::write(&p, "existing\n").unwrap();
        let args = serde_json::json!({
            "files": [{
                "path": p.to_str().unwrap(),
                "mode": "append",
                "content": null,
            }],
            "scope": dir.path().to_str().unwrap(),
        });
        let (session, bloom) = services();
        let out =
            tool_write(&args, &session, &bloom).expect("error reported per file, not at top level");
        assert!(
            out.contains("'content' must be a string"),
            "expected content-type rejection: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "existing\n",
            "non-string content must not modify the file"
        );
    }
}
