use memchr::memmem;
use serde_json::Value;

use crate::run::types::{Counts, DetectResult, Diagnostic, Location, ParsedOutput, Severity};

use super::Parser;

pub static PARSER: EslintParser = EslintParser;

pub struct EslintParser;

impl Parser for EslintParser {
    fn name(&self) -> &'static str {
        "eslint"
    }

    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // JSON fingerprint: array containing objects with "filePath" and "messages".
        let file_path_finder = memmem::Finder::new(b"\"filePath\"");
        let messages_finder = memmem::Finder::new(b"\"messages\"");
        if sample.trim_start().starts_with('[')
            && file_path_finder.find(bytes).is_some()
            && messages_finder.find(bytes).is_some()
        {
            return DetectResult::SingleJson;
        }

        // Text fingerprint: indented lines with `error` or `warning` and a rule containing `/`
        // or a well-known bare rule name pattern.
        if sample.lines().any(looks_like_eslint_text_line) {
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

/// Returns true if `line` looks like an ESLint text-format diagnostic line.
///
/// ESLint text lines are indented and have the form:
///   `  10:5  error  'x' is defined but never used  no-unused-vars`
///   `  15:3  warning  Missing semicolon  semi`
#[allow(clippy::doc_markdown)] // "ESLint" is a tool name, not a code item
fn looks_like_eslint_text_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    // Must be indented (original line starts with whitespace).
    if trimmed.is_empty() || !line.starts_with(' ') {
        return false;
    }
    // First token must be `line:col`.
    let Some((loc, rest)) = trimmed.split_once("  ") else {
        return false;
    };
    let loc = loc.trim();
    if !loc.contains(':') || !loc.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    let rest = rest.trim_start();
    // Next token must be `error` or `warning`.
    let severity_word = if rest.starts_with("error") {
        "error"
    } else if rest.starts_with("warning") {
        "warning"
    } else {
        return false;
    };
    // After severity there should be at least two more whitespace-separated fields.
    // The last field is the rule ID.  A rule ID either contains `/` (scoped: @scope/rule)
    // or is a plain identifier.  We confirm the line has content after the severity word.
    let after_severity = rest[severity_word.len()..].trim_start();
    !after_severity.is_empty()
}

// ---------------------------------------------------------------------------
// JSON path
// ---------------------------------------------------------------------------

