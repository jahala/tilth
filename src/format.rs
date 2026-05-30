use std::fmt::Write;
use std::path::Path;

use crate::types::{estimate_tokens, ViewMode};

/// Build the standard header line:
/// `# path/to/file.ts (N lines, ~X.Xk tokens) [mode]`
pub fn file_header(path: &Path, byte_len: u64, line_count: u32, mode: ViewMode) -> String {
    let tokens = estimate_tokens(byte_len);
    let token_str = if tokens >= 1000 {
        format!("~{}.{}k tokens", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens} tokens")
    };
    format!(
        "# {} ({line_count} lines, {token_str}) [{mode}]",
        path.display()
    )
}

/// Build header for binary files: `# path (binary, size, mime) [skipped]`
pub fn binary_header(path: &Path, byte_len: u64, mime: &str) -> String {
    let size_str = format_size(byte_len);
    format!(
        "# {} (binary, {size_str}, {mime}) [skipped]",
        path.display()
    )
}

/// Build header for search results.
pub fn search_header(
    query: &str,
    scope: &Path,
    total: usize,
    defs: usize,
    usages: usize,
) -> String {
    let parts = match (total, defs, usages) {
        (0, _, _) => "0 matches (no definitions or usages; try kind=content for strings/comments, widen scope, or check spelling)".to_string(),
        (_, 0, _) => format!("{total} matches"),
        (_, d, u) => format!("{total} matches ({d} definitions, {u} usages)"),
    };
    format!("# Search: \"{query}\" in {} — {parts}", scope.display())
}

/// Which search-kind produced a zero-result response. Determines which hint
/// the empty-result header surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyHint {
    Symbol,
    Content,
    Regex,
    Merged,
}

/// Emit the zero-result search header with three counts and a hint chosen
/// from the dispatch table:
///
/// * `files_matched_glob == 0` → `glob matched no files — broaden glob or check path`
/// * `Symbol` → `no symbols matched; try kind: content or check spelling`
/// * `Content` → `no content matches; try kind: symbol or a broader pattern`
/// * `Regex` → `regex matched zero content; try kind: symbol or a broader pattern`
/// * `Merged` → `no matches in any mode — re-check the query and glob`
pub fn search_empty_header(
    query: &str,
    scope: &Path,
    files_matched_glob: usize,
    files_searched: usize,
    content_hits: usize,
    kind: EmptyHint,
) -> String {
    let hint = if files_matched_glob == 0 {
        "glob matched no files — broaden glob or check path"
    } else {
        match kind {
            EmptyHint::Symbol => "no symbols matched; try kind: content or check spelling",
            EmptyHint::Content => "no content matches; try kind: symbol or a broader pattern",
            EmptyHint::Regex => "regex matched zero content; try kind: symbol or a broader pattern",
            EmptyHint::Merged => "no matches in any mode — re-check the query and glob",
        }
    };
    format!(
        "# Search: \"{query}\" in {scope_disp} — 0 matches\n  \
         Files matched glob: {files_matched_glob}\n  \
         Files searched:     {files_searched}\n  \
         Content hits:       {content_hits}\n  \
         Hint: {hint}",
        scope_disp = scope.display()
    )
}

/// Human-readable file size. Integer math only — no floats.
fn format_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b}B"),
        b if b < 1024 * 1024 => format!("{}KB", b / 1024),
        b => format!(
            "{}.{}MB",
            b / (1024 * 1024),
            (b % (1024 * 1024)) * 10 / (1024 * 1024)
        ),
    }
}

/// Prefix each line with its 1-indexed line number, right-aligned.
pub fn number_lines(content: &str, start: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let last = (start as usize + lines.len()).max(1);
    let width = (last.ilog10() + 1) as usize;
    let mut out = String::with_capacity(content.len() + lines.len() * (width + 2));
    for (i, line) in lines.iter().enumerate() {
        let num = start as usize + i;
        let _ = writeln!(out, "{num:>width$}  {line}");
    }
    out
}

// ---------------------------------------------------------------------------
// Hashline support (edit mode)
// ---------------------------------------------------------------------------

/// FNV-1a hash of a line, truncated to 12 bits (3 hex chars).
/// Used as a per-line content checksum for edit-mode anchors.
pub(crate) fn line_hash(bytes: &[u8]) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    (h & 0xFFF) as u16
}

