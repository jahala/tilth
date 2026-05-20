use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::error::TilthError;
use crate::format;
use crate::index::bloom::BloomFilterCache;

/// A single edit operation targeting a line range by hash anchors.
#[derive(Debug, Clone)]
pub struct Edit {
    pub start_line: usize,
    pub start_hash: u16,
    pub end_line: usize,
    pub end_hash: u16,
    pub content: String,
}

/// One file's worth of work for a batch `tilth_edit`. Parse errors are deferred
/// onto the task so a malformed entry surfaces as a per-file failure instead of
/// aborting the whole batch.
///
/// `Create` is structurally separate from `Ready` because creating a file has
/// no anchor-and-replace semantics — there's no existing content to hash.
#[derive(Debug)]
pub enum FileEditTask {
    Ready { path: PathBuf, edits: Vec<Edit> },
    Create { path: PathBuf, content: String },
    ParseError { label: String, msg: String },
}

/// Per-edit diff: old lines removed vs new lines added.
#[derive(Debug)]
struct EditDiff {
    /// Original line number (pre-edit) for `-` lines.
    old_start: usize,
    /// Adjusted line number (post-edit) for `+` lines.
    new_start: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

/// Result of applying edits to a file. Internal — callers go through
/// [`apply_batch`], which renders the per-file outcome to a Markdown section.
#[derive(Debug)]
enum EditResult {
    /// All edits applied successfully.
    Applied {
        /// Compact diff showing `-`/`+` lines per edit site.
        diff: String,
        /// Hashlined context around edit sites (existing behavior).
        context: String,
    },
    /// One or more hashes didn't match current content.
    HashMismatch(String),
}

/// Apply a batch of edits to a file.
///
/// 1. Read file into lines
/// 2. Verify ALL hashes before applying ANY edit (fail-fast)
/// 3. Sort edits by `start_line` descending (reverse preserves line numbers)
/// 4. Splice replacements
/// 5. Write file
/// 6. Return hashlined context around edit sites
fn apply_edits(path: &Path, edits: &[Edit]) -> Result<EditResult, TilthError> {
    if edits.is_empty() {
        return Ok(EditResult::Applied {
            diff: String::new(),
            context: String::new(),
        });
    }

    // Read file
    let content = fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => TilthError::NotFound {
            path: path.to_path_buf(),
            suggestion: None,
        },
        std::io::ErrorKind::PermissionDenied => TilthError::PermissionDenied {
            path: path.to_path_buf(),
        },
        _ => TilthError::IoError {
            path: path.to_path_buf(),
            source: e,
        },
    })?;

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    // Phase 1: Verify all hashes
    let mut mismatches: Vec<String> = Vec::new();

    for edit in edits {
        // Bounds check
        if edit.start_line < 1 || edit.start_line > total {
            mismatches.push(format!(
                "Line {} out of bounds (file has {} lines)",
                edit.start_line, total
            ));
            continue;
        }
        if edit.end_line < 1 || edit.end_line > total {
            mismatches.push(format!(
                "Line {} out of bounds (file has {} lines)",
                edit.end_line, total
            ));
            continue;
        }
        if edit.end_line < edit.start_line {
            mismatches.push(format!(
                "Invalid range: {}-{} (end < start)",
                edit.start_line, edit.end_line
            ));
            continue;
        }

        // Verify start hash
        let start_idx = edit.start_line - 1;
        let start_actual_hash = format::line_hash(lines[start_idx].as_bytes());
        if start_actual_hash != edit.start_hash {
            let context_start = start_idx.saturating_sub(2);
            let context_end = (start_idx + 3).min(total);
            let context_lines: String = lines[context_start..context_end].join("\n");
            let hashlined = format::hashlines(&context_lines, (context_start + 1) as u32);
            mismatches.push(format!(
                "Hash mismatch at line {} (expected {:03x}, got {:03x}):\n{}",
                edit.start_line, edit.start_hash, start_actual_hash, hashlined
            ));
            continue;
        }

        // Verify end hash if different line
        if edit.end_line != edit.start_line {
            let end_idx = edit.end_line - 1;
            let end_actual_hash = format::line_hash(lines[end_idx].as_bytes());
            if end_actual_hash != edit.end_hash {
                let context_start = end_idx.saturating_sub(2);
                let context_end = (end_idx + 3).min(total);
                let context_lines: String = lines[context_start..context_end].join("\n");
                let hashlined = format::hashlines(&context_lines, (context_start + 1) as u32);
                mismatches.push(format!(
                    "Hash mismatch at line {} (expected {:03x}, got {:03x}):\n{}",
                    edit.end_line, edit.end_hash, end_actual_hash, hashlined
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        return Ok(EditResult::HashMismatch(mismatches.join("\n\n")));
    }

    // Check for overlapping ranges
    let mut range_check: Vec<(usize, usize)> =
        edits.iter().map(|e| (e.start_line, e.end_line)).collect();
    range_check.sort_by_key(|&(s, _)| s);
    for pair in range_check.windows(2) {
        if pair[0].1 >= pair[1].0 {
            return Err(TilthError::InvalidQuery {
                query: format!(
                    "lines {}-{} and {}-{}",
                    pair[0].0, pair[0].1, pair[1].0, pair[1].1
                ),
                reason: "overlapping edit ranges in batch".into(),
            });
        }
    }

    // Capture old lines for each edit before applying (for diff output).
    // Ordered by edit index so we can zip with edits later.
    let old_snapshots: Vec<Vec<String>> = edits
        .iter()
        .map(|edit| {
            let start_idx = edit.start_line - 1;
            let end_idx = edit.end_line;
            lines[start_idx..end_idx]
                .iter()
                .map(|&s| s.to_string())
                .collect()
        })
        .collect();

    // Phase 2: Apply edits in reverse order
    let mut indices: Vec<usize> = (0..edits.len()).collect();
    indices.sort_by_key(|&i| std::cmp::Reverse(edits[i].start_line));

    let mut owned: Vec<String> = lines.iter().map(|&s| s.to_string()).collect();

    for &idx in &indices {
        let edit = &edits[idx];
        let start_idx = edit.start_line - 1;
        let end_idx = edit.end_line; // exclusive end for inclusive range

        let replacement: Vec<String> = if edit.content.is_empty() {
            vec![]
        } else {
            edit.content.lines().map(String::from).collect()
        };

        owned.splice(start_idx..end_idx, replacement);
    }

    // Phase 3: Write file, preserving original line ending style
    let line_sep = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let has_trailing_newline = content.ends_with('\n');
    let mut output = owned.join(line_sep);
    if has_trailing_newline {
        output.push_str(line_sep);
    }

    fs::write(path, &output).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Phase 4: Build diffs and context around each edit site.
    // Process edits in start_line order. Track cumulative offset since
    // earlier edits shift later line numbers.
    let mut ctx_order: Vec<usize> = (0..edits.len()).collect();
    ctx_order.sort_by_key(|&i| edits[i].start_line);

    let mut offset: isize = 0;
    let mut contexts: Vec<String> = Vec::new();
    let mut diffs: Vec<EditDiff> = Vec::with_capacity(edits.len());

    for &idx in &ctx_order {
        let edit = &edits[idx];
        let adjusted = ((edit.start_line as isize - 1) + offset).max(0) as usize;
        let old_count = edit.end_line - edit.start_line + 1;
        let new_lines: Vec<String> = if edit.content.is_empty() {
            vec![]
        } else {
            edit.content.lines().map(String::from).collect()
        };
        let new_count = new_lines.len();

        // Collect diff data. `-` lines use original positions; `+` lines use
        // offset-adjusted positions so line numbers match the written file.
        let new_start = (adjusted + 1).max(1);
        diffs.push(EditDiff {
            old_start: edit.start_line,
            new_start,
            old_lines: old_snapshots[idx].clone(),
            new_lines,
        });

        // Build hashlined context (existing behavior)
        let context_start = adjusted.saturating_sub(5);
        let context_end = (adjusted + new_count + 5).min(owned.len());
        if context_start < context_end {
            let context_lines: String = owned[context_start..context_end].join("\n");
            let hashlined = format::hashlines(&context_lines, (context_start + 1) as u32);
            contexts.push(hashlined);
        }

        offset += new_count as isize - old_count as isize;
    }

    let diff = format_diffs(&diffs);
    let context = contexts.join("\n---\n");

    Ok(EditResult::Applied { diff, context })
}

/// Create a new file with the given content.
///
/// Atomic via `OpenOptions::create_new(true)` (OS-level `O_CREAT|O_EXCL`) — if
/// the file already exists the OS rejects the open before any bytes are
/// written, eliminating the read-then-write TOCTOU window. Two concurrent
/// callers racing the same path produce exactly one winner; the other gets a
/// clean `AlreadyExists` error.
///
/// Parent directories are auto-created with `fs::create_dir_all`, which is
/// idempotent if the directory already exists.
///
/// Returns an `EditResult::Applied` with:
///   - `diff`: a single-line `[+]` marker so the per-file response makes the
///     create operation visible. Not a multi-line `+` block because the whole
///     file is new — the agent gets the full content via `context` instead.
///   - `context`: hashlined content of the new file, so the agent can drive a
///     follow-up `tilth_edit` against the file without first calling
///     `tilth_read`.
fn create_file(path: &Path, content: &str) -> Result<EditResult, TilthError> {
    // Auto-create parent dirs. `parent()` of `"new.rs"` is `Some("")`, which
    // `create_dir_all` would error on, so skip when empty.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| TilthError::IoError {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }

    // Atomic O_CREAT|O_EXCL — fails loudly with ErrorKind::AlreadyExists if
    // the file already exists. No silent overwrite. On Linux/macOS this is
    // O_EXCL semantics (does NOT follow symlinks — a symlink at `path` causes
    // EEXIST, mapped to AlreadyExists); on Windows it's CREATE_NEW with the
    // same atomicity.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => TilthError::AlreadyExists {
                path: path.to_path_buf(),
            },
            std::io::ErrorKind::PermissionDenied => TilthError::PermissionDenied {
                path: path.to_path_buf(),
            },
            _ => TilthError::IoError {
                path: path.to_path_buf(),
                source: e,
            },
        })?;

    file.write_all(content.as_bytes())
        .map_err(|e| TilthError::IoError {
            path: path.to_path_buf(),
            source: e,
        })?;

    let context = crate::format::hashlines(content, 1);
    Ok(EditResult::Applied {
        diff: "[+] (file created)".into(),
        context,
    })
}

