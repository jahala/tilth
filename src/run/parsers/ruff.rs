use memchr::memmem;
use serde_json::Value;

use crate::run::types::{Counts, DetectResult, Diagnostic, Location, ParsedOutput, Severity};

use super::Parser;

pub static PARSER: RuffParser = RuffParser;

pub struct RuffParser;

impl Parser for RuffParser {
    fn name(&self) -> &'static str {
        "ruff"
    }

    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // JSON fingerprint: ruff JSON always has "code", "filename", AND "location"/"row".
        // Checking only "code"+"filename" is too weak — e.g. GitHub API errors match.
        // Adding "location" + "row" makes this specific to ruff's schema.
        let code_finder = memmem::Finder::new(b"\"code\"");
        let filename_finder = memmem::Finder::new(b"\"filename\"");
        let location_finder = memmem::Finder::new(b"\"location\"");
        let row_finder = memmem::Finder::new(b"\"row\"");
        if code_finder.find(bytes).is_some()
            && filename_finder.find(bytes).is_some()
            && location_finder.find(bytes).is_some()
            && row_finder.find(bytes).is_some()
        {
            // Accept both `[` (JSON array) and NDJSON (each line is a `{`).
            let trimmed = sample.trim_start();
            if trimmed.starts_with('[') {
                return DetectResult::SingleJson;
            }
            if trimmed.starts_with('{') {
                return DetectResult::NdJson;
            }
        }

        // Text fingerprint: `path.py:N:M: CODE message` where CODE is letter(s) + digits.
        if sample.lines().any(looks_like_ruff_text_line) {
            return DetectResult::Text;
        }

        DetectResult::NoMatch
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

/// Returns true if `line` looks like a Ruff text-format diagnostic line.
///
/// Format: `path/to/file.py:5:1: F401 [*] 'os' imported but unused`
/// The rule code is letter(s) followed by digits, e.g. `E501`, `F401`, `UP006`, `I001`.
fn looks_like_ruff_text_line(line: &str) -> bool {
    // Quick reject: must contain `: ` with a code-looking token after a `col:` prefix.
    let Some(colon_space) = line.find(": ") else {
        return false;
    };
    // The part before `: ` must look like `path:line:col`.
    let prefix = &line[..colon_space];
    let parts: Vec<&str> = prefix.rsplitn(3, ':').collect();
    // parts[0] = col, parts[1] = line, parts[2] = path (in rsplitn order)
    if parts.len() < 3 {
        return false;
    }
    if !parts[0].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !parts[1].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // After `: ` the next token should be a Ruff rule code: letters then digits.
    let after = line[colon_space + 2..].trim_start();
    let code_end = after.find([' ', '\t']).unwrap_or(after.len());
    let code = &after[..code_end];
    is_ruff_code(code)
}

/// Returns true if `s` looks like a Ruff rule code such as `E501`, `F401`, `UP006`, `I001`.
fn is_ruff_code(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let letters: usize = s.chars().take_while(char::is_ascii_alphabetic).count();
    if letters == 0 {
        return false;
    }
    let digits: usize = s[letters..]
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if digits == 0 {
        return false;
    }
    // Code must be entirely letters + digits (nothing else).
    letters + digits == s.len()
}

// ---------------------------------------------------------------------------
// JSON path
// ---------------------------------------------------------------------------

fn parse_json(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    let trimmed = input.trim();

    // Try as a JSON array first.
    if trimmed.starts_with('[') {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
            for item in &items {
                if let Some(diag) = extract_json_item(item) {
                    diagnostics.push(diag);
                }
            }
        }
    } else {
        // NDJSON: one JSON object per line.
        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(diag) = extract_json_item(&value) {
                diagnostics.push(diag);
            }
        }
    }

    let issue_count = diagnostics.len() as u32;
    let summary = build_summary(issue_count);

    ParsedOutput {
        tool: "ruff",
        summary,
        diagnostics,
        counts: Counts {
            warnings: issue_count,
            ..Counts::default()
        },
        duration_secs: None,
        raw_lines,
        raw_bytes,
    }
}

fn extract_json_item(item: &Value) -> Option<Diagnostic> {
    let code = item
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let message = item.get("message").and_then(Value::as_str)?.to_string();

    let filename = item
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let location = item.get("location").and_then(|loc| {
        let row = loc.get("row").and_then(Value::as_u64)? as u32;
        let column = loc.get("column").and_then(Value::as_u64).map(|c| c as u32);
        Some(Location {
            file: filename.clone(),
            line: row,
            column,
        })
    });

    Some(Diagnostic {
        severity: Severity::Warning,
        location,
        name: code,
        message,
        detail: None,
    })
}

// ---------------------------------------------------------------------------
// Text path
// ---------------------------------------------------------------------------

fn parse_text(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    for line in input.lines() {
        if let Some(diag) = parse_text_line(line) {
            diagnostics.push(diag);
        }
    }

    let issue_count = diagnostics.len() as u32;
    let summary = build_summary(issue_count);

    ParsedOutput {
        tool: "ruff",
        summary,
        diagnostics,
        counts: Counts {
            warnings: issue_count,
            ..Counts::default()
        },
        duration_secs: None,
        raw_lines,
        raw_bytes,
    }
}

