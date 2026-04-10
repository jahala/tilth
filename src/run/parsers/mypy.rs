use memchr::memmem;
use serde_json::Value;

use crate::run::types::{
    build_lint_summary, parse_found_count, Counts, DetectResult, Diagnostic, Location,
    ParsedOutput, Severity,
};

use super::Parser;

pub static PARSER: MypyParser = MypyParser;

pub struct MypyParser;

impl Parser for MypyParser {
    fn name(&self) -> &'static str {
        "mypy"
    }

    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // JSON fingerprint (mypy --output=json NDJSON): lines with "severity" + "message" + .py
        let severity_finder = memmem::Finder::new(b"\"severity\"");
        let message_finder = memmem::Finder::new(b"\"message\"");
        let py_finder = memmem::Finder::new(b".py\"");
        if severity_finder.find(bytes).is_some()
            && message_finder.find(bytes).is_some()
            && py_finder.find(bytes).is_some()
        {
            return DetectResult::NdJson;
        }

        // Text fingerprint: `path.py:N: error:` or `path.py:N: warning:`
        let error_finder = memmem::Finder::new(b".py:");
        if error_finder.find(bytes).is_none() {
            return DetectResult::NoMatch;
        }
        if sample.lines().any(looks_like_mypy_text_line) {
            DetectResult::Text
        } else {
            DetectResult::NoMatch
        }
    }

    fn parse(&self, input: &str, hint: DetectResult) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        if hint.is_json() {
            parse_json(input, raw_lines, raw_bytes)
        } else {
            parse_text(input, raw_lines, raw_bytes)
        }
    }
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// Returns true if `line` looks like a mypy text diagnostic line.
///
/// Format: `src/main.py:42: error: Incompatible types  [assignment]`
fn looks_like_mypy_text_line(line: &str) -> bool {
    looks_like_mypy_text_line_inner(line).unwrap_or(false)
}

fn looks_like_mypy_text_line_inner(line: &str) -> Option<bool> {
    // Must contain `.py:` followed by a line number and `: error:` or `: warning:` or `: note:`
    let py_col = line.find(".py:")?;
    let after_py = &line[py_col + 4..];
    // Next portion should be digits then `: severity:`
    let colon_pos = after_py.find(':')?;
    let line_num_part = &after_py[..colon_pos];
    if !line_num_part.chars().all(|c| c.is_ascii_digit()) || line_num_part.is_empty() {
        return Some(false);
    }
    let after_line = &after_py[colon_pos + 1..];
    let severity_part = after_line.trim_start();
    Some(
        severity_part.starts_with("error:")
            || severity_part.starts_with("warning:")
            || severity_part.starts_with("note:"),
    )
}

// ---------------------------------------------------------------------------
// JSON path
// ---------------------------------------------------------------------------