/// Format per-edit diffs as compact `-`/`+` blocks with hashline anchors.
fn format_diffs(diffs: &[EditDiff]) -> String {
    if diffs.is_empty() {
        return String::new();
    }

    let mut out = String::from("\u{2500}\u{2500} diff \u{2500}\u{2500}");

    for (i, d) in diffs.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        } else {
            out.push('\n');
        }

        // Header: line range (uses original positions for orientation)
        let old_end = d.old_start + d.old_lines.len().saturating_sub(1);
        if d.old_lines.len() <= 1 && d.new_lines.len() <= 1 {
            let _ = write!(out, ":{}", d.old_start);
        } else {
            let new_end = d.new_start + d.new_lines.len().saturating_sub(1);
            let end = old_end.max(new_end);
            let _ = write!(out, ":{}-{}", d.old_start, end);
        }

        // Removed lines with hashline anchors (original line numbers)
        for (j, line) in d.old_lines.iter().enumerate() {
            let num = d.old_start + j;
            let hash = format::line_hash(line.as_bytes());
            let _ = write!(out, "\n- {num}:{hash:03x}|{line}");
        }

        // Added lines with hashline anchors (post-edit line numbers)
        for (j, line) in d.new_lines.iter().enumerate() {
            let num = d.new_start + j;
            let hash = format::line_hash(line.as_bytes());
            let _ = write!(out, "\n+ {num}:{hash:03x}|{line}");
        }
    }

    out
}

