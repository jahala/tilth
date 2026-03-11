use memchr::memmem;
use serde_json::Value;

use crate::run::types::{Counts, Diagnostic, Location, ParsedOutput, Severity};

use super::Parser;

pub static PARSER: CargoBuildParser = CargoBuildParser;

pub struct CargoBuildParser;

impl Parser for CargoBuildParser {
    fn name(&self) -> &'static str {
        "cargo-build"
    }

    fn detect(&self, sample: &str) -> bool {
        let bytes = sample.as_bytes();

        // JSON fingerprint: any line contains `"reason":"compiler-` or `"reason": "compiler-`
        let json_finder_compact = memmem::Finder::new(b"\"reason\":\"compiler-");
        let json_finder_spaced = memmem::Finder::new(b"\"reason\": \"compiler-");
        if json_finder_compact.find(bytes).is_some() || json_finder_spaced.find(bytes).is_some() {
            return true;
        }

        // Text fingerprint: `error[E` or `warning[` or `warning:` with ` --> ` arrow
        let arrow_finder = memmem::Finder::new(b" --> ");
        if arrow_finder.find(bytes).is_none() {
            return false;
        }
        let error_code = memmem::Finder::new(b"error[E");
        let warning_bracket = memmem::Finder::new(b"warning[");
        let warning_colon = memmem::Finder::new(b"warning:");
        error_code.find(bytes).is_some()
            || warning_bracket.find(bytes).is_some()
            || warning_colon.find(bytes).is_some()
    }

    fn parse(&self, input: &str) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        // Try JSON path first; fall back to text parsing.
        if looks_like_json(input) {
            try_json(input, raw_lines, raw_bytes)
        } else {
            parse_text(input, raw_lines, raw_bytes)
        }
    }
}

/// Heuristic: treat as JSON if any of the first 5 non-empty lines starts with `{`.
fn looks_like_json(input: &str) -> bool {
    input
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(5)
        .any(|l| l.trim_start().starts_with('{'))
}

// ---------------------------------------------------------------------------
// JSON path
// ---------------------------------------------------------------------------