/// Parse mypy NDJSON output (one JSON object per line).
///
/// Each line looks like:
/// ```json
/// {"file": "src/main.py", "line": 42, "column": 5, "severity": "error", "message": "Incompatible types", "code": "assignment"}
/// ```
fn parse_json(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(diag) = extract_json_diagnostic(&value) else {
            continue;
        };
        match diag.severity {
            Severity::Error => error_count += 1,
            Severity::Warning => warning_count += 1,
            Severity::Info => {}
        }
        diagnostics.push(diag);
    }

    let summary = build_lint_summary(error_count, warning_count);

    ParsedOutput {
        tool: "mypy",
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

fn extract_json_diagnostic(value: &Value) -> Option<Diagnostic> {
    let severity_str = value
        .get("severity")
        .and_then(Value::as_str)
        .unwrap_or("note");
    let severity = match severity_str {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    };

    let message = value.get("message").and_then(Value::as_str)?.to_string();

    let name = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let file = value
        .get("file")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let line = value.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
    let column = value
        .get("column")
        .and_then(Value::as_u64)
        .map(|c| c as u32);

    let location = if file.is_empty() {
        None
    } else {
        Some(Location { file, line, column })
    };

    Some(Diagnostic {
        severity,
        location,
        name,
        message,
        detail: None,
    })
}

// ---------------------------------------------------------------------------
// Text path
// ---------------------------------------------------------------------------

/// Parse mypy text output.
///
/// Diagnostic format: `src/main.py:42: error: Incompatible types in assignment  [assignment]`
/// Summary format: `Found 2 errors in 1 file (checked 5 source files)`
fn parse_text(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;

    // Check for summary line to extract counts if diagnostics are absent
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

        // `Found N errors in M file(s)` or `Found N errors`
        if let Some(n) = parse_found_count(line) {
            found_summary_errors = Some(n);
        }
    }

    // If we got a summary but no diagnostics (e.g. abbreviated output), use summary counts
    if diagnostics.is_empty() {
        if let Some(n) = found_summary_errors {
            error_count = n;
        }
    }

    let summary = build_lint_summary(error_count, warning_count);

    ParsedOutput {
        tool: "mypy",
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

/// Parse a single mypy text diagnostic line.
///
/// Format: `path.py:N: severity: message  [code]`
fn parse_text_line(line: &str) -> Option<Diagnostic> {
    if !looks_like_mypy_text_line(line) {
        return None;
    }

    // Split on the first `: ` that follows a `path.py:N` prefix.
    // Find `.py:digits:` pattern to locate the end of the location prefix.
    let py_col = line.find(".py:")?;
    let after_py = &line[py_col + 4..];
    let colon_pos = after_py.find(':')?;
    // `location_end` is the index in `line` of the `:` after the line number
    let location_end = py_col + 4 + colon_pos;
    let file = line[..py_col + 3].to_string(); // includes ".py"
    let line_num: u32 = after_py[..colon_pos].trim().parse().ok()?;

    // After `path.py:N:` we have ` severity: message  [code]`
    let rest = line[location_end + 1..].trim_start();

    let (severity_str, after_severity) = if let Some(r) = rest.strip_prefix("error:") {
        ("error", r)
    } else if let Some(r) = rest.strip_prefix("warning:") {
        ("warning", r)
    } else if let Some(r) = rest.strip_prefix("note:") {
        ("note", r)
    } else {
        return None;
    };

    let severity = match severity_str {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    };

    let message_raw = after_severity.trim();

    // Extract optional `[code]` at the end of the message.
    // Try double-space prefix first ("  ["), then single-space (" [").
    // The offset to skip past the opening bracket differs: "  [" → +3, " [" → +2.
    let (message, name) = if message_raw.ends_with(']') {
        if let Some(bracket_start) = message_raw.rfind("  [") {
            let msg = message_raw[..bracket_start].trim().to_string();
            let code = message_raw[bracket_start + 3..message_raw.len() - 1].to_string();
            (msg, code)
        } else if let Some(bracket_start) = message_raw.rfind(" [") {
            let msg = message_raw[..bracket_start].trim().to_string();
            let code = message_raw[bracket_start + 2..message_raw.len() - 1].to_string();
            (msg, code)
        } else {
            (message_raw.to_string(), severity_str.to_string())
        }
    } else {
        (message_raw.to_string(), severity_str.to_string())
    };

    Some(Diagnostic {
        severity,
        location: Some(Location {
            file,
            line: line_num,
            column: None,
        }),
        name,
        message,
        detail: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- detect ---

    #[test]
    fn detect_text() {
        let sample = "src/main.py:42: error: Incompatible types in assignment  [assignment]\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects() {
        let sample = "some random\noutput with no mypy markers\n";
        assert!(!PARSER.detect(sample).matched());
    }

    // --- JSON path ---

    #[test]
    fn parse_json() {
        let input = concat!(
            r#"{"file": "src/main.py", "line": 42, "column": 5, "severity": "error", "message": "Incompatible types in assignment", "code": "assignment"}"#,
            "\n",
            r#"{"file": "src/main.py", "line": 43, "column": 1, "severity": "note", "message": "Expected \"int\", got \"str\"", "code": null}"#,
            "\n",
        );
        let out = PARSER.parse(input, DetectResult::NdJson);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.severity, Severity::Error);
        assert_eq!(first.name, "assignment");
        assert_eq!(first.message, "Incompatible types in assignment");
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("src/main.py")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(42));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(5));

        let second = &out.diagnostics[1];
        assert_eq!(second.severity, Severity::Info);

        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.counts.warnings, 0);
        assert_eq!(out.summary, "1 error(s)");
    }

    // --- Text path ---

    #[test]
    fn parse_text() {
        let input = concat!(
            "src/main.py:42: error: Incompatible types in assignment  [assignment]\n",
            "src/main.py:43: note: Expected \"int\", got \"str\"\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.severity, Severity::Error);
        assert_eq!(first.name, "assignment");
        assert_eq!(first.message, "Incompatible types in assignment");
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("src/main.py")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(42));

        let second = &out.diagnostics[1];
        assert_eq!(second.severity, Severity::Info);

        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.summary, "1 error(s)");
    }

    // Bug 6 regression: single-space " [code]" prefix must use offset +2, not +3.
    // Without the fix, the extracted code would be "assignment]" (off-by-one).
    #[test]
    fn parse_text_single_space_bracket_code() {
        let input = "src/main.py:10: error: Incompatible types [assignment]\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(
            diag.name, "assignment",
            "code must not include leading space or bracket"
        );
        assert_eq!(diag.message, "Incompatible types");
    }

    #[test]
    fn parse_text_summary() {
        // When mypy emits only a summary line (e.g., with --no-error-summary off),
        // we should still capture the count.
        let input = "Found 3 errors in 2 files (checked 10 source files)\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.errors, 3);
        assert_eq!(out.summary, "3 error(s)");
    }

    // Bug 5 regression: ruff's weak fingerprint was before mypy in PARSERS, so
    // mypy JSON output could be claimed by ruff if ruff matched first.
    // With the strengthened ruff fingerprint (Bug 3 fix), mypy JSON must NOT be
    // detected as ruff — mypy NDJSON uses "severity"/"message"/"file", not "filename"/"row".
    #[test]
    fn mypy_json_not_claimed_by_ruff() {
        let mypy_json = concat!(
            r#"{"file": "src/main.py", "line": 42, "column": 5, "severity": "error", "message": "Incompatible types in assignment", "code": "assignment"}"#,
            "\n",
        );
        // ruff.detect() should NOT match mypy's NDJSON format.
        let ruff_result = crate::run::parsers::ruff::PARSER.detect(mypy_json);
        assert!(
            !ruff_result.matched(),
            "ruff should not claim mypy NDJSON output (mypy uses 'file'/'severity', not 'filename'/'location'/'row')"
        );
        // mypy.detect() should match.
        let mypy_result = PARSER.detect(mypy_json);
        assert!(
            mypy_result.matched(),
            "mypy should detect its own NDJSON output"
        );
    }
}