/// Format lines with hashline anchors: `{line}:{hash}|{content}`
/// Used in edit mode so the agent can reference lines by content hash.
pub fn hashlines(content: &str, start: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = String::with_capacity(content.len() + lines.len() * 8);
    for (i, line) in lines.iter().enumerate() {
        let num = start as usize + i;
        let hash = line_hash(line.as_bytes());
        let _ = writeln!(out, "{num}:{hash:03x}|{line}");
    }
    out
}

/// Parse a hashline anchor `"42:a3f"` into `(line_number, hash)`.
/// Inverse of the format produced by [`hashlines`].
pub(crate) fn parse_anchor(s: &str) -> Option<(usize, u16)> {
    let (line_str, hash_str) = s.split_once(':')?;
    let line: usize = line_str.trim().parse().ok()?;
    if line == 0 {
        return None; // 1-indexed
    }
    let hash = u16::from_str_radix(hash_str.trim(), 16).ok()?;
    Some((line, hash))
}

/// Path relative to scope for cleaner output. Falls back to full path.
pub(crate) fn rel(path: &Path, scope: &Path) -> String {
    path.strip_prefix(scope)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G3: a zero-match search must carry an actionable hint so agents stop
    /// retrying the same query blindly, not just report "0 matches".
    #[test]
    fn search_header_zero_matches_includes_hint() {
        let header = search_header("doesNotExist", Path::new("/repo"), 0, 0, 0);
        assert!(header.contains("0 matches"), "{header}");
        assert!(header.contains("kind=content"), "{header}");
        assert!(header.contains("widen scope"), "{header}");
        assert!(header.contains("check spelling"), "{header}");
    }

    #[test]
    fn search_header_with_matches_has_no_hint() {
        let header = search_header("Foo", Path::new("/repo"), 3, 1, 2);
        assert!(
            header.contains("3 matches (1 definitions, 2 usages)"),
            "{header}"
        );
        assert!(!header.contains("check spelling"), "{header}");
    }

    /// Regression: a result with hits but zero definitions (every content
    /// search, plus usage-only symbol searches) must print just "{total}
    /// matches" — not the noisy "(0 definitions, N usages)" the zero-match
    /// hint change accidentally reintroduced on this path.
    #[test]
    fn search_header_usages_only_omits_definition_counts() {
        let header = search_header("logLine", Path::new("/repo"), 10, 0, 10);
        assert!(header.contains("10 matches"), "{header}");
        assert!(
            !header.contains("0 definitions"),
            "usage-only result must not show a 0-definitions count: {header}"
        );
        assert!(!header.contains("check spelling"), "{header}");
    }

    fn scope() -> std::path::PathBuf {
        std::path::PathBuf::from("/repo")
    }

    #[test]
    fn empty_header_glob_zero_overrides_kind() {
        // files_matched_glob == 0 wins regardless of kind.
        let out = search_empty_header("foo", &scope(), 0, 0, 0, EmptyHint::Symbol);
        assert!(out.contains("0 matches"), "{out}");
        assert!(out.contains("Files matched glob: 0"), "{out}");
        assert!(
            out.contains("glob matched no files — broaden glob or check path"),
            "{out}"
        );
    }

    #[test]
    fn empty_header_symbol_branch() {
        let out = search_empty_header("Foo", &scope(), 47, 47, 0, EmptyHint::Symbol);
        assert!(
            out.contains("no symbols matched; try kind: content or check spelling"),
            "{out}"
        );
        assert!(out.contains("Files searched:     47"), "{out}");
    }

    #[test]
    fn empty_header_content_branch() {
        let out = search_empty_header("foo", &scope(), 47, 47, 0, EmptyHint::Content);
        assert!(
            out.contains("no content matches; try kind: symbol or a broader pattern"),
            "{out}"
        );
    }

    #[test]
    fn empty_header_regex_branch() {
        // Regex has its own hint, distinct from Content's.
        let out = search_empty_header("foo.*bar", &scope(), 47, 47, 0, EmptyHint::Regex);
        assert!(
            out.contains("regex matched zero content; try kind: symbol or a broader pattern"),
            "{out}"
        );
    }

    #[test]
    fn empty_header_merged_branch() {
        let out = search_empty_header("foo", &scope(), 47, 47, 0, EmptyHint::Merged);
        assert!(
            out.contains("no matches in any mode — re-check the query and glob"),
            "{out}"
        );
    }
}