/// Build a stable dedup key for a path. Canonicalise first (resolves symlinks
/// and `.`/`..` when the file exists), fall back to a lexical normalization
/// (strips `CurDir` components, walks `ParentDir` against the in-memory
/// stack — catches not-yet-created aliases like `new.rs` vs `./new.rs`)
/// then to the raw path. On macOS (commonly case-insensitive APFS) the key
/// is ASCII-lowercased so `Foo.rs` and `FOO.RS` collide; false-positive
/// collisions on case-sensitive APFS configs are preferred over
/// false-negatives that race two writers against the same inode.
///
/// **No `current_dir()` calls.** `std::path::absolute(p)` was previously
/// used here, but it reads `current_dir()` which is process-global mutable
/// state. Two parallel tests (one of which calls `set_current_dir`) could
/// race against each other and produce different keys for the same path
/// — surfacing as a flaky `dedup_catches_nonexistent_alias_spellings`
/// failure under CI's parallel test runner. Pure-lexical normalization
/// removes the race.
pub(crate) fn normalize_path_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path));
    let key = resolved.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

/// Lexical-only path normalization: skip `CurDir`, walk `ParentDir`
/// against the component stack, leave the rest in order. Does not touch
/// the filesystem or `current_dir()`, so it's deterministic under
/// parallel tests.
///
/// `ParentDir` handling depends on what's already on the stack:
///   * If the last component is a real (`Normal`) name, pop it —
///     `a/../b.rs` collapses to `b.rs`.
///   * If the stack is empty or only contains `..` markers AND the path
///     is relative, push `..` — `../foo.rs` stays `../foo.rs` (else it
///     would collapse to `foo.rs`, which is a different file on disk).
///   * If absolute and at root, `..` is a no-op (Linux semantics:
///     `/.. == /`).
///
/// The result is that two paths produce the same key iff they refer to
/// the same logical target through the lexical lens — `foo.rs` and
/// `./foo.rs` collide; `a/../b.rs` and `b.rs` collide; **`../foo.rs`
/// and `foo.rs` do NOT collide** (different parent dirs).
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut is_absolute = false;
    // Count of `Normal` segments currently on the stack. Lets us decide
    // in O(1) whether `..` can pop something real (vs. needing to be
    // preserved as an unresolved `..` in a relative path).
    let mut normal_count: usize = 0;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                is_absolute = true;
                out.push(component.as_os_str());
            }
            Component::Normal(_) => {
                out.push(component.as_os_str());
                normal_count += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if normal_count > 0 {
                    out.pop();
                    normal_count -= 1;
                } else if !is_absolute {
                    // Preserve unresolved `..` in relative paths.
                    out.push("..");
                }
                // Absolute path with `..` at root → no-op.
            }
        }
    }
    out
}

/// Return an error if any two `Ready` tasks resolve to the same file. Called
/// from `apply_batch` before any worker starts so the invariant lives with
/// the code that depends on it — two rayon workers racing `fs::write` against
/// the same inode would silently lose an edit.
pub(crate) fn detect_duplicate_paths(tasks: &[FileEditTask]) -> Option<String> {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for task in tasks {
        let path = match task {
            FileEditTask::Ready { path, .. } | FileEditTask::Create { path, .. } => path,
            FileEditTask::ParseError { .. } => continue,
        };
        if !seen.insert(normalize_path_key(path)) {
            return Some(format!(
                "duplicate file path in batch: {} — group all edits for a file under one entry",
                path.display()
            ));
        }
    }
    None
}

/// Apply a batch of file edits in parallel.
///
/// Each task is processed independently — a hash mismatch, parse error, or
/// I/O failure on one file does not block siblings. Output is a series of
/// `## <path>` sections joined by `---`. Returns `Err` only when every file
/// failed (so the MCP response sets `isError: true`). Output ordering
/// matches the input `tasks` — rayon's `par_iter().collect()` preserves
/// index order even though execution order is not deterministic.
///
/// Rejects the whole batch up front when two tasks (Ready or Create)
/// resolve to the same canonical path so workers cannot race writes against
/// the same file.
pub fn apply_batch(
    tasks: Vec<FileEditTask>,
    bloom: &Arc<BloomFilterCache>,
    show_diff: bool,
) -> Result<String, String> {
    if let Some(msg) = detect_duplicate_paths(&tasks) {
        return Err(msg);
    }

    let bloom: &BloomFilterCache = bloom;
    let outcomes: Vec<(String, bool)> = tasks
        .into_par_iter()
        .map(|task| apply_one(task, bloom, show_diff))
        .collect();

    let any_success = outcomes.iter().any(|(_, ok)| *ok);
    let combined = outcomes
        .into_iter()
        .map(|(s, _)| s)
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    if any_success {
        Ok(combined)
    } else {
        Err(combined)
    }
}

