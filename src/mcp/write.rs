//! `tilth_write` helpers: overwrite / append modes plus strict
//! fingerprint-based auto-fix for hash-anchored edits.

use std::fs;
use std::path::Path;

use crate::format;

/// Overwrite `path` with `content`, creating parent dirs if absent.
pub fn write_overwrite(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p)?;
        }
    }
    fs::write(path, content)
}

/// Append `content` to `path`, creating the file (and parent dirs) if absent.
pub fn write_append(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p)?;
        }
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(content.as_bytes())
}

/// Result of a strict auto-fix attempt.
pub enum AutoFixResult {
    /// Original anchor body was found at exactly one new location.
    Relocated { new_line: usize },
    /// Zero or 2+ matches — caller should return fresh hashlined region.
    Ambiguous { matches: usize },
}

/// Strict fingerprint auto-fix: re-read `path`, look for `original_body` byte
/// content. Returns the new 1-indexed start line if exactly one match exists.
pub fn auto_fix_locate(path: &Path, original_body: &str) -> std::io::Result<AutoFixResult> {
    let fresh = fs::read_to_string(path)?;
    let fresh_lines: Vec<&str> = fresh.lines().collect();
    let needle_lines: Vec<&str> = original_body.lines().collect();
    if needle_lines.is_empty() {
        return Ok(AutoFixResult::Ambiguous { matches: 0 });
    }
    let mut hits = Vec::new();
    if needle_lines.len() <= fresh_lines.len() {
        for start in 0..=(fresh_lines.len() - needle_lines.len()) {
            if fresh_lines[start..start + needle_lines.len()]
                .iter()
                .zip(needle_lines.iter())
                .all(|(a, b)| a == b)
            {
                hits.push(start + 1);
                if hits.len() > 1 {
                    break;
                }
            }
        }
    }
    match hits.len() {
        1 => Ok(AutoFixResult::Relocated { new_line: hits[0] }),
        n => Ok(AutoFixResult::Ambiguous { matches: n }),
    }
}

/// Format a fresh region's hashlined content around (and including) a
/// resolved line range, so the agent can retry in one turn.
pub fn fresh_region(path: &Path, start: usize, end: usize) -> std::io::Result<String> {
    let content = fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let s = start.saturating_sub(3).max(1);
    let e = (end + 3).min(total);
    let slice = lines[s - 1..e].join("\n");
    let mut out = format!("# {} (fresh region {s}-{e})\n", path.display());
    out.push_str(&format::hashlines(&slice, s as u32));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_fix_locate_exactly_one_match_relocates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        // The unique body "target line" appears once after a 2-line shift.
        std::fs::write(&p, "prefix1\nprefix2\ntarget line\nafter\n").unwrap();
        match auto_fix_locate(&p, "target line").unwrap() {
            AutoFixResult::Relocated { new_line } => {
                assert_eq!(new_line, 3, "target on line 3 (1-indexed)");
            }
            AutoFixResult::Ambiguous { matches } => {
                panic!("expected Relocated, got {matches} matches")
            }
        }
    }

    #[test]
    fn auto_fix_locate_zero_matches_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        match auto_fix_locate(&p, "NOT PRESENT ANYWHERE").unwrap() {
            AutoFixResult::Ambiguous { matches } => assert_eq!(matches, 0),
            AutoFixResult::Relocated { .. } => panic!("must not relocate when 0 matches"),
        }
    }

    #[test]
    fn auto_fix_locate_two_matches_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "dup\nother\ndup\nend\n").unwrap();
        match auto_fix_locate(&p, "dup").unwrap() {
            AutoFixResult::Ambiguous { matches } => {
                assert!(matches >= 2, "expected ≥2, got {matches}");
            }
            AutoFixResult::Relocated { .. } => panic!("ambiguous duplicates must not auto-fix"),
        }
    }

    #[test]
    fn auto_fix_locate_empty_needle_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "some content\n").unwrap();
        match auto_fix_locate(&p, "").unwrap() {
            AutoFixResult::Ambiguous { matches } => assert_eq!(matches, 0, "empty needle ⇒ 0"),
            AutoFixResult::Relocated { .. } => panic!("empty needle must not relocate"),
        }
    }

    #[test]
    fn fresh_region_returns_hashlined_window() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "a\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
        let out = fresh_region(&p, 4, 5).unwrap();
        assert!(out.contains("fresh region"), "header missing: {out}");
        // Window is ±3 lines, clamped to file bounds: lines 1..=8 here.
        assert!(out.contains('d'), "line 4 in window: {out}");
        assert!(out.contains('e'), "line 5 in window: {out}");
    }

    #[test]
    fn fresh_region_clamps_near_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "only\nfew\nlines\n").unwrap();
        // start=1 with saturating_sub(3).max(1) ⇒ window begins at 1, not 0/underflow.
        let out = fresh_region(&p, 1, 1).unwrap();
        assert!(out.contains("only"), "window must include line 1: {out}");
    }
}