/// Parse a single Ruff text diagnostic line.
///
/// Format: `src/main.py:5:1: F401 [*] 'os' imported but unused`
///
/// The `[*]` fix-availability marker is optional; it is stripped from the message.
fn parse_text_line(line: &str) -> Option<Diagnostic> {
    if !looks_like_ruff_text_line(line) {
        return None;
    }

    // Split `path:line:col` from `: CODE message`.
    let colon_space = line.find(": ")?;
    let location_part = &line[..colon_space];
    let rest = &line[colon_space + 2..];

    // Parse location: rsplitn to handle paths that contain colons on Windows.
    let parts: Vec<&str> = location_part.rsplitn(3, ':').collect();
    // parts[0]=col, parts[1]=line_num, parts[2]=file
    if parts.len() < 3 {
        return None;
    }
    let column: u32 = parts[0].trim().parse().ok()?;
    let line_num: u32 = parts[1].trim().parse().ok()?;
    let file = parts[2].trim().to_string();

    // Split code from message.
    let (code_str, message_raw) = rest.split_once(' ')?;
    let code = code_str.trim().to_string();

    // Strip optional fix-availability marker `[*]` or `[x]` at the start of the message.
    let message = if message_raw.trim_start().starts_with('[') {
        // Find the closing `]` and skip past it plus any whitespace.
        let s = message_raw.trim_start();
        if let Some(end) = s.find(']') {
            s[end + 1..].trim().to_string()
        } else {
            message_raw.trim().to_string()
        }
    } else {
        message_raw.trim().to_string()
    };

    Some(Diagnostic {
        severity: Severity::Warning,
        location: Some(Location {
            file,
            line: line_num,
            column: Some(column),
        }),
        name: code,
        message,
        detail: None,
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn build_summary(count: u32) -> String {
    if count == 0 {
        "no issues found".to_string()
    } else {
        format!("{count} issue(s)")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- detect ---

    #[test]
    fn detect_json() {
        let sample = r#"[{"code":"F401","filename":"src/main.py","message":"unused","location":{"row":5,"column":1},"url":""}]"#;
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_text() {
        let sample = "src/main.py:5:1: F401 `os` imported but unused\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects() {
        let sample = "some random\noutput with no ruff markers\n";
        assert!(!PARSER.detect(sample).matched());
    }

    // Bug 3 regression: ruff JSON fingerprint was too weak — only checked "code"+"filename".
    // A response like {"code": 404, "filename": "..."} would false-match.
    #[test]
    fn detect_rejects_generic_json_with_code_and_filename() {
        // Has "code" and "filename" but no "location"/"row" — NOT ruff output.
        let sample = r#"{"code": 404, "filename": "not_found.py", "message": "File not found"}"#;
        assert!(
            !PARSER.detect(sample).matched(),
            "should not match generic JSON that has code+filename but no location/row"
        );
    }

    #[test]
    fn detect_rejects_github_api_style_json() {
        // GitHub API error that happens to have "code" and "filename" but no location.
        let sample =
            r#"[{"code":"not_found","filename":"src/foo.py","resource":"File","field":"path"}]"#;
        assert!(
            !PARSER.detect(sample).matched(),
            "should not match GitHub API JSON lacking location/row"
        );
    }

    // --- JSON path ---

    #[test]
    fn parse_json() {
        let input = r#"[
  {
    "cell": null,
    "code": "F401",
    "filename": "src/main.py",
    "location": {"column": 1, "row": 5},
    "message": "`os` imported but unused",
    "url": "https://docs.astral.sh/ruff/rules/unused-import"
  },
  {
    "cell": null,
    "code": "E501",
    "filename": "src/utils.py",
    "location": {"column": 89, "row": 12},
    "message": "Line too long (120 > 88)",
    "url": "https://docs.astral.sh/ruff/rules/line-too-long"
  }
]"#;
        let out = PARSER.parse(input, DetectResult::SingleJson);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.name, "F401");
        assert_eq!(first.message, "`os` imported but unused");
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("src/main.py")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(5));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(1));

        let second = &out.diagnostics[1];
        assert_eq!(second.name, "E501");
        assert_eq!(
            second.location.as_ref().map(|l| l.file.as_str()),
            Some("src/utils.py")
        );

        assert_eq!(out.counts.warnings, 2);
        assert_eq!(out.summary, "2 issue(s)");
    }

    // --- Text path ---

    #[test]
    fn parse_text() {
        let input = "src/main.py:5:1: F401 [*] `os` imported but unused\nsrc/utils.py:12:89: E501 Line too long (120 > 88)\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.name, "F401");
        assert_eq!(first.message, "`os` imported but unused");
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("src/main.py")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(5));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(1));

        let second = &out.diagnostics[1];
        assert_eq!(second.name, "E501");
        assert_eq!(second.message, "Line too long (120 > 88)");
        assert_eq!(second.location.as_ref().map(|l| l.line), Some(12));

        assert_eq!(out.counts.warnings, 2);
        assert_eq!(out.summary, "2 issue(s)");
    }
}
