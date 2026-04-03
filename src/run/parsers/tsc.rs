use memchr::memmem;

use crate::run::types::{
    build_lint_summary, parse_found_count, Counts, DetectResult, Diagnostic, Location,
    ParsedOutput, Severity,
};

use super::Parser;

pub static PARSER: TscParser = TscParser;

pub struct TscParser;

impl Parser for TscParser {
    fn name(&self) -> &'static str {
        "tsc"
    }

    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();
        // tsc text fingerprint: `): error TS` or `): warning TS`
        let error_finder = memmem::Finder::new(b"): error TS");
        let warning_finder = memmem::Finder::new(b"): warning TS");
        if error_finder.find(bytes).is_some() || warning_finder.find(bytes).is_some() {
            DetectResult::Text
        } else {
            DetectResult::NoMatch
        }
    }

    fn parse(&self, input: &str, _hint: DetectResult) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();
        parse_text(input, raw_lines, raw_bytes)
    }
}

// ---------------------------------------------------------------------------
// Text path
// ---------------------------------------------------------------------------

/// Parse tsc text output.
///
/// Diagnostic format: `src/index.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.`
/// Summary format: `Found 2 errors in 2 files.`
fn parse_text(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;

    let mut found_summary_errors: Option<u32> = None;

    for line in input.lines() {
        if let Some(diag) = parse_text_line(line) {
            match diag.severity {
                Severity::Error => error_count += 1,
                Severity::Warning => warning_count += 1,
                Severity::Info => {}
            }
            diagnostics.push(diag);
            continue;
        }

        if let Some(n) = parse_found_count(line) {
            found_summary_errors = Some(n);
        }
    }

    // If we got a summary but no diagnostics, use summary counts
    if diagnostics.is_empty() {
        if let Some(n) = found_summary_errors {
            error_count = n;
        }
    }

    let summary = build_lint_summary(error_count, warning_count);

    ParsedOutput {
        tool: "tsc",
        summary,
        diagnostics,
        counts: Counts {
            errors: error_count,
            warnings: warning_count,
            ..Counts::default()
        },
        duration_secs: None,
        raw_lines,
        raw_bytes,
    }
}

/// Scan `bytes` forward for the tsc location pattern `(<digits>,<digits>):`.
///
/// Returns `(paren_open, paren_close)` byte indices on success.
/// Using `rfind('(')` is incorrect because tsc error messages often contain
/// parentheses in type signatures, e.g. `(a: number) => void`.
fn find_location_parens(bytes: &[u8], len: usize) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < len {
        if bytes[i] != b'(' {
            i += 1;
            continue;
        }
        let open = i;
        let inner_start = open + 1;

        // Collect digits for line number.
        let mut j = inner_start;
        while j < len && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == inner_start || j >= len || bytes[j] != b',' {
            i += 1;
            continue;
        }

        // Collect digits for column number.
        let mut k = j + 1;
        while k < len && bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k == j + 1 || k >= len || bytes[k] != b')' {
            i += 1;
            continue;
        }

        // Verify `):` follows so we know this is the location, not an inner paren.
        if k + 1 >= len || bytes[k + 1] != b':' {
            i += 1;
            continue;
        }

        return Some((open, k));
    }
    None
}