/// Process one task into a `(section, success)` tuple. Kept separate so the
/// parallel closure stays trivial and per-file logic is testable in isolation.
fn apply_one(task: FileEditTask, bloom: &BloomFilterCache, show_diff: bool) -> (String, bool) {
    match task {
        FileEditTask::ParseError { label, msg } => (format!("## {label}\nerror: {msg}"), false),
        FileEditTask::Ready { path, edits } => {
            let header = format!("## {}", path.display());
            match render_applied(&path, &edits, bloom, show_diff) {
                Ok(body) if body.is_empty() => (header, true),
                Ok(body) => (format!("{header}\n{body}"), true),
                Err(msg) => (format!("{header}\n{msg}"), false),
            }
        }
        FileEditTask::Create { path, content } => {
            let header = format!("## {}", path.display());
            match render_created(&path, &content, show_diff) {
                Ok(body) if body.is_empty() => (header, true),
                Ok(body) => (format!("{header}\n{body}"), true),
                Err(msg) => (format!("{header}\n{msg}"), false),
            }
        }
    }
}

/// Wrap [`create_file`] with the same `(diff?, context)` rendering as
/// [`render_applied`] so the per-file Markdown section reads consistently
/// across edit and create operations.
///
/// The `[+] (file created)` marker is emitted unconditionally — unlike
/// `render_applied`'s diff (gated on `show_diff`), the create marker is the
/// only confirmation the operation ran. An empty-content (`touch`) create
/// would otherwise produce a header-only response indistinguishable from a
/// no-op.
///
/// No blast-radius check: a brand-new file has no existing callers to
/// inform. [`render_applied`] runs blast-radius after applying anchor edits;
/// the equivalent for Create is a no-op by construction.
fn render_created(path: &Path, content: &str, show_diff: bool) -> Result<String, String> {
    match create_file(path, content).map_err(|e| e.to_string())? {
        EditResult::Applied { diff, context } => {
            let mut output = String::new();
            output.push_str(&diff);
            if !content.is_empty() {
                output.push('\n');
                output.push_str(&context);
            }
            let _ = show_diff; // Marker is unconditional; flag is accepted for API symmetry.
            Ok(output)
        }
        EditResult::HashMismatch(_) => {
            unreachable!("create_file never returns HashMismatch — there are no hashes to mismatch")
        }
    }
}

