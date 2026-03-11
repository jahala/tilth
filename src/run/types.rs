use std::fmt;

pub struct ParsedOutput {
    pub tool: &'static str,
    pub summary: String,
    pub diagnostics: Vec<Diagnostic>,
    pub counts: Counts,
    pub duration_secs: Option<f64>,
    pub raw_lines: usize,
    pub raw_bytes: usize,
}

/// Aggregate counts from parsed output.
///
/// Field semantics:
/// - `passed` / `failed` / `skipped`: test outcome counts (test runners only).
/// - `errors`: compilation errors, lint violations, or type-check errors (compilers/linters).
/// - `warnings`: compiler or lint warnings.
///
/// Test runners use `failed` for test failures and leave `errors` at 0.
/// Linters/compilers use `errors`/`warnings` and leave test fields at 0.
#[derive(Default)]
pub struct Counts {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub warnings: u32,
    pub errors: u32,
}

pub struct Diagnostic {
    pub severity: Severity,
    pub location: Option<Location>,
    pub name: String,
    pub message: String,
    pub detail: Option<String>,
}

pub struct DiagnosticGroup {
    pub severity: Severity,
    pub signature: String,
    pub locations: Vec<Location>,
    pub total: usize,
    pub representative: Diagnostic,
    pub cascading: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug)]
pub struct Location {
    pub file: String,
    pub line: u32,
    pub column: Option<u32>,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => f.write_str("ERROR"),
            Severity::Warning => f.write_str("WARN"),
            Severity::Info => f.write_str("INFO"),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.column {
            Some(col) => write!(f, "{}:{}:{}", self.file, self.line, col),
            None => write!(f, "{}:{}", self.file, self.line),
        }
    }
}

impl Location {
    /// Byte-walk a single line looking for a `path:digits` or `path:digits:digits` pattern.
    ///
    /// This is the shared location extractor used by multiple parsers. It looks for
    /// `file.ext:line` or `file.ext:line:col` patterns, walking backwards from the
    /// colon to find the path start (delimited by whitespace or quote chars).
    pub fn scan_line(line: &str) -> Option<Location> {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut i = 0;

        while i < len {
            if bytes[i] != b':' {
                i += 1;
                continue;
            }

            let num_start = i + 1;
            let mut num_end = num_start;
            while num_end < len && bytes[num_end].is_ascii_digit() {
                num_end += 1;
            }
            if num_end == num_start {
                i += 1;
                continue;
            }

            let line_num: u32 = match std::str::from_utf8(&bytes[num_start..num_end])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => { i += 1; continue; }
            };

            // Optional column after another colon.
            let col = if num_end < len && bytes[num_end] == b':' {
                let col_start = num_end + 1;
                let mut col_end = col_start;
                while col_end < len && bytes[col_end].is_ascii_digit() {
                    col_end += 1;
                }
                if col_end > col_start {
                    std::str::from_utf8(&bytes[col_start..col_end])
                        .ok()
                        .and_then(|s| s.parse::<u32>().ok())
                } else {
                    None
                }
            } else {
                None
            };

            let path_end = i;
            let path_start = bytes[..path_end]
                .iter()
                .rposition(|&b| b == b' ' || b == b'\t' || b == b'\'' || b == b'"' || b == b'(')
                .map(|pos| pos + 1)
                .unwrap_or(0);

            let path_bytes = &bytes[path_start..path_end];
            let looks_like_path = path_bytes.iter().any(|&b| b == b'.' || b == b'/');
            let all_digits = path_bytes.iter().all(|&b| b.is_ascii_digit());

            if looks_like_path && !all_digits && !path_bytes.is_empty() {
                if let Ok(file) = std::str::from_utf8(path_bytes) {
                    return Some(Location {
                        file: file.to_string(),
                        line: line_num,
                        column: col,
                    });
                }
            }

            i += 1;
        }

        None
    }

    /// Scan multi-line text for the first location match.
    pub fn scan_text(text: &str) -> Option<Location> {
        for line in text.lines() {
            if let Some(loc) = Location::scan_line(line.trim()) {
                return Some(loc);
            }
        }
        None
    }
}
/// Maximum byte length for diagnostic detail fields.
pub const MAX_DETAIL_BYTES: usize = 2048;

/// Truncate a string to [`MAX_DETAIL_BYTES`], respecting UTF-8 char boundaries.
///
/// Returns `None` if the input is empty/whitespace-only.
pub fn truncate_detail(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_DETAIL_BYTES {
        let mut boundary = MAX_DETAIL_BYTES;
        // Walk back to a UTF-8 char boundary (leading bytes start with 0xxxxxxx or 11xxxxxx).
        while boundary > 0 && !trimmed.is_char_boundary(boundary) {
            boundary -= 1;
        }
        Some(format!("{}…", &trimmed[..boundary]))
    } else {
        Some(trimmed.to_string())
    }
}

/// Extract a leading integer from a fragment like `"5 passed"`.
///
/// Searches `haystack` for `label`, then walks backwards from the match position
/// to collect consecutive digits, producing the count.
pub fn extract_count(haystack: &str, label: &str) -> Option<u32> {
    let pos = haystack.find(label)?;
    let before = haystack[..pos].trim_end();
    let digits: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}