/// Parse a single tsc diagnostic line.
///
/// Format: `path/to/file.ts(line,col): error TS2322: message text`
fn parse_text_line(line: &str) -> Option<Diagnostic> {
    // Scan forward for `(<digits>,<digits>):` — do NOT use rfind('(') which
    // breaks when the error message itself contains parentheses, e.g. type
    // signatures like `(a: number) => void`.
    let bytes = line.as_bytes();
    let len = bytes.len();
    let (paren_open, paren_close) = find_location_parens(bytes, len)?;

    let file = line[..paren_open].to_string();
    if file.is_empty() {
        return None;
    }

    // Parse `line,col` inside the parens
    let coords = &line[paren_open + 1..paren_close];
    let (line_num, column) = parse_coords(coords)?;

    // After `):` we expect ` error TSxxxx:` or ` warning TSxxxx:`
    let after_paren = line[paren_close + 2..].trim_start();

    let (severity, after_severity) = if let Some(r) = after_paren.strip_prefix("error ") {
        (Severity::Error, r)
    } else if let Some(r) = after_paren.strip_prefix("warning ") {
        (Severity::Warning, r)
    } else {
        return None;
    };

    // `after_severity` should be `TSxxxx: message`
    let colon_pos = after_severity.find(": ")?;
    let ts_code = after_severity[..colon_pos].to_string();
    let message = after_severity[colon_pos + 2..].trim().to_string();

    // Validate the code looks like `TSdddd`
    if !ts_code.starts_with("TS") {
        return None;
    }

    Some(Diagnostic {
        severity,
        location: Some(Location {
            file,
            line: line_num,
            column: Some(column),
        }),
        name: ts_code,
        message,
        detail: None,
    })
}

/// Parse `line,col` from the parenthesized location portion.
fn parse_coords(coords: &str) -> Option<(u32, u32)> {
    let comma = coords.find(',')?;
    let line_num: u32 = coords[..comma].trim().parse().ok()?;
    let col: u32 = coords[comma + 1..].trim().parse().ok()?;
    Some((line_num, col))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- detect ---

    #[test]
    fn detect_error_ts() {
        let sample =
            "src/index.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_warning_ts() {
        let sample = "src/utils.ts(15,3): warning TS6133: 'unused' is declared but never used.\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects() {
        let sample = "some random\noutput with no tsc markers\n";
        assert!(!PARSER.detect(sample).matched());
    }

    // --- Text path ---

    #[test]
    fn parse_text_errors() {
        let input = concat!(
            "src/index.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.\n",
            "src/utils.ts(20,1): error TS2345: Argument of type 'number' is not assignable to parameter of type 'string'.\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.severity, Severity::Error);
        assert_eq!(first.name, "TS2322");
        assert_eq!(
            first.message,
            "Type 'string' is not assignable to type 'number'."
        );
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("src/index.ts")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(10));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(5));

        let second = &out.diagnostics[1];
        assert_eq!(second.name, "TS2345");
        assert_eq!(
            second.location.as_ref().map(|l| l.file.as_str()),
            Some("src/utils.ts")
        );

        assert_eq!(out.counts.errors, 2);
        assert_eq!(out.counts.warnings, 0);
        assert_eq!(out.summary, "2 error(s)");
    }

    #[test]
    fn parse_text_warnings() {
        let input = "src/utils.ts(15,3): warning TS6133: 'unused' is declared but never used.\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 1);

        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.name, "TS6133");
        assert_eq!(diag.message, "'unused' is declared but never used.");
        assert_eq!(diag.location.as_ref().map(|l| l.line), Some(15));
        assert_eq!(diag.location.as_ref().and_then(|l| l.column), Some(3));

        assert_eq!(out.counts.warnings, 1);
        assert_eq!(out.counts.errors, 0);
        assert_eq!(out.summary, "1 warning(s)");
    }

    #[test]
    fn parse_text_summary() {
        let input = "Found 2 errors in 2 files.\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.errors, 2);
        assert_eq!(out.summary, "2 error(s)");
    }

    // --- Bug 1 regression: rfind('(') breaks on messages with parentheses ---

    #[test]
    fn parse_text_message_with_parens_in_type() {
        // The message contains a function type `(a: number) => void` with parens.
        // rfind('(') would wrongly find the `(a:` paren instead of the location paren.
        let input = "src/index.ts(10,5): error TS2322: Type '(a: number) => void' is not assignable to type 'string'.\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "TS2322");
        assert_eq!(
            diag.location.as_ref().map(|l| l.file.as_str()),
            Some("src/index.ts")
        );
        assert_eq!(diag.location.as_ref().map(|l| l.line), Some(10));
        assert_eq!(diag.location.as_ref().and_then(|l| l.column), Some(5));
        assert!(
            diag.message.contains("(a: number) => void"),
            "message should contain the function type signature"
        );
    }
}