fn try_json(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;
    let mut build_success: Option<bool> = None;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let reason = match value.get("reason").and_then(Value::as_str) {
            Some(r) => r,
            None => continue,
        };

        match reason {
            "compiler-message" => {
                if let Some(msg_obj) = value.get("message") {
                    if let Some(diag) = extract_json_diagnostic(msg_obj) {
                        match diag.severity {
                            Severity::Error => error_count += 1,
                            Severity::Warning => warning_count += 1,
                            Severity::Info => {}
                        }
                        diagnostics.push(diag);
                    }
                }
            }
            "build-finished" => {
                build_success = value.get("success").and_then(Value::as_bool);
            }
            // compiler-artifact, build-script-executed, etc.: skip
            _ => {}
        }
    }

    let summary = build_summary(error_count, warning_count, build_success);

    ParsedOutput {
        tool: "cargo-build",
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

/// Extract a `Diagnostic` from a compiler-message `.message` object.
fn extract_json_diagnostic(msg: &Value) -> Option<Diagnostic> {
    let level_str = msg.get("level").and_then(Value::as_str).unwrap_or("info");
    let severity = match level_str {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    };

    let message = msg
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // Use the error code (e.g., "E0308") as name; fall back to a truncated message.
    let name = msg
        .get("code")
        .and_then(|c| c.get("code"))
        .and_then(Value::as_str)
        .unwrap_or(&message)
        .to_string();

    // Primary span -> location.
    let location = msg
        .get("spans")
        .and_then(Value::as_array)
        .and_then(|spans| {
            spans.iter().find(|s| {
                s.get("is_primary")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
        })
        .and_then(|span| {
            let file = span.get("file_name")?.as_str()?.to_string();
            let line = span.get("line_start")?.as_u64()? as u32;
            let column = span
                .get("column_start")
                .and_then(Value::as_u64)
                .map(|c| c as u32);
            Some(Location { file, line, column })
        });

    // Collect help children into detail.
    let detail: Option<String> = msg
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter(|c| {
                    c.get("level")
                        .and_then(Value::as_str)
                        .map(|l| l == "help")
                        .unwrap_or(false)
                })
                .filter_map(|c| c.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty());

    Some(Diagnostic {
        severity,
        location,
        name,
        message,
        detail,
    })
}

// ---------------------------------------------------------------------------
// Text path
// ---------------------------------------------------------------------------

/// State machine for accumulating a single diagnostic block from text output.
struct TextDiag {
    severity: Severity,
    name: String,
    message: String,
    location: Option<Location>,
    help_lines: Vec<String>,
}

impl TextDiag {
    fn finish(self) -> Diagnostic {
        let detail = if self.help_lines.is_empty() {
            None
        } else {
            Some(self.help_lines.join("\n"))
        };
        Diagnostic {
            severity: self.severity,
            location: self.location,
            name: self.name,
            message: self.message,
            detail,
        }
    }
}

fn parse_text(input: &str, raw_lines: usize, raw_bytes: usize) -> ParsedOutput {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut current: Option<TextDiag> = None;
    let mut error_count: u32 = 0;
    let mut warning_count: u32 = 0;

    for line in input.lines() {
        // Skip visual noise: lines with `|` decorations and `^` underlines.
        if is_decoration_line(line) {
            continue;
        }

        // Summary lines — skip or finalize without adding a duplicate diagnostic.
        if line.starts_with("error: could not compile") {
            // Flush current if any; don't add a new diagnostic for this summary.
            if let Some(d) = current.take() {
                push_diag(&mut diagnostics, &mut error_count, &mut warning_count, d);
            }
            continue;
        }
        if line.starts_with("error: aborting due to") {
            continue;
        }

        // `error[EXXXX]: message` or `warning[CXXXX]: message`
        if let Some((sev, name, message)) = parse_diagnostic_header(line) {
            if let Some(d) = current.take() {
                push_diag(&mut diagnostics, &mut error_count, &mut warning_count, d);
            }
            current = Some(TextDiag {
                severity: sev,
                name,
                message,
                location: None,
                help_lines: Vec::new(),
            });
            continue;
        }

        // `warning: message` without a bracketed code
        if let Some(message) = parse_plain_warning(line) {
            if let Some(d) = current.take() {
                push_diag(&mut diagnostics, &mut error_count, &mut warning_count, d);
            }
            current = Some(TextDiag {
                severity: Severity::Warning,
                name: message.clone(),
                message,
                location: None,
                help_lines: Vec::new(),
            });
            continue;
        }

        // ` --> src/lib.rs:42:5`
        if let Some(loc) = parse_arrow_location(line) {
            if let Some(ref mut d) = current {
                if d.location.is_none() {
                    d.location = Some(loc);
                }
            }
            continue;
        }

        // `help: suggestion`
        if let Some(help) = parse_help_line(line) {
            if let Some(ref mut d) = current {
                d.help_lines.push(help);
            }
            continue;
        }
    }

    // Flush final diagnostic.
    if let Some(d) = current.take() {
        push_diag(&mut diagnostics, &mut error_count, &mut warning_count, d);
    }

    let summary = build_summary(error_count, warning_count, None);

    ParsedOutput {
        tool: "cargo-build",
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

fn push_diag(
    diagnostics: &mut Vec<Diagnostic>,
    error_count: &mut u32,
    warning_count: &mut u32,
    d: TextDiag,
) {
    match d.severity {
        Severity::Error => *error_count += 1,
        Severity::Warning => *warning_count += 1,
        Severity::Info => {}
    }
    diagnostics.push(d.finish());
}

/// Returns `(severity, name, message)` for lines like:
/// `error[E0308]: mismatched types`
/// `warning[clippy::needless_return]: needless return`
fn parse_diagnostic_header(line: &str) -> Option<(Severity, String, String)> {
    let (sev, rest) = if let Some(r) = line.strip_prefix("error[") {
        (Severity::Error, r)
    } else if let Some(r) = line.strip_prefix("warning[") {
        (Severity::Warning, r)
    } else {
        return None;
    };

    // Find the closing `]:`
    let bracket_end = rest.find("]:")?;
    let code = rest[..bracket_end].to_string();
    let message = rest[bracket_end + 2..].trim().to_string();

    Some((sev, code, message))
}

/// Matches `warning: text` but NOT `warning[...]`.
fn parse_plain_warning(line: &str) -> Option<String> {
    let rest = line.strip_prefix("warning: ")?;
    // Exclude the bracketed variant — that's handled by parse_diagnostic_header.
    if line.starts_with("warning[") {
        return None;
    }
    Some(rest.trim().to_string())
}

/// Parses ` --> file:line:col` or ` --> file:line`.
fn parse_arrow_location(line: &str) -> Option<Location> {
    let rest = line.trim().strip_prefix("--> ")?;
    parse_location_str(rest.trim())
}

/// Parses `file:line:col` or `file:line` into a Location.
fn parse_location_str(s: &str) -> Option<Location> {
    // Split on ':' — file may be a Windows path like C:\..., but we're on Unix.
    let mut parts = s.splitn(3, ':');
    let file = parts.next()?.trim().to_string();
    if file.is_empty() {
        return None;
    }
    let line: u32 = parts.next()?.trim().parse().ok()?;
    let column: Option<u32> = parts.next().and_then(|c| c.trim().parse().ok());
    Some(Location { file, line, column })
}

/// Parses `help: suggestion text`.
fn parse_help_line(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("help: ")?;
    Some(rest.trim().to_string())
}

/// Returns true for lines that are visual noise from rustc's output renderer:
/// lines that consist mainly of pipe characters, caret underlines, and whitespace.
fn is_decoration_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Lines that start with `|` (with optional leading whitespace and line numbers)
    // or consist only of `^`, `~`, `-`, `=` mixed with spaces are decoration.
    let stripped = trimmed
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start();
    if stripped.starts_with('|') {
        return true;
    }
    // Pure underline lines: all chars are `^`, `~`, `-`, ` `, `_`
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| matches!(c, '^' | '~' | '-' | ' ' | '_'))
        && trimmed.contains('^')
    {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn build_summary(errors: u32, warnings: u32, success: Option<bool>) -> String {
    if errors == 0 && success != Some(false) {
        if warnings == 0 {
            "build succeeded".to_string()
        } else {
            format!("build succeeded, {warnings} warning(s)")
        }
    } else {
        format!("{errors} error(s), {warnings} warning(s)")
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
        let sample = r#"{"reason":"compiler-message","package_id":"foo"}"#;
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_json_fingerprint_spaced() {
        let sample = r#"{"reason": "compiler-message", "package_id": "foo"}"#;
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_text_fingerprint() {
        let sample = "error[E0308]: mismatched types\n --> src/lib.rs:10:5\n";
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "some random\noutput\nwith no rust compiler markers\n";
        assert!(!PARSER.detect(sample));
    }

    // --- JSON path ---

    #[test]
    fn parse_json_compiler_message() {
        let input = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","manifest_path":"/foo/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"foo","src_path":"/foo/src/lib.rs","edition":"2021"},"message":{"message":"mismatched types","code":{"code":"E0308","explanation":"..."},"level":"error","spans":[{"file_name":"src/lib.rs","byte_start":100,"byte_end":110,"line_start":10,"line_end":10,"column_start":5,"column_end":15,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"rendered":"error[E0308]: mismatched types\n"}}"#;

        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(diag.name, "E0308");
        assert_eq!(diag.message, "mismatched types");
        assert_eq!(
            diag.location.as_ref().map(|l| l.file.as_str()),
            Some("src/lib.rs")
        );
        assert_eq!(diag.location.as_ref().map(|l| l.line), Some(10));
        assert_eq!(diag.location.as_ref().and_then(|l| l.column), Some(5));
        assert_eq!(out.counts.errors, 1);
    }

    #[test]
    fn parse_json_with_help() {
        let input = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","manifest_path":"/foo/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"foo","src_path":"/foo/src/lib.rs","edition":"2021"},"message":{"message":"unused variable: `x`","code":{"code":"unused_variables","explanation":null},"level":"warning","spans":[{"file_name":"src/lib.rs","byte_start":50,"byte_end":51,"line_start":5,"line_end":5,"column_start":9,"column_end":10,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[{"message":"if this is intentional, prefix it with an underscore: `_x`","code":null,"level":"help","spans":[],"children":[],"rendered":null}],"rendered":"warning: unused variable: `x`\n"}}"#;

        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Warning);
        assert!(diag.detail.is_some());
        assert!(diag
            .detail
            .as_ref()
            .unwrap()
            .contains("prefix it with an underscore"));
        assert_eq!(out.counts.warnings, 1);
    }

    // --- Text path ---

    #[test]
    fn parse_text_error_with_location() {
        let input = "error[E0308]: mismatched types\n --> src/lib.rs:42:5\n  |\n42 |     let x: i32 = \"hello\";\n  |                  ^^^^^^^ expected `i32`, found `&str`\n";
        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(diag.name, "E0308");
        assert_eq!(diag.message, "mismatched types");
        assert_eq!(
            diag.location.as_ref().map(|l| l.file.as_str()),
            Some("src/lib.rs")
        );
        assert_eq!(diag.location.as_ref().map(|l| l.line), Some(42));
        assert_eq!(diag.location.as_ref().and_then(|l| l.column), Some(5));
    }

    #[test]
    fn parse_text_warning() {
        let input = "warning[unused_variables]: unused variable: `x`\n --> src/main.rs:7:9\n  |\n7 |     let x = 5;\n  |         ^ help: if this is intentional, prefix it with an underscore: `_x`\n  |\n";
        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.name, "unused_variables");
        assert_eq!(out.counts.warnings, 1);
    }

    #[test]
    fn parse_text_clippy_warning() {
        let input = "warning: redundant pattern matching\n --> src/lib.rs:15:5\n";
        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.name, "redundant pattern matching");
        assert_eq!(out.counts.warnings, 1);
    }
}
