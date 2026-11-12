use std::fmt::Write;
use std::path::Path;

use crate::types::{estimate_tokens, ViewMode};

/// The noun that follows a count of `n`: `one` when `n` is exactly 1 and
/// `many` otherwise, as in "0 lines", "1 line", "2 lines". In "3 of 5 lines"
/// or "4→1 line" the noun agrees with the last count.
pub(crate) fn noun<'a, N: Copy + PartialEq + From<u8>>(
    n: N,
    one: &'a str,
    many: &'a str,
) -> &'a str {
    if n == N::from(1) {
        one
    } else {
        many
    }
}

/// Build the standard header line:
/// `# path/to/file.ts (N lines, ~X.Xk tokens) [mode]`
pub fn file_header(path: &Path, byte_len: u64, line_count: u32, mode: ViewMode) -> String {
    let tokens = estimate_tokens(byte_len);
    let token_str = if tokens >= 1000 {
        format!("~{}.{}k tokens", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens} {}", noun(tokens, "token", "tokens"))
    };
    format!(
        "# {} ({line_count} {}, {token_str}) [{mode}]",
        path.display(),
        noun(line_count, "line", "lines")
    )
}

/// Build header for binary files: `# path (binary, size, mime) [skipped]`
pub fn binary_header(path: &Path, byte_len: u64, mime: &str) -> String {
    let summary = binary_summary(byte_len, mime);
    format!("# {} ({summary}) [skipped]", path.display())
}

/// The `binary, <size>, <mime>` fragment shared by read headers and glob previews.
pub(crate) fn binary_summary(byte_len: u64, mime: &str) -> String {
    let size_str = format_size(byte_len);
    format!("binary, {size_str}, {mime}")
}

/// Build header for search results.
pub fn search_header(
    query: &str,
    scope: &Path,
    shown: usize,
    total: usize,
    defs: usize,
    usages: usize,
) -> String {
    // When the display cap bit, say so — a bare capped count reads as a
    // complete result and silently under-reports (the #51 bug).
    let matches = noun(total, "match", "matches");
    let count = if total > shown {
        format!("{shown} of {total} {matches}")
    } else {
        format!("{total} {matches}")
    };
    let parts = match (defs, usages) {
        (0, _) => count,
        (d, u) => format!(
            "{count} ({d} {}, {u} {})",
            noun(d, "definition", "definitions"),
            noun(u, "usage", "usages")
        ),
    };
    format!("# Search: \"{query}\" in {} — {parts}", scope.display())
}

/// Human-readable file size. Integer math only — no floats.
fn format_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    match bytes {
        b if b < 1024 => format!("{b}B"),
        b if b < MB => format!("{}KB", b / 1024),
        b if b < GB => format!("{}.{}MB", b / MB, (b % MB) * 10 / MB),
        b => format!("{}.{}GB", b / GB, (b % GB) * 10 / GB),
    }
}

/// Compact token count for directory rollups, as in `12.3k` and `1.2M`.
/// The `tilth_list` tree and the codebase map both use it.
///
/// The k→M switchover triggers at `999_950` rather than `1_000_000` so values
/// that would round to `"1000.0k"` under `{:.1}` formatting roll cleanly into
/// the M tier (`"1.0M"`) instead of producing four-digit k labels.
pub(crate) fn fmt_tokens(n: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "by design: display only, rounded to one decimal"
    )]
    let f = n as f64;
    if f >= 999_950.0 {
        format!("{:.1}M", f / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", f / 1_000.0)
    } else {
        n.to_string()
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

    #[test]
    fn counts_of_one_take_the_singular() {
        let header = file_header(Path::new("a.rs"), 2, 1, ViewMode::Full);
        assert_eq!(header, "# a.rs (1 line, ~1 token) [full]");
        let header = file_header(Path::new("a.rs"), 0, 0, ViewMode::Full);
        assert_eq!(header, "# a.rs (0 lines, ~0 tokens) [full]");

        let search = search_header("q", Path::new("src"), 1, 1, 1, 0);
        assert_eq!(
            search,
            "# Search: \"q\" in src — 1 match (1 definition, 0 usages)"
        );
    }

    #[test]
    fn fmt_tokens_thresholds() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(1_000_000), "1.0M");
        assert_eq!(fmt_tokens(2_500_000), "2.5M");
    }

    /// Boundary: values that would round to "1000.0k" under `{:.1}`
    /// formatting must roll over to the M tier instead of producing a
    /// four-digit k label.
    #[test]
    fn fmt_tokens_rolls_over_at_999_950() {
        // Just below the rollover boundary — still formats as k.
        assert_eq!(fmt_tokens(999_949), "999.9k");
        // At and above the boundary — formats as M, not "1000.0k".
        assert_eq!(fmt_tokens(999_950), "1.0M");
        assert_eq!(fmt_tokens(999_999), "1.0M");
        assert_eq!(fmt_tokens(1_499_999), "1.5M");
    }

    #[test]
    fn format_size_covers_every_tier() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(8 * 1024), "8KB");
        assert_eq!(format_size(3 * 1024 * 1024 + 512 * 1024), "3.5MB");
        // Just under a GB stays in the MB tier…
        assert_eq!(format_size(1024 * 1024 * 1024 - 1), "1023.9MB");
        // …and from 1GB up the GB tier takes over: 8GB is not "8192.0MB".
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0GB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024 / 2), "1.5GB");
        assert_eq!(format_size(8 * 1024 * 1024 * 1024), "8.0GB");
    }

    #[test]
    fn search_header_qualifies_only_a_capped_count() {
        let scope = Path::new("src");
        // Nothing hidden — the bare total, exactly as before.
        assert_eq!(
            search_header("handleAuth", scope, 6, 6, 2, 4),
            "# Search: \"handleAuth\" in src — 6 matches (2 definitions, 4 usages)"
        );
        // Capped — the shown count is qualified by the real total, and the
        // definition/usage breakdown still describes the whole result set.
        assert_eq!(
            search_header("handleAuth", scope, 10, 42, 2, 40),
            "# Search: \"handleAuth\" in src — 10 of 42 matches (2 definitions, 40 usages)"
        );
        // Content/regex results report no definitions — count only.
        assert_eq!(
            search_header("needle", scope, 10, 15, 0, 15),
            "# Search: \"needle\" in src — 10 of 15 matches"
        );
    }
}
