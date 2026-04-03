use memchr::memmem;

use crate::run::types::{Counts, DetectResult, Diagnostic, Location, ParsedOutput, Severity};

use super::Parser;

pub static PARSER: GenericParser = GenericParser;

pub struct GenericParser;

/// Error-level keywords searched via SIMD `memmem` finders.
///
/// We build these once and reuse them per parse call.
struct Finders {
    error: memmem::Finder<'static>,
    failed: memmem::Finder<'static>,
    fatal: memmem::Finder<'static>,
    exception: memmem::Finder<'static>,
    panic: memmem::Finder<'static>,
}

impl Finders {
    fn new() -> Self {
        Self {
            error: memmem::Finder::new("error"),
            failed: memmem::Finder::new("failed"),
            fatal: memmem::Finder::new("fatal"),
            exception: memmem::Finder::new("exception"),
            panic: memmem::Finder::new("panic"),
        }
    }

    fn matches(&self, lower: &[u8]) -> bool {
        self.error.find(lower).is_some()
            || self.failed.find(lower).is_some()
            || self.fatal.find(lower).is_some()
            || self.exception.find(lower).is_some()
            || self.panic.find(lower).is_some()
    }
}

impl Parser for GenericParser {
    fn name(&self) -> &'static str {
        "unknown"
    }

    /// The generic parser is the implicit fallback; it never claims a match.
    fn detect(&self, _sample: &str) -> DetectResult {
        DetectResult::NoMatch
    }

    fn parse(&self, input: &str, _hint: DetectResult) -> ParsedOutput {
        let raw_bytes = input.len();
        let lines: Vec<&str> = input.lines().collect();
        let raw_lines = lines.len();

        // ≤20 lines: passthrough — format step will handle this, but we still build
        // a minimal ParsedOutput so the caller can check `raw_lines`.
        if raw_lines <= 20 {
            return ParsedOutput {
                tool: "unknown",
                summary: String::new(),
                diagnostics: Vec::new(),
                counts: Counts::default(),
                duration_secs: None,
                raw_lines,
                raw_bytes,
            };
        }

        let finders = Finders::new();
        let mut diagnostics = Vec::new();
        let mut error_count: u32 = 0;
        // Reusable buffer for ASCII-lowercased bytes — avoids allocation per line.
        let mut lower_buf: Vec<u8> = Vec::new();

        for line in &lines {
            // ASCII-lowercase into reusable buffer (all keywords are ASCII).
            let line_bytes = line.as_bytes();
            lower_buf.clear();
            lower_buf.extend(line_bytes.iter().map(u8::to_ascii_lowercase));

            if finders.matches(&lower_buf) {
                error_count += 1;
                let location = Location::scan_line(line);
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    location,
                    name: normalize_message(line.trim()),
                    message: line.trim().to_string(),
                    detail: None,
                });
            }
        }

        let summary = if error_count == 0 {
            format!("{raw_lines} lines, no errors detected")
        } else {
            format!("{raw_lines} lines, {error_count} error line(s)")
        };

        let counts = Counts {
            errors: error_count,
            ..Counts::default()
        };

        ParsedOutput {
            tool: "unknown",
            summary,
            diagnostics,
            counts,
            duration_secs: None,
            raw_lines,
            raw_bytes,
        }
    }
}

/// Normalize an error message into a grouping key by stripping location-specific
/// parts (file paths, line numbers). Two lines that say the same thing at different
/// locations should produce the same key.
///
/// Example: "error: unused variable `x` at src/lib.rs:42" → "error: unused variable `x`"
fn normalize_message(msg: &str) -> String {
    // Strip trailing " at path:line:col" or " at path:line" patterns
    let trimmed = msg.trim();

    // Strip leading "path:line:col: " prefix (common in compiler output)
    let after_prefix = if let Some(loc_end) = find_location_prefix_end(trimmed) {
        trimmed[loc_end..].trim_start_matches([' ', ':'])
    } else {
        trimmed
    };

    // Strip trailing " at path:line" suffix
    if let Some(at_pos) = after_prefix.rfind(" at ") {
        let suffix = &after_prefix[at_pos + 4..];
        if looks_like_location(suffix) {
            return after_prefix[..at_pos].to_string();
        }
    }

    after_prefix.to_string()
}

