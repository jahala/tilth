use memchr::memmem;
use serde_json::Value;

use crate::run::types::{Counts, DetectResult, Diagnostic, Location, ParsedOutput, Severity};

use super::Parser;

pub static PARSER: GolangciLintParser = GolangciLintParser;

pub struct GolangciLintParser;

impl Parser for GolangciLintParser {
    fn name(&self) -> &'static str {
        "golangci-lint"
    }

    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // JSON fingerprint (single-object): `"Issues"` and `"Severity"` both present.
        let issues_finder = memmem::Finder::new(b"\"Issues\"");
        let severity_finder = memmem::Finder::new(b"\"Severity\"");
        if issues_finder.find(bytes).is_some() && severity_finder.find(bytes).is_some() {
            return DetectResult::SingleJson;
        }

        // JSON fingerprint (per-line): lines with `"FromLinter"` and `"Text"`.
        let from_linter_finder = memmem::Finder::new(b"\"FromLinter\"");
        let text_finder = memmem::Finder::new(b"\"Text\"");
        if from_linter_finder.find(bytes).is_some() && text_finder.find(bytes).is_some() {
            return DetectResult::NdJson;
        }

        // Text fingerprint: `.go:N:M: linterName: message` — look for `.go:` with a digit after.
        let go_finder = memmem::Finder::new(b".go:");
        if go_finder.find(bytes).is_none() {
            return DetectResult::NoMatch;
        }
        // Confirm at least one line looks like a real golangci-lint text line.
        if sample.lines().any(looks_like_lint_line) {
            DetectResult::Text
        } else {
            DetectResult::NoMatch
        }
    }

    fn parse(&self, input: &str, hint: DetectResult) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        if hint.is_json() {
            try_json(input, raw_lines, raw_bytes)
        } else {
            parse_text(input, raw_lines, raw_bytes)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns true if `line` matches `path.go:N:M: linterName: message` (with or without column).
fn looks_like_lint_line(line: &str) -> bool {
    // Must contain `.go:`
    let Some(go_pos) = line.find(".go:") else {
        return false;
    };
    let after_go = &line[go_pos + 4..]; // skip ".go:"
                                        // Expect a digit immediately after `.go:`
    let Some(first) = after_go.chars().next() else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    // Must have at least two `: ` separators after the path (location + linter name + message)
    after_go.matches(": ").count() >= 2
}

// ---------------------------------------------------------------------------
// JSON path
// ---------------------------------------------------------------------------

fn try_json(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;

    // Try parsing as a single JSON object first (standard golangci-lint JSON output).
    let trimmed = input.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(issues) = value.get("Issues").and_then(Value::as_array) {
            for issue in issues {
                if let Some(diag) = extract_json_issue(issue) {
                    match diag.severity {
                        Severity::Error => error_count += 1,
                        Severity::Warning => warning_count += 1,
                        Severity::Info => {}
                    }
                    diagnostics.push(diag);
                }
            }
        }
    } else {
        // Fall back to per-line JSON (one JSON object per line).
        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(diag) = extract_json_issue(&value) {
                match diag.severity {
                    Severity::Error => error_count += 1,
                    Severity::Warning => warning_count += 1,
                    Severity::Info => {}
                }
                diagnostics.push(diag);
            }
        }
    }

    let linter_count = count_unique_linters(&diagnostics);
    let summary = build_summary(diagnostics.len(), linter_count);

    ParsedOutput {
        tool: "golangci-lint",
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

/// Extract a `Diagnostic` from a single golangci-lint JSON issue object.
fn extract_json_issue(issue: &Value) -> Option<Diagnostic> {
    let from_linter = issue
        .get("FromLinter")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let message = issue.get("Text").and_then(Value::as_str)?.to_string();

    let severity_str = issue
        .get("Severity")
        .and_then(Value::as_str)
        .unwrap_or("warning");
    let severity = if severity_str == "error" {
        Severity::Error
    } else {
        Severity::Warning
    };

    let location = issue.get("Pos").and_then(|pos| {
        let file = pos.get("Filename")?.as_str()?.to_string();
        let line = pos.get("Line")?.as_u64()? as u32;
        let column = pos.get("Column").and_then(Value::as_u64).map(|c| c as u32);
        Some(Location { file, line, column })
    });

    Some(Diagnostic {
        severity,
        location,
        name: from_linter,
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

    for line in input.lines() {
        if let Some(diag) = parse_lint_line(line) {
            match diag.severity {
                Severity::Error => error_count += 1,
                Severity::Warning => warning_count += 1,
                Severity::Info => {}
            }
            diagnostics.push(diag);
        }
    }

    let linter_count = count_unique_linters(&diagnostics);
    let summary = build_summary(diagnostics.len(), linter_count);

    ParsedOutput {
        tool: "golangci-lint",
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

/// Parse a single golangci-lint text line.
///
/// Formats handled:
/// - `path/to/file.go:N:M: linterName: message`
/// - `path/to/file.go:N: linterName: message`
fn parse_lint_line(line: &str) -> Option<Diagnostic> {
    if !looks_like_lint_line(line) {
        return None;
    }

    let go_pos = line.find(".go:")?;
    let path = &line[..go_pos + 3]; // up to and including ".go"
    let rest = &line[go_pos + 4..]; // after ".go:"

    // rest is now `N:M: linterName: message` or `N: linterName: message`
    // Split off the line number.
    let (line_num_str, after_line) = rest.split_once(':')?;
    let line_num: u32 = line_num_str.trim().parse().ok()?;

    // Check whether next token is a column number or the linter name.
    let (column, after_loc) = if let Some((maybe_col, remainder)) = after_line.split_once(':') {
        let maybe_col = maybe_col.trim();
        if let Ok(col) = maybe_col.parse::<u32>() {
            // `M: linterName: message`
            (Some(col), remainder)
        } else {
            // No column — `maybe_col` is linter name start; treat after_line as `: linterName: message`
            (None, after_line)
        }
    } else {
        return None;
    };

    // after_loc is ` linterName: message` (with leading space after the colon)
    let after_loc = after_loc.trim_start_matches(' ');
    let (linter_name, message) = after_loc.split_once(": ")?;
    let linter_name = linter_name.trim().to_string();
    let message = message.trim().to_string();

    if linter_name.is_empty() || message.is_empty() {
        return None;
    }

    Some(Diagnostic {
        severity: Severity::Warning,
        location: Some(Location {
            file: path.to_string(),
            line: line_num,
            column,
        }),
        name: linter_name,
        message,
        detail: None,
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn count_unique_linters(diagnostics: &[Diagnostic]) -> usize {
    let mut names: Vec<&str> = diagnostics.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    names.len()
}

fn build_summary(issue_count: usize, linter_count: usize) -> String {
    if issue_count == 0 {
        "no issues found".to_string()
    } else {
        format!("{issue_count} issue(s) from {linter_count} linter(s)")
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
    fn detect_json_fingerprint() {
        let sample = r#"{ "Issues": [], "Severity": "warning" }"#;
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_text_fingerprint() {
        let sample = "pkg/foo/bar.go:42:5: govet: printf: wrong type\npkg/foo/bar.go:10:1: errcheck: unchecked error\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "some random\noutput\nwith no golang lint markers\n";
        assert!(!PARSER.detect(sample).matched());
    }

    // --- JSON path ---

    #[test]
    fn parse_json_issues() {
        let input = r#"{
  "Issues": [
    {
      "FromLinter": "govet",
      "Text": "printf: fmt.Sprintf format %d has arg str of wrong type string",
      "Severity": "warning",
      "Pos": { "Filename": "main.go", "Line": 42, "Column": 5 }
    },
    {
      "FromLinter": "errcheck",
      "Text": "Error return value of `os.Remove` is not checked",
      "Severity": "error",
      "Pos": { "Filename": "cmd/root.go", "Line": 17, "Column": 2 }
    }
  ]
}"#;
        let out = PARSER.parse(input, DetectResult::SingleJson);
        assert_eq!(out.diagnostics.len(), 2);

        let govet = &out.diagnostics[0];
        assert_eq!(govet.name, "govet");
        assert_eq!(govet.severity, Severity::Warning);
        assert_eq!(
            govet.location.as_ref().map(|l| l.file.as_str()),
            Some("main.go")
        );
        assert_eq!(govet.location.as_ref().map(|l| l.line), Some(42));
        assert_eq!(govet.location.as_ref().and_then(|l| l.column), Some(5));

        let errcheck = &out.diagnostics[1];
        assert_eq!(errcheck.name, "errcheck");
        assert_eq!(errcheck.severity, Severity::Error);
        assert_eq!(out.counts.errors, 1);
        assert_eq!(out.counts.warnings, 1);
        assert_eq!(out.summary, "2 issue(s) from 2 linter(s)");
    }

    #[test]
    fn parse_json_no_issues() {
        let input = r#"{ "Issues": [] }"#;
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 0);
        assert_eq!(out.summary, "no issues found");
    }

    // --- Text path ---

    #[test]
    fn parse_text_lint_lines() {
        let input = "main.go:42:5: govet: printf: wrong type\ncmd/root.go:17:2: errcheck: unchecked error\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 2);

        let first = &out.diagnostics[0];
        assert_eq!(first.name, "govet");
        assert_eq!(first.message, "printf: wrong type");
        assert_eq!(
            first.location.as_ref().map(|l| l.file.as_str()),
            Some("main.go")
        );
        assert_eq!(first.location.as_ref().map(|l| l.line), Some(42));
        assert_eq!(first.location.as_ref().and_then(|l| l.column), Some(5));

        let second = &out.diagnostics[1];
        assert_eq!(second.name, "errcheck");
        assert_eq!(second.location.as_ref().map(|l| l.line), Some(17));
    }

    #[test]
    fn parse_text_groups_by_linter() {
        let input =
            "a.go:1:1: govet: msg one\nb.go:2:3: govet: msg two\nc.go:5:1: errcheck: msg three\n";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 3);
        // Two govet issues, one errcheck — 2 distinct linters.
        assert_eq!(out.summary, "3 issue(s) from 2 linter(s)");
        // All are warnings (text path has no severity info, defaults to Warning).
        assert_eq!(out.counts.warnings, 3);
        assert_eq!(out.counts.errors, 0);
    }
}
