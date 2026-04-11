use std::path::PathBuf;

use super::{DiffLine, DiffLineKind, FileDiff, FileStatus, Hunk};
use crate::lang::detection::is_generated_by_name;

/// Parse `git diff --no-color` unified diff output into a list of `FileDiff`s.
pub(crate) fn parse_unified_diff(raw: &str) -> Vec<FileDiff> {
    if raw.is_empty() {
        return Vec::new();
    }

    let mut results: Vec<FileDiff> = Vec::new();

    // Per-file state.
    let mut path: Option<PathBuf> = None;
    let mut old_path: Option<PathBuf> = None;
    let mut status = FileStatus::Modified;
    let mut is_binary = false;
    let mut hunks: Vec<Hunk> = Vec::new();

    // Per-hunk state.
    let mut current_hunk: Option<Hunk> = None;

    // Commit the current in-progress file (if any) to `results`.
    macro_rules! flush_file {
        () => {
            if let Some(p) = path.take() {
                if let Some(h) = current_hunk.take() {
                    hunks.push(h);
                }
                let generated = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(is_generated_by_name)
                    .unwrap_or(false);
                results.push(FileDiff {
                    path: p,
                    old_path: old_path.take(),
                    status,
                    hunks: std::mem::take(&mut hunks),
                    is_generated: generated,
                    is_binary,
                });
            }
        };
    }

    for line in raw.lines() {
        // ── New file header ─────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_file!();
            // Reset per-file state for the new file.
            is_binary = false;
            status = FileStatus::Modified;
            // Extract the `b/` path (right-hand side) which is canonical.
            // Format: `diff --git a/<path> b/<path>`
            // Paths may contain spaces; split at " b/" from the right.
            if let Some(b_pos) = rest.rfind(" b/") {
                let b_part = &rest[b_pos + 3..]; // skip " b/"
                path = Some(PathBuf::from(b_part));
            } else {
                // Fallback: take everything after "a/"
                path = rest
                    .strip_prefix("a/")
                    .map(PathBuf::from)
                    .or_else(|| Some(PathBuf::from(rest)));
            }
            continue;
        }

        // ── Rename markers ───────────────────────────────────────────────────
        if let Some(from) = line.strip_prefix("rename from ") {
            status = FileStatus::Renamed;
            old_path = Some(PathBuf::from(from.trim()));
            continue;
        }
        if line.starts_with("rename to ") || line.starts_with("similarity index ") {
            // `rename to` confirms the new path (already captured from `b/` above).
            // `similarity index` is metadata — skip.
            continue;
        }

        // ── Binary marker ────────────────────────────────────────────────────
        if line.starts_with("Binary files ") && line.ends_with("differ") {
            is_binary = true;
            continue;
        }

        // ── `--- a/` / `+++ b/` ──────────────────────────────────────────────
        if let Some(src) = line.strip_prefix("--- ") {
            if src == "/dev/null" {
                status = FileStatus::Added;
            }
            // Otherwise it's just the old path header — already captured from diff --git.
            continue;
        }
        if let Some(dst) = line.strip_prefix("+++ ") {
            if dst == "/dev/null" {
                status = FileStatus::Deleted;
            }
            continue;
        }

        // ── Hunk header ──────────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("@@ ") {
            // Flush previous hunk.
            if let Some(h) = current_hunk.take() {
                hunks.push(h);
            }
            // Parse `@@ -old_start[,old_count] +new_start[,new_count] @@`
            let (old_start, old_count, new_start, new_count) = parse_hunk_header(rest);
            current_hunk = Some(Hunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
            continue;
        }

        // ── Diff content lines ───────────────────────────────────────────────
        if let Some(hunk) = current_hunk.as_mut() {
            if let Some(content) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    content: content.to_owned(),
                });
            } else if let Some(content) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine {
                    kind: DiffLineKind::Removed,
                    content: content.to_owned(),
                });
            } else if let Some(content) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine {
                    kind: DiffLineKind::Context,
                    content: content.to_owned(),
                });
            } else if line.starts_with('\\') {
                // `\ No newline at end of file` — skip silently.
            }
            // Any other line inside a hunk (e.g. empty) — ignore.
        }
        // Lines before the first hunk (index lines, mode changes, etc.) — ignore.
    }

    // Flush the final file.
    flush_file!();

    results
}