fn parse_json(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;

    let Ok(Value::Array(files)) = serde_json::from_str::<Value>(input.trim()) else {
        return empty_output(raw_lines, raw_bytes);
    };

    for file_obj in &files {
        let file_path = file_obj
            .get("filePath")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let Some(messages) = file_obj.get("messages").and_then(Value::as_array) else {
            continue;
        };

        for msg in messages {
            let Some(diag) = extract_json_message(msg, &file_path) else {
                continue;
            };
            match diag.severity {
                Severity::Error => error_count += 1,
                Severity::Warning => warning_count += 1,
                Severity::Info => {}
            }
            diagnostics.push(diag);
        }
    }

    let summary = build_summary(error_count, warning_count);

    ParsedOutput {
        tool: "eslint",
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

fn extract_json_message(msg: &Value, file_path: &str) -> Option<Diagnostic> {
    let message = msg.get("message").and_then(Value::as_str)?.to_string();

    // severity: 2 = Error, 1 = Warning
    let severity = match msg.get("severity").and_then(Value::as_u64).unwrap_or(1) {
        2 => Severity::Error,
        _ => Severity::Warning,
    };

    let name = msg
        .get("ruleId")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let line = msg.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
    let column = msg.get("column").and_then(Value::as_u64).map(|c| c as u32);

    let location = if !file_path.is_empty() && line > 0 {
        Some(Location {
            file: file_path.to_string(),
            line,
            column,
        })
    } else {
        None
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

fn parse_text(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;
    let mut current_file = String::new();

    for line in input.lines() {
        // File header: non-indented, non-empty line that is not the summary.
        if !line.starts_with(' ') && !line.is_empty() {
            // Skip the summary line (e.g. "✖ 2 problems (1 error, 1 warning)").
            if !line
                .trim_start_matches('\u{2716}')
                .trim()
                .starts_with(|c: char| c.is_ascii_digit())
            {
                current_file = line.trim().to_string();
            }
            continue;
        }

        let Some(diag) = parse_text_line(line, &current_file) else {
            continue;
        };
        match diag.severity {
            Severity::Error => error_count += 1,
            Severity::Warning => warning_count += 1,
            Severity::Info => {}
        }
        diagnostics.push(diag);
    }

    let summary = build_summary(error_count, warning_count);

    ParsedOutput {
        tool: "eslint",
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

/// Parse a single indented `ESLint` text diagnostic line.
///
/// Expected format: `  10:5  error  'x' is defined but never used  no-unused-vars`
fn parse_text_line(line: &str, file: &str) -> Option<Diagnostic> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return None;
    }

    // Split on two-or-more spaces to extract fields robustly.
    // Fields: loc, severity, message, rule_id
    let fields: Vec<&str> = trimmed
        .splitn(4, "  ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if fields.len() < 3 {
        return None;
    }

    // Field 0: `line:col`
    let loc = fields[0];
    let (line_num, column) = parse_location(loc)?;

    // Field 1: severity word
    let severity = match fields[1] {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => return None,
    };

    // Fields 2...: message and optional rule ID (last field).
    // With splitn(4, ..) fields[2] is the message, fields[3] (if present) is the rule.
    let (message, name) = if fields.len() >= 4 {
        (fields[2].to_string(), fields[3].to_string())
    } else {
        // No rule ID — message is fields[2], name unknown.
        (fields[2].to_string(), "unknown".to_string())
    };

    let location = if file.is_empty() {
        None
    } else {
        Some(Location {
            file: file.to_string(),
            line: line_num,
            column: Some(column),
        })
    };

    Some(Diagnostic {
        severity,
        location,
        name,
        message,
        detail: None,
    })
}

/// Parse `line:col` into `(line, col)`.
fn parse_location(loc: &str) -> Option<(u32, u32)> {
    let (line_str, col_str) = loc.split_once(':')?;
    let line: u32 = line_str.trim().parse().ok()?;
    let col: u32 = col_str.trim().parse().ok()?;
    Some((line, col))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn build_summary(errors: u32, warnings: u32) -> String {
    if errors == 0 && warnings == 0 {
        "no issues found".to_string()
    } else {
        format!("{errors} error(s), {warnings} warning(s)")
    }
}

fn empty_output(raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    ParsedOutput {
        tool: "eslint",
        summary: "no issues found".to_string(),
        diagnostics: Vec::new(),
        counts: Counts::default(),
        duration_secs: None,
        raw_lines,
        raw_bytes,
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
        let sample = r#"[{"filePath":"/src/app.js","messages":[]}]"#;
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_text() {
        let sample = "/src/app.js\n  10:5  error  'x' is defined but never used  no-unused-vars\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects() {
        let sample = "some random\noutput with no eslint markers\n";
        assert!(!PARSER.detect(sample).matched());
    }

    // --- JSON path ---

    #[test]
    fn parse_json() {
        let input = r#"[{
  "filePath": "/path/to/file.js",
  "messages": [
    {
      "severity": 2,
      "line": 10,
      "column": 5,
      "ruleId": "no-unused-vars",
      "message": "'x' is defined but never used."
    },
    {
      "severity": 1,
      "line": 15,
      "column": 3,
      "ruleId": "@typescript-eslint/no-explicit-any",
      "message": "Unexpected any."
    }
  ]
}]"#;
        let out = PARSER.parse(input, DetectResult::SingleJson);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.name, "no-unused-vars");
        assert_eq!(first.severity, Severity::Error);
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("/path/to/file.js")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(10));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(5));

        let second = &out.diagnostics[1];
        assert_eq!(second.name, "@typescript-eslint/no-explicit-any");
        assert_eq!(second.severity, Severity::Warning);

        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.counts.warnings, 1);
        assert_eq!(out.summary, "1 error(s), 1 warning(s)");
    }

    // --- Text path ---

    #[test]
    fn parse_text() {
        let input = "/path/to/file.js\n  10:5  error  'x' is defined but never used  no-unused-vars\n  15:3  warning  Unexpected any  @typescript-eslint/no-explicit-any\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.name, "no-unused-vars");
        assert_eq!(first.severity, Severity::Error);
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("/path/to/file.js")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(10));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(5));

        let second = &out.diagnostics[1];
        assert_eq!(second.severity, Severity::Warning);
        assert_eq!(second.name, "@typescript-eslint/no-explicit-any");

        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.counts.warnings, 1);
        assert_eq!(out.summary, "1 error(s), 1 warning(s)");
    }
}
