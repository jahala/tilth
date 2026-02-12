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
    let parts = match (defs, usages) {
        (0, _) => format!("{total} matches"),
        (d, u) => format!("{total} matches ({d} definitions, {u} usages)"),
    };
    format!("# Search: \"{query}\" in {} — {parts}", scope.display())
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