/// Parse `@@ -old_start[,old_count] +new_start[,new_count] @@ ...` and return
/// `(old_start, old_count, new_start, new_count)`.
///
/// Omitted counts default to 1.
fn parse_hunk_header(rest: &str) -> (u32, u32, u32, u32) {
    // `rest` is everything after the opening `@@ `, e.g.:
    //   `-3,7 +3,8 @@ fn foo()`
    // Find the closing `@@` to isolate the range part.
    let range_part = if let Some(pos) = rest.find(" @@") {
        &rest[..pos]
    } else {
        rest.trim_end_matches('@').trim()
    };

    let mut parts = range_part.split_whitespace();
    let old_spec = parts.next().unwrap_or("-0");
    let new_spec = parts.next().unwrap_or("+0");

    let (old_start, old_count) = parse_range_spec(old_spec.trim_start_matches('-'));
    let (new_start, new_count) = parse_range_spec(new_spec.trim_start_matches('+'));

    (old_start, old_count, new_start, new_count)
}

/// Parse `start` or `start,count` — omitted count defaults to 1.
fn parse_range_spec(spec: &str) -> (u32, u32) {
    if let Some((s, c)) = spec.split_once(',') {
        let start = s.parse().unwrap_or(0);
        let count = c.parse().unwrap_or(1);
        (start, count)
    } else {
        let start = spec.parse().unwrap_or(0);
        (start, 1)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── 1. Basic modified file ────────────────────────────────────────────────
    #[test]
    fn test_basic_modified_file() {
        let raw = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"world\");
+    println!(\"extra\");
 }
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let f = &diffs[0];
        assert_eq!(f.path, PathBuf::from("src/main.rs"));
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.old_count, 3);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.new_count, 4);
        assert_eq!(h.lines[0].kind, DiffLineKind::Context);
        assert_eq!(h.lines[1].kind, DiffLineKind::Removed);
        assert_eq!(h.lines[2].kind, DiffLineKind::Added);
        assert_eq!(h.lines[3].kind, DiffLineKind::Added);
        assert_eq!(h.lines[4].kind, DiffLineKind::Context);
    }

    // ── 2. Added file ─────────────────────────────────────────────────────────
    #[test]
    fn test_added_file() {
        let raw = "\
diff --git a/new.rs b/new.rs
new file mode 100644
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+fn new() {}
+fn other() {}
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let f = &diffs[0];
        assert_eq!(f.status, FileStatus::Added);
        assert!(f.hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == DiffLineKind::Added));
    }

    // ── 3. Deleted file ───────────────────────────────────────────────────────
    #[test]
    fn test_deleted_file() {
        let raw = "\
diff --git a/old.rs b/old.rs
deleted file mode 100644
--- a/old.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn gone() {}
-fn also_gone() {}
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let f = &diffs[0];
        assert_eq!(f.status, FileStatus::Deleted);
        assert!(f.hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == DiffLineKind::Removed));
    }

    // ── 4. Renamed file ───────────────────────────────────────────────────────
    #[test]
    fn test_renamed_file() {
        let raw = "\
diff --git a/old_name.rs b/new_name.rs
similarity index 95%
rename from old_name.rs
rename to new_name.rs
--- a/old_name.rs
+++ b/new_name.rs
@@ -1 +1 @@
-fn old() {}
+fn new() {}
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let f = &diffs[0];
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.old_path, Some(PathBuf::from("old_name.rs")));
        assert_eq!(f.path, PathBuf::from("new_name.rs"));
    }

    // ── 5. Binary file ────────────────────────────────────────────────────────
    #[test]
    fn test_binary_file() {
        let raw = "\
diff --git a/image.png b/image.png
index abc..def 100644
Binary files a/image.png and b/image.png differ
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let f = &diffs[0];
        assert!(f.is_binary);
        assert!(f.hunks.is_empty());
    }

    // ── 6. Multi-file diff ────────────────────────────────────────────────────
    #[test]
    fn test_multi_file_diff() {
        let raw = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old a
+new a
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-old b
+new b
diff --git a/c.rs b/c.rs
--- a/c.rs
+++ b/c.rs
@@ -1 +1 @@
-old c
+new c
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 3);
        let paths: Vec<_> = diffs.iter().map(|d| d.path.to_str().unwrap()).collect();
        assert!(paths.contains(&"a.rs"));
        assert!(paths.contains(&"b.rs"));
        assert!(paths.contains(&"c.rs"));
    }

    // ── 7. Multiple hunks ─────────────────────────────────────────────────────
    #[test]
    fn test_multiple_hunks() {
        let raw = "\
diff --git a/foo.rs b/foo.rs
--- a/foo.rs
+++ b/foo.rs
@@ -1,3 +1,3 @@
 ctx
-old1
+new1
 ctx
@@ -10,3 +10,3 @@
 ctx
-old2
+new2
 ctx
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let f = &diffs[0];
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].old_start, 1);
        assert_eq!(f.hunks[0].old_count, 3);
        assert_eq!(f.hunks[1].old_start, 10);
        assert_eq!(f.hunks[1].old_count, 3);
    }

    // ── 8. Empty input ────────────────────────────────────────────────────────
    #[test]
    fn test_empty_input() {
        let diffs = parse_unified_diff("");
        assert!(diffs.is_empty());
    }

    // ── 9. Omitted hunk count defaults to 1 ──────────────────────────────────
    #[test]
    fn test_hunk_omitted_count() {
        // `@@ -5 +5 @@` — no comma → count defaults to 1
        let raw = "\
diff --git a/x.rs b/x.rs
--- a/x.rs
+++ b/x.rs
@@ -5 +5 @@
-removed
+added
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let h = &diffs[0].hunks[0];
        assert_eq!(h.old_start, 5);
        assert_eq!(h.old_count, 1);
        assert_eq!(h.new_start, 5);
        assert_eq!(h.new_count, 1);
    }

    // ── 10. No-newline marker skipped ─────────────────────────────────────────
    #[test]
    fn test_no_newline_marker_skipped() {
        let raw = "\
diff --git a/y.rs b/y.rs
--- a/y.rs
+++ b/y.rs
@@ -1,2 +1,2 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let lines = &diffs[0].hunks[0].lines;
        // Only the actual diff lines, no backslash marker.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].kind, DiffLineKind::Removed);
        assert_eq!(lines[1].kind, DiffLineKind::Added);
        assert!(lines.iter().all(|l| !l.content.starts_with('\\')));
    }

    // ── 11. Generated file ────────────────────────────────────────────────────
    #[test]
    fn test_generated_file() {
        let raw = "\
diff --git a/package-lock.json b/package-lock.json
--- a/package-lock.json
+++ b/package-lock.json
@@ -1 +1 @@
-{}
+{ \"version\": 2 }
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].is_generated);
    }

    // ── 12. Context lines ─────────────────────────────────────────────────────
    #[test]
    fn test_context_lines() {
        let raw = "\
diff --git a/ctx.rs b/ctx.rs
--- a/ctx.rs
+++ b/ctx.rs
@@ -1,3 +1,3 @@
 before
-old
+new
 after
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let lines = &diffs[0].hunks[0].lines;
        assert_eq!(lines[0].kind, DiffLineKind::Context);
        assert_eq!(lines[0].content, "before");
        assert_eq!(lines[3].kind, DiffLineKind::Context);
        assert_eq!(lines[3].content, "after");
    }

    // ── 13. Hunk trailing context text ignored ────────────────────────────────
    #[test]
    fn test_hunk_trailing_context() {
        let raw = "\
diff --git a/lib.rs b/lib.rs
--- a/lib.rs
+++ b/lib.rs
@@ -10,3 +10,3 @@ fn my_function() {
 ctx
-old
+new
 ctx
";
        let diffs = parse_unified_diff(raw);
        assert_eq!(diffs.len(), 1);
        let h = &diffs[0].hunks[0];
        // Ranges parsed correctly regardless of trailing `fn my_function() {`
        assert_eq!(h.old_start, 10);
        assert_eq!(h.old_count, 3);
        assert_eq!(h.new_start, 10);
        assert_eq!(h.new_count, 3);
        assert_eq!(h.lines.len(), 4);
    }
}