/// Check if a string looks like "path:line" or "path:line:col"
fn looks_like_location(s: &str) -> bool {
    let parts: Vec<&str> = s.splitn(3, ':').collect();
    parts.len() >= 2 && parts[1].chars().all(|c| c.is_ascii_digit()) && !parts[1].is_empty()
}

/// Find the end of a "path:line:col: " prefix. Returns byte offset after the prefix.
fn find_location_prefix_end(s: &str) -> Option<usize> {
    // Look for pattern: non-space chars containing '.', then :digits, optional :digits, then :
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut saw_dot = false;

    // Scan the path part
    while i < bytes.len() && bytes[i] != b':' {
        if bytes[i] == b'.' || bytes[i] == b'/' {
            saw_dot = true;
        }
        if bytes[i] == b' ' {
            return None;
        } // paths don't have spaces
        i += 1;
    }
    if !saw_dot || i == 0 || i >= bytes.len() {
        return None;
    }

    // Must have :digits
    i += 1; // skip :
    let num_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == num_start {
        return None;
    }

    // Optional :digits (column)
    if i < bytes.len() && bytes[i] == b':' {
        let col_start = i + 1;
        let mut j = col_start;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > col_start {
            i = j;
        }
    }

    // Must be followed by : or space
    if i < bytes.len() && (bytes[i] == b':' || bytes[i] == b' ') {
        Some(i)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_passthrough() {
        let input = "line 1\nline 2\nline 3";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.raw_lines, 3);
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn extracts_error_lines() {
        let mut lines = Vec::new();
        for i in 0..30 {
            if i % 5 == 0 {
                lines.push(format!("ERROR: something failed at line {i}"));
            } else {
                lines.push(format!("line {i}: all good"));
            }
        }
        let input = lines.join("\n");
        let out = PARSER.parse(&input, DetectResult::Text);
        assert!(!out.diagnostics.is_empty());
        assert_eq!(out.counts.errors, out.diagnostics.len() as u32);
    }

    #[test]
    fn location_extraction() {
        let loc = Location::scan_line("src/main.rs:42: error: something");
        assert!(loc.is_some());
        let loc = loc.unwrap();
        assert_eq!(loc.file, "src/main.rs");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn location_with_column() {
        let loc = Location::scan_line("src/lib.rs:10:5: warning here");
        assert!(loc.is_some());
        let loc = loc.unwrap();
        assert_eq!(loc.line, 10);
        assert_eq!(loc.column, Some(5));
    }

    #[test]
    fn detect_always_false() {
        assert!(!PARSER.detect("anything").matched());
    }

    #[test]
    fn normalize_strips_location_prefix() {
        assert_eq!(
            normalize_message("src/lib.rs:42: error: unused variable"),
            "error: unused variable"
        );
    }

    #[test]
    fn normalize_strips_at_suffix() {
        assert_eq!(
            normalize_message("error: unused variable `x` at src/lib.rs:42"),
            "error: unused variable `x`"
        );
    }

    #[test]
    fn normalize_preserves_plain_message() {
        assert_eq!(
            normalize_message("ERROR: something failed"),
            "ERROR: something failed"
        );
    }

    #[test]
    fn duplicate_errors_group_by_message() {
        let mut lines = Vec::new();
        for i in 0..30 {
            lines.push(format!(
                "error: unused variable `x` at src/lib.rs:{}",
                i + 1
            ));
        }
        let input = lines.join("\n");
        let out = PARSER.parse(&input, DetectResult::Text);
        // All 30 should have the same name (normalized message)
        let first_name = &out.diagnostics[0].name;
        assert!(out.diagnostics.iter().all(|d| &d.name == first_name));
    }
}