fn render_applied(
    path: &Path,
    edits: &[Edit],
    bloom: &BloomFilterCache,
    show_diff: bool,
) -> Result<String, String> {
    match apply_edits(path, edits).map_err(|e| e.to_string())? {
        EditResult::Applied { diff, context } => {
            let mut output = String::new();
            if show_diff && !diff.is_empty() {
                output.push_str(&diff);
                if !context.is_empty() {
                    output.push_str("\n\n");
                }
            }
            if !context.is_empty() {
                output.push_str(&context);
            }
            let abs_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let scope = crate::lang::package_root(&abs_path).map_or_else(
                || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                Path::to_path_buf,
            );
            if let Some(blast) = crate::search::blast::blast_radius(path, edits, &scope, bloom) {
                output.push_str(&blast);
            }
            Ok(output)
        }
        EditResult::HashMismatch(msg) => Err(format!(
            "hash mismatch — file changed since last read:\n\n{msg}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("tilth_edit_test_{name}"));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn hash_at(content: &str, line: usize) -> u16 {
        let lines: Vec<&str> = content.lines().collect();
        format::line_hash(lines[line - 1].as_bytes())
    }

    #[test]
    fn single_line_replacement() {
        let content = "aaa\nbbb\nccc\n";
        let path = write_temp("single", content);
        let h = hash_at(content, 2);

        let edits = vec![Edit {
            start_line: 2,
            start_hash: h,
            end_line: 2,
            end_hash: h,
            content: "BBB".into(),
        }];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, context } => {
                assert!(
                    diff.contains("- 2:"),
                    "diff should have removed line: {diff}"
                );
                assert!(diff.contains("+ 2:"), "diff should have added line: {diff}");
                assert!(
                    diff.contains("|bbb"),
                    "diff should show old content: {diff}"
                );
                assert!(
                    diff.contains("|BBB"),
                    "diff should show new content: {diff}"
                );
                assert!(!context.is_empty(), "context should not be empty");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn multi_line_replacement_fewer_lines() {
        let content = "aaa\nbbb\nccc\nddd\n";
        let path = write_temp("fewer", content);
        let h2 = hash_at(content, 2);
        let h3 = hash_at(content, 3);

        let edits = vec![Edit {
            start_line: 2,
            start_hash: h2,
            end_line: 3,
            end_hash: h3,
            content: "XYZ".into(),
        }];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                assert!(diff.contains("|bbb"), "old line 2: {diff}");
                assert!(diff.contains("|ccc"), "old line 3: {diff}");
                assert!(diff.contains("|XYZ"), "new line: {diff}");
                // Should have 2 removed lines and 1 added line
                let minus_count = diff.lines().filter(|l| l.starts_with("- ")).count();
                let plus_count = diff.lines().filter(|l| l.starts_with("+ ")).count();
                assert_eq!(minus_count, 2, "should remove 2 lines");
                assert_eq!(plus_count, 1, "should add 1 line");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn multi_line_replacement_more_lines() {
        let content = "aaa\nbbb\nccc\n";
        let path = write_temp("more", content);
        let h2 = hash_at(content, 2);

        let edits = vec![Edit {
            start_line: 2,
            start_hash: h2,
            end_line: 2,
            end_hash: h2,
            content: "X1\nX2\nX3".into(),
        }];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                let minus_count = diff.lines().filter(|l| l.starts_with("- ")).count();
                let plus_count = diff.lines().filter(|l| l.starts_with("+ ")).count();
                assert_eq!(minus_count, 1, "should remove 1 line");
                assert_eq!(plus_count, 3, "should add 3 lines");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn line_deletion() {
        let content = "aaa\nbbb\nccc\n";
        let path = write_temp("delete", content);
        let h2 = hash_at(content, 2);

        let edits = vec![Edit {
            start_line: 2,
            start_hash: h2,
            end_line: 2,
            end_hash: h2,
            content: String::new(),
        }];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                let minus_count = diff.lines().filter(|l| l.starts_with("- ")).count();
                let plus_count = diff.lines().filter(|l| l.starts_with("+ ")).count();
                assert_eq!(minus_count, 1, "should remove 1 line");
                assert_eq!(plus_count, 0, "should add 0 lines");
                assert!(diff.contains("|bbb"), "should show deleted content: {diff}");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        // Verify file content
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "aaa\nccc\n");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn multiple_edits_batch() {
        let content = "aaa\nbbb\nccc\nddd\neee\n";
        let path = write_temp("batch", content);
        let h1 = hash_at(content, 1);
        let h4 = hash_at(content, 4);

        let edits = vec![
            Edit {
                start_line: 1,
                start_hash: h1,
                end_line: 1,
                end_hash: h1,
                content: "AAA".into(),
            },
            Edit {
                start_line: 4,
                start_hash: h4,
                end_line: 4,
                end_hash: h4,
                content: "DDD".into(),
            },
        ];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                assert!(diff.contains("|aaa"), "should show old line 1: {diff}");
                assert!(diff.contains("|AAA"), "should show new line 1: {diff}");
                assert!(diff.contains("|ddd"), "should show old line 4: {diff}");
                assert!(diff.contains("|DDD"), "should show new line 4: {diff}");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "AAA\nbbb\nccc\nDDD\neee\n");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_edits_no_diff() {
        let content = "aaa\nbbb\n";
        let path = write_temp("empty", content);

        let result = apply_edits(&path, &[]).unwrap();
        match result {
            EditResult::Applied { diff, context } => {
                assert!(diff.is_empty(), "diff should be empty for no edits");
                assert!(context.is_empty(), "context should be empty for no edits");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn diff_header_format() {
        let content = "aaa\nbbb\nccc\n";
        let path = write_temp("header", content);
        let h2 = hash_at(content, 2);

        let edits = vec![Edit {
            start_line: 2,
            start_hash: h2,
            end_line: 2,
            end_hash: h2,
            content: "BBB".into(),
        }];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                assert!(
                    diff.starts_with("\u{2500}\u{2500} diff \u{2500}\u{2500}"),
                    "should start with diff header: {diff}"
                );
                assert!(
                    diff.contains(":2"),
                    "should have line number in header: {diff}"
                );
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unicode_content_in_diff() {
        let content = "hello\n日本語テスト\nworld\n";
        let path = write_temp("unicode", content);
        let h2 = hash_at(content, 2);

        let edits = vec![Edit {
            start_line: 2,
            start_hash: h2,
            end_line: 2,
            end_hash: h2,
            content: "中文测试".into(),
        }];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                assert!(diff.contains("|日本語テスト"), "old unicode: {diff}");
                assert!(diff.contains("|中文测试"), "new unicode: {diff}");
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn batch_edit_offset_line_numbers() {
        // Edit 1 deletes line 2 (shifts subsequent lines up by 1).
        // Edit 2 replaces line 5. In the new file, original line 5 is at line 4.
        // The diff `+` lines for edit 2 should show line 4, not line 5.
        let content = "aaa\nbbb\nccc\nddd\neee\nfff\n";
        let path = write_temp("offset", content);
        let h2 = hash_at(content, 2);
        let h5 = hash_at(content, 5);

        let edits = vec![
            Edit {
                start_line: 2,
                start_hash: h2,
                end_line: 2,
                end_hash: h2,
                content: String::new(), // delete line 2
            },
            Edit {
                start_line: 5,
                start_hash: h5,
                end_line: 5,
                end_hash: h5,
                content: "EEE".into(),
            },
        ];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                // Edit 1: `-` at original line 2, no `+` lines
                assert!(
                    diff.contains("- 2:"),
                    "edit 1 should show removed line 2: {diff}"
                );
                // Edit 2: `-` at original line 5, `+` at adjusted line 4
                assert!(
                    diff.contains("- 5:"),
                    "edit 2 should show removed original line 5: {diff}"
                );
                assert!(
                    diff.contains("+ 4:"),
                    "edit 2 should show added at adjusted line 4: {diff}"
                );
                assert!(
                    diff.contains("|EEE"),
                    "edit 2 should show new content: {diff}"
                );
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        // Verify final file
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "aaa\nccc\nddd\nEEE\nfff\n");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn batch_edit_insertion_shifts_lines() {
        // Edit 1 expands line 1 to 3 lines (+2 offset).
        // Edit 2 replaces line 3. In the new file, original line 3 is at line 5.
        let content = "aaa\nbbb\nccc\nddd\n";
        let path = write_temp("insert_shift", content);
        let h1 = hash_at(content, 1);
        let h3 = hash_at(content, 3);

        let edits = vec![
            Edit {
                start_line: 1,
                start_hash: h1,
                end_line: 1,
                end_hash: h1,
                content: "A1\nA2\nA3".into(), // 1 line -> 3 lines
            },
            Edit {
                start_line: 3,
                start_hash: h3,
                end_line: 3,
                end_hash: h3,
                content: "CCC".into(),
            },
        ];

        let result = apply_edits(&path, &edits).unwrap();
        match result {
            EditResult::Applied { diff, .. } => {
                // Edit 2: `+` should be at adjusted line 5 (original 3 + offset 2)
                assert!(
                    diff.contains("+ 5:"),
                    "edit 2 should show added at adjusted line 5: {diff}"
                );
            }
            EditResult::HashMismatch(msg) => panic!("unexpected mismatch: {msg}"),
        }

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "A1\nA2\nA3\nbbb\nCCC\nddd\n");

        let _ = std::fs::remove_file(&path);
    }

    // ----------------------------------------------------------------- batch

    fn ready_task(path: PathBuf, edits: Vec<Edit>) -> FileEditTask {
        FileEditTask::Ready { path, edits }
    }

    fn fresh_bloom() -> Arc<BloomFilterCache> {
        Arc::new(BloomFilterCache::new())
    }

    #[test]
    fn batch_two_files_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "aaa\nbbb\n").unwrap();
        std::fs::write(&b, "ccc\nddd\n").unwrap();
        let ha = hash_at("aaa\nbbb\n", 1);
        let hb = hash_at("ccc\nddd\n", 2);

        let tasks = vec![
            ready_task(
                a.clone(),
                vec![Edit {
                    start_line: 1,
                    start_hash: ha,
                    end_line: 1,
                    end_hash: ha,
                    content: "AAA".into(),
                }],
            ),
            ready_task(
                b.clone(),
                vec![Edit {
                    start_line: 2,
                    start_hash: hb,
                    end_line: 2,
                    end_hash: hb,
                    content: "DDD".into(),
                }],
            ),
        ];

        let out = apply_batch(tasks, &fresh_bloom(), false).expect("batch should succeed");
        assert!(out.contains(&format!("## {}", a.display())));
        assert!(out.contains(&format!("## {}", b.display())));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "AAA\nbbb\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "ccc\nDDD\n");
    }

    #[test]
    fn batch_partial_failure_does_not_block_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.txt");
        let bad = dir.path().join("bad.txt");
        std::fs::write(&good, "x\n").unwrap();
        std::fs::write(&bad, "y\n").unwrap();
        let h_good = hash_at("x\n", 1);

        let tasks = vec![
            ready_task(
                good.clone(),
                vec![Edit {
                    start_line: 1,
                    start_hash: h_good,
                    end_line: 1,
                    end_hash: h_good,
                    content: "X".into(),
                }],
            ),
            ready_task(
                bad.clone(),
                vec![Edit {
                    start_line: 1,
                    // wrong hash → HashMismatch on this file only
                    start_hash: 0xFFF,
                    end_line: 1,
                    end_hash: 0xFFF,
                    content: "Y".into(),
                }],
            ),
        ];

        let out = apply_batch(tasks, &fresh_bloom(), false).expect("good half succeeded");
        assert!(out.contains("hash mismatch"), "bad file reports mismatch");
        assert!(out.contains(&format!("## {}", bad.display())));
        // good file actually got written
        assert_eq!(std::fs::read_to_string(&good).unwrap(), "X\n");
        // bad file unchanged
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "y\n");
    }

    #[test]
    fn batch_all_failed_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        std::fs::write(&a, "z\n").unwrap();

        let tasks = vec![ready_task(
            a,
            vec![Edit {
                start_line: 1,
                start_hash: 0xABC,
                end_line: 1,
                end_hash: 0xABC,
                content: "Z".into(),
            }],
        )];

        let err = apply_batch(tasks, &fresh_bloom(), false)
            .expect_err("batch with no successes returns Err");
        assert!(err.contains("hash mismatch"));
    }

    #[test]
    fn batch_parse_error_surfaces_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("g.txt");
        std::fs::write(&good, "k\n").unwrap();
        let h = hash_at("k\n", 1);

        let tasks = vec![
            ready_task(
                good.clone(),
                vec![Edit {
                    start_line: 1,
                    start_hash: h,
                    end_line: 1,
                    end_hash: h,
                    content: "K".into(),
                }],
            ),
            FileEditTask::ParseError {
                label: "files[1]".into(),
                msg: "missing 'edits' array".into(),
            },
        ];

        let out =
            apply_batch(tasks, &fresh_bloom(), false).expect("good half kept the batch alive");
        assert!(out.contains("## files[1]"));
        assert!(out.contains("error: missing 'edits' array"));
        assert_eq!(std::fs::read_to_string(&good).unwrap(), "K\n");
    }

    // ------------------------------------------------- dedup

    fn ready_noop(path: PathBuf) -> FileEditTask {
        FileEditTask::Ready {
            path,
            edits: vec![],
        }
    }

    #[test]
    fn dedup_catches_nonexistent_alias_spellings() {
        let tasks = vec![
            ready_noop(PathBuf::from("definitely_nonexistent_dedup_target.rs")),
            ready_noop(PathBuf::from("./definitely_nonexistent_dedup_target.rs")),
        ];
        let err = detect_duplicate_paths(&tasks).expect("alias spellings should collide");
        assert!(err.contains("duplicate file path"), "unexpected: {err}");
    }

    #[test]
    fn dedup_allows_distinct_nonexistent_paths() {
        let tasks = vec![
            ready_noop(PathBuf::from("nonexistent_dedup_a.rs")),
            ready_noop(PathBuf::from("nonexistent_dedup_b.rs")),
        ];
        assert!(detect_duplicate_paths(&tasks).is_none());
    }

    /// Pin the race fix: `normalize_path_key` MUST NOT depend on
    /// `current_dir()`. If it did, this test could flake under parallel
    /// test execution (another test calling `set_current_dir` could
    /// change the result of the second key computation). The dedup
    /// must hold even when cwd changes between the two computations,
    /// which we simulate here by toggling cwd in between.
    ///
    /// Regression: pre-fix this used `std::path::absolute()` whose
    /// behavior depends on `current_dir()`; the test was flaky on
    /// Linux CI because `mcp::tests::scope_handoff_when_cwd_is_root`
    /// runs in parallel and calls `set_current_dir("/")`.
    #[test]
    fn normalize_path_key_is_cwd_independent() {
        let key_a = normalize_path_key(Path::new("foo.rs"));
        let key_b = normalize_path_key(Path::new("./foo.rs"));
        assert_eq!(
            key_a, key_b,
            "foo.rs and ./foo.rs must normalize identically"
        );

        // `a/../b.rs` should resolve lexically to `b.rs` — same key as `b.rs`.
        let key_c = normalize_path_key(Path::new("a/../b.rs"));
        let key_d = normalize_path_key(Path::new("b.rs"));
        assert_eq!(
            key_c, key_d,
            "a/../b.rs and b.rs must normalize identically"
        );
    }

    /// Lexical normalization unit tests — these are the predicates the
    /// `normalize_path_key_is_cwd_independent` test pins as a guarantee.
    #[test]
    fn lexical_normalize_strips_curdir() {
        assert_eq!(
            lexical_normalize(Path::new("./foo.rs")),
            PathBuf::from("foo.rs")
        );
        assert_eq!(
            lexical_normalize(Path::new("a/./b/./c.rs")),
            PathBuf::from("a/b/c.rs")
        );
    }

    #[test]
    fn lexical_normalize_pops_on_parentdir() {
        assert_eq!(
            lexical_normalize(Path::new("a/../b.rs")),
            PathBuf::from("b.rs")
        );
        assert_eq!(
            lexical_normalize(Path::new("a/b/../../c.rs")),
            PathBuf::from("c.rs")
        );
    }

    #[test]
    fn lexical_normalize_preserves_absolute() {
        assert_eq!(
            lexical_normalize(Path::new("/abs/./foo.rs")),
            PathBuf::from("/abs/foo.rs")
        );
        assert_eq!(
            lexical_normalize(Path::new("/foo/../bar.rs")),
            PathBuf::from("/bar.rs")
        );
        // `..` at absolute root is a no-op (Linux: /.. == /).
        assert_eq!(lexical_normalize(Path::new("/..")), PathBuf::from("/"));
    }

    /// `../foo.rs` and `foo.rs` refer to DIFFERENT files (one in parent
    /// dir, one in cwd). The dedup must NOT collide them. Tests the
    /// fix to v1 of `lexical_normalize` which mistakenly popped at empty
    /// stack and collapsed `../foo.rs` → `foo.rs`.
    #[test]
    fn lexical_normalize_preserves_unresolved_parentdir() {
        assert_eq!(
            lexical_normalize(Path::new("../foo.rs")),
            PathBuf::from("../foo.rs")
        );
        // Multi-level: foo/bar/../../../baz.rs → ../baz.rs
        // (foo → +foo, bar → +bar, .. → pop bar, .. → pop foo, .. → push .., baz.rs → +baz.rs)
        assert_eq!(
            lexical_normalize(Path::new("foo/bar/../../../baz.rs")),
            PathBuf::from("../baz.rs")
        );
        // ../foo.rs and foo.rs must NOT produce equal keys.
        assert_ne!(
            normalize_path_key(Path::new("../foo.rs")),
            normalize_path_key(Path::new("foo.rs"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dedup_catches_case_aliases_on_macos() {
        // Case-insensitive APFS resolves Foo.rs and FOO.RS to the same inode.
        // normalize_path_key ASCII-lowercases on macOS so the keys collide
        // before two workers can race writes.
        let tasks = vec![
            ready_noop(PathBuf::from("nonexistent_case_target.rs")),
            ready_noop(PathBuf::from("NONEXISTENT_CASE_TARGET.RS")),
        ];
        let err = detect_duplicate_paths(&tasks).expect("case aliases should collide on macOS");
        assert!(err.contains("duplicate file path"), "unexpected: {err}");
    }

    #[test]
    fn apply_batch_rejects_duplicate_paths() {
        // The dedup gate lives inside apply_batch — exercise the integration
        // path so the invariant is locked even if a future caller bypasses
        // the MCP wire layer.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("dup.txt");
        std::fs::write(&a, "x\n").unwrap();
        let h = hash_at("x\n", 1);

        let make_edit = || Edit {
            start_line: 1,
            start_hash: h,
            end_line: 1,
            end_hash: h,
            content: "X".into(),
        };

        let tasks = vec![
            ready_task(a.clone(), vec![make_edit()]),
            ready_task(a.clone(), vec![make_edit()]),
        ];

        let err = apply_batch(tasks, &fresh_bloom(), false)
            .expect_err("duplicate paths must reject the batch");
        assert!(err.contains("duplicate file path"), "unexpected: {err}");
        // File must be untouched — no worker ran.
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "x\n");
    }

    // ── create_file ────────────────────────────────────────────────────────

    #[test]
    fn create_file_writes_content_to_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.rs");
        let content = "fn main() {}\n";

        let result = create_file(&path, content).expect("create should succeed");
        match result {
            EditResult::Applied { diff, context } => {
                assert!(diff.contains("[+]"), "diff should mark create: {diff}");
                assert!(
                    context.contains("|fn main() {}"),
                    "context must hashline the new content: {context}"
                );
            }
            EditResult::HashMismatch(_) => panic!("create cannot produce HashMismatch"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn create_file_fails_when_path_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exists.rs");
        std::fs::write(&path, "original").unwrap();

        let err =
            create_file(&path, "overwrite attempt").expect_err("create on existing path must fail");
        assert!(
            matches!(err, TilthError::AlreadyExists { .. }),
            "expected AlreadyExists, got: {err:?}"
        );
        // Original content untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    #[cfg(unix)]
    fn create_file_fails_when_path_is_dangling_symlink() {
        // On Linux/macOS, O_CREAT|O_EXCL does NOT follow symlinks — a symlink
        // at the target path causes EEXIST (the symlink itself exists). The
        // error surfaces as AlreadyExists, matching the regular-file collision
        // case so the agent gets a consistent typed error.
        let dir = tempfile::tempdir().unwrap();
        let dangling_target = dir.path().join("does_not_exist.rs");
        let symlink_path = dir.path().join("link.rs");
        std::os::unix::fs::symlink(&dangling_target, &symlink_path).unwrap();

        let err = create_file(&symlink_path, "via dangling symlink")
            .expect_err("create through dangling symlink must fail atomically");
        assert!(
            matches!(err, TilthError::AlreadyExists { .. }),
            "expected AlreadyExists for dangling symlink, got: {err:?}"
        );
        // The dangling target must not have been created.
        assert!(!dangling_target.exists());
    }

    #[test]
    fn create_file_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/deep.rs");
        assert!(
            !path.parent().unwrap().exists(),
            "parent must not pre-exist"
        );

        create_file(&path, "deep\n").expect("create should auto-mkdir");
        assert!(path.exists());
        assert!(path.parent().unwrap().is_dir());
    }

    #[test]
    fn create_file_accepts_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("touched");

        create_file(&path, "").expect("empty content = touch");
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 0);
    }

    #[test]
    fn create_file_returns_hashlined_output_for_followup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("followup.rs");
        let content = "line one\nline two\n";

        let result = create_file(&path, content).expect("create should succeed");
        let context = match result {
            EditResult::Applied { context, .. } => context,
            EditResult::HashMismatch(_) => panic!("unreachable"),
        };

        // The hashlined context must contain anchors that an immediate
        // follow-up tilth_edit could parse via format::parse_anchor.
        let first_line = context.lines().next().expect("at least one hashline");
        let anchor_end = first_line
            .find('|')
            .expect("hashline format is `<n>:<hash>|<content>`");
        let anchor = &first_line[..anchor_end];
        let (line_num, hash) = format::parse_anchor(anchor)
            .expect("create's context must be parseable as an edit anchor");
        assert_eq!(line_num, 1, "first hashline starts at line 1");
        assert_eq!(hash, format::line_hash(b"line one"));
    }

    #[test]
    #[cfg(unix)]
    fn create_file_rejects_permission_denied_as_typed_error() {
        // Verifies the io::Error → TilthError mapping: PermissionDenied
        // ErrorKind becomes TilthError::PermissionDenied (not IoError).
        // Induces real PermissionDenied via chmod inside a tempdir — Unix-only
        // because Windows' permission model is not chmod-based.
        let dir = tempfile::tempdir().unwrap();
        let readonly_dir = dir.path().join("ro");
        std::fs::create_dir(&readonly_dir).unwrap();
        let mut perms = std::fs::metadata(&readonly_dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
        std::fs::set_permissions(&readonly_dir, perms).unwrap();

        let path = readonly_dir.join("blocked.rs");
        let err = create_file(&path, "x").expect_err("readonly parent must reject");
        assert!(
            matches!(err, TilthError::PermissionDenied { .. }),
            "expected PermissionDenied, got: {err:?}"
        );

        // Restore perms so the tempdir cleanup can remove the directory.
        let mut perms = std::fs::metadata(&readonly_dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&readonly_dir, perms).unwrap();
    }

    // ── apply_batch integration with Create ───────────────────────────────

    #[test]
    fn batch_mixes_create_and_replace_in_one_call() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing.txt");
        std::fs::write(&existing, "old\n").unwrap();
        let h = hash_at("old\n", 1);

        let new_path = dir.path().join("new.txt");

        let tasks = vec![
            ready_task(
                existing.clone(),
                vec![Edit {
                    start_line: 1,
                    start_hash: h,
                    end_line: 1,
                    end_hash: h,
                    content: "NEW".into(),
                }],
            ),
            FileEditTask::Create {
                path: new_path.clone(),
                content: "created\n".into(),
            },
        ];

        let output = apply_batch(tasks, &fresh_bloom(), false)
            .expect("mixed batch should succeed when both files succeed");
        assert!(output.contains("existing.txt"));
        assert!(output.contains("new.txt"));
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "NEW\n");
        assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "created\n");
    }

    #[test]
    fn batch_continues_when_one_create_already_exists() {
        // Best-effort semantic: a create that fails because the file exists
        // must not block sibling files in the same batch.
        let dir = tempfile::tempdir().unwrap();
        let preexisting = dir.path().join("collision.txt");
        std::fs::write(&preexisting, "original\n").unwrap();

        let fresh = dir.path().join("ok.txt");

        let tasks = vec![
            FileEditTask::Create {
                path: preexisting.clone(),
                content: "would overwrite".into(),
            },
            FileEditTask::Create {
                path: fresh.clone(),
                content: "this one succeeds\n".into(),
            },
        ];

        let output = apply_batch(tasks, &fresh_bloom(), false)
            .expect("at least one success should yield Ok");
        // First file: original untouched.
        assert_eq!(std::fs::read_to_string(&preexisting).unwrap(), "original\n");
        // Second file: created.
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "this one succeeds\n"
        );
        // Per-file sections must mention both paths.
        assert!(output.contains("collision.txt"));
        assert!(output.contains("ok.txt"));
    }

    #[test]
    fn duplicate_create_paths_rejected_before_dispatch() {
        // The dedup gate must cover Create variants — racing two creates of
        // the same path inside one batch would otherwise rely on O_EXCL's
        // arbitration, and the loser's error message would be confusing.
        // Reject the whole batch up front instead.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("twice.rs");

        let tasks = vec![
            FileEditTask::Create {
                path: path.clone(),
                content: "first".into(),
            },
            FileEditTask::Create {
                path: path.clone(),
                content: "second".into(),
            },
        ];

        let err = apply_batch(tasks, &fresh_bloom(), false)
            .expect_err("duplicate Create paths must reject");
        assert!(err.contains("duplicate file path"), "unexpected: {err}");
        assert!(!path.exists(), "no worker should have run");
    }
}
