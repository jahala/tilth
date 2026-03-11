use std::collections::HashMap;

use memchr::memmem;
use serde_json::Value;

use crate::run::types::{truncate_detail, Counts, Diagnostic, Location, ParsedOutput, Severity};

use super::Parser;

pub static PARSER: GoTestParser = GoTestParser;

pub struct GoTestParser;

impl Parser for GoTestParser {
    fn name(&self) -> &'static str {
        "go-test"
    }

    /// Detect `go test` output via byte scanning — no regex.
    ///
    /// Accepts JSON format (`"Action":"` + `"Package":"`) or text format
    /// (`--- FAIL:` or `--- PASS:`).
    fn detect(&self, sample: &str) -> bool {
        let bytes = sample.as_bytes();

        // JSON fingerprint: both "Action":" and "Package":" must appear.
        let action = memmem::Finder::new(r#""Action":""#);
        let package = memmem::Finder::new(r#""Package":""#);
        if action.find(bytes).is_some() && package.find(bytes).is_some() {
            return true;
        }

        // Text fingerprint: at least one of the canonical test result markers.
        let fail_marker = memmem::Finder::new("--- FAIL:");
        let pass_marker = memmem::Finder::new("--- PASS:");
        fail_marker.find(bytes).is_some() || pass_marker.find(bytes).is_some()
    }

    fn parse(&self, input: &str) -> ParsedOutput {
        // Try JSON (NDJSON) first — any line starting with `{` signals JSON mode.
        if input.lines().any(|l| l.trim_start().starts_with('{')) {
            if let Some(parsed) = self.try_json(input) {
                return parsed;
            }
        }
        self.parse_text(input)
    }
}

impl GoTestParser {
    /// Parse `go test -json` NDJSON output.
    ///
    /// Each line is an independent JSON object (event). We drive a per-test
    /// state machine keyed by `(Package, Test)` to accumulate output lines,
    /// then produce a `Diagnostic` on failure.
    fn try_json(&self, input: &str) -> Option<ParsedOutput> {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut passed: u32 = 0;
        let mut failed: u32 = 0;
        let mut skipped: u32 = 0;
        let mut duration_secs: f64 = 0.0;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Accumulated output lines per (package, test_name).
        let mut output_buf: HashMap<(String, String), Vec<String>> = HashMap::new();

        let events: Vec<Value> = input
            .lines()
            .filter(|l| l.trim_start().starts_with('{'))
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        if events.is_empty() {
            return None;
        }

        for event in &events {
            let action = event.get("Action").and_then(|v| v.as_str()).unwrap_or("");
            let pkg = event
                .get("Package")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let test_name = event.get("Test").and_then(|v| v.as_str());

            match action {
                "run" => {
                    if let Some(name) = test_name {
                        output_buf.entry((pkg, name.to_string())).or_default();
                    }
                }
                "output" => {
                    if let Some(name) = test_name {
                        let text = event
                            .get("Output")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        output_buf
                            .entry((pkg, name.to_string()))
                            .or_default()
                            .push(text);
                    }
                }
                "pass" => {
                    if let Some(elapsed) = event.get("Elapsed").and_then(|v| v.as_f64()) {
                        if test_name.is_some() {
                            duration_secs += elapsed;
                        }
                    }
                    if let Some(name) = test_name {
                        passed += 1;
                        output_buf.remove(&(pkg, name.to_string()));
                    }
                }
                "fail" => {
                    if let Some(elapsed) = event.get("Elapsed").and_then(|v| v.as_f64()) {
                        if test_name.is_some() {
                            duration_secs += elapsed;
                        }
                    }
                    if let Some(name) = test_name {
                        failed += 1;
                        let key = (pkg.clone(), name.to_string());
                        let lines = output_buf.remove(&key).unwrap_or_default();
                        let combined = lines.join("");
                        let message = extract_failure_message(&combined);
                        let location = Location::scan_text(&combined);
                        let detail = truncate_detail(&combined);
                        let diag_name = if pkg.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}::{}", pkg, name)
                        };
                        diagnostics.push(Diagnostic {
                            severity: Severity::Error,
                            location,
                            name: diag_name,
                            message,
                            detail,
                        });
                    }
                }
                "skip" => {
                    if let Some(name) = test_name {
                        skipped += 1;
                        output_buf.remove(&(pkg, name.to_string()));
                    }
                }
                _ => {}
            }
        }

        let duration = if duration_secs > 0.0 {
            Some(duration_secs)
        } else {
            None
        };
        let summary = build_summary(passed, failed, skipped);
        let counts = Counts {
            passed,
            failed,
            errors: failed,
            skipped,
            ..Counts::default()
        };

        Some(ParsedOutput {
            tool: "go-test",
            summary,
            diagnostics,
            counts,
            duration_secs: duration,
            raw_lines,
            raw_bytes,
        })
    }

    /// Parse plain `go test` text output (without `-json`).
    fn parse_text(&self, input: &str) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut passed: u32 = 0;
        let mut skipped: u32 = 0;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Current failure block being accumulated.
        let mut failure_name: Option<String> = None;
        let mut failure_lines: Vec<String> = Vec::new();

        for line in input.lines() {
            // --- FAIL: TestName (0.00s)
            if let Some(rest) = line.strip_prefix("--- FAIL: ") {
                // Flush any previous block first.
                if let Some(name) = failure_name.take() {
                    diagnostics.push(build_text_diagnostic(name, &failure_lines));
                    failure_lines.clear();
                }
                // Parse "TestName (0.00s)" — take everything before the first space.
                let name = rest.split_whitespace().next().unwrap_or(rest).to_string();
                failure_name = Some(name);
                continue;
            }

            // --- PASS: TestName (0.00s)
            if line.starts_with("--- PASS: ") {
                if let Some(name) = failure_name.take() {
                    diagnostics.push(build_text_diagnostic(name, &failure_lines));
                    failure_lines.clear();
                }
                passed += 1;
                continue;
            }

            // --- SKIP: TestName (0.00s)
            if line.starts_with("--- SKIP: ") {
                if let Some(name) = failure_name.take() {
                    diagnostics.push(build_text_diagnostic(name, &failure_lines));
                    failure_lines.clear();
                }
                skipped += 1;
                continue;
            }

            // FAIL\tpackage\t0.00s — package-level failure; already counted via --- FAIL lines.
            // ok  \tpackage\t0.00s — package passed; count sub-tests already tallied above.
            if line.starts_with("ok  \t") || line.starts_with("FAIL\t") {
                if let Some(name) = failure_name.take() {
                    diagnostics.push(build_text_diagnostic(name, &failure_lines));
                    failure_lines.clear();
                }
                // The package-level "ok" lines don't add to passed individually since
                // individual --- PASS lines already do; just ensure we close any open block.
                continue;
            }

            // Indented detail line belonging to the current failure block.
            if failure_name.is_some() {
                failure_lines.push(line.to_string());
            }
        }

        // Flush any trailing failure block.
        if let Some(name) = failure_name.take() {
            diagnostics.push(build_text_diagnostic(name, &failure_lines));
        }

        // Failure count comes directly from the collected diagnostics.
        let failed = diagnostics.len() as u32;

        let summary = build_summary(passed, failed, skipped);
        let counts = Counts {
            passed,
            failed,
            errors: failed,
            skipped,
            ..Counts::default()
        };

        ParsedOutput {
            tool: "go-test",
            summary,
            diagnostics,
            counts,
            duration_secs: None,
            raw_lines,
            raw_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_text_diagnostic(name: String, lines: &[String]) -> Diagnostic {
    let combined = lines.join("\n");
    let message = extract_failure_message(&combined);
    let location = Location::scan_text(&combined);
    let detail = truncate_detail(&combined);
    Diagnostic {
        severity: Severity::Error,
        location,
        name,
        message,
        detail,
    }
}

/// Build a human-readable summary, omitting zero categories except `passed`.
fn build_summary(passed: u32, failed: u32, skipped: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{passed} passed"));
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    parts.join(", ")
}

/// Extract the most useful failure message from accumulated output text.
///
/// Priority:
/// 1. Lines containing `Error:` or `error:` (testify-style)
/// 2. Lines matching `    file_test.go:N: message` (standard t.Error/t.Fatal format)
/// 3. First non-empty line
fn extract_failure_message(output: &str) -> String {
    // Testify / assertion errors: "Error: Not equal:" etc.
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Error:") || trimmed.starts_with("error:") {
            return trimmed.to_string();
        }
    }

    // Standard t.Errorf / t.Fatalf lines: "    file_test.go:42: the message"
    for line in output.lines() {
        let trimmed = line.trim();
        // Pattern: something like `foo_test.go:42: message`
        if let Some(msg) = extract_test_log_message(trimmed) {
            return msg;
        }
    }

    // Fallback: first non-empty line.
    output
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("test failed")
        .to_string()
}

/// Parse a `file_test.go:42: message` line and return just the message part.
fn extract_test_log_message(line: &str) -> Option<String> {
    // Find `filename:digits:` pattern and return the text after it.
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        // Check for digits immediately after colon.
        let num_start = i + 1;
        let mut num_end = num_start;
        while num_end < len && bytes[num_end].is_ascii_digit() {
            num_end += 1;
        }
        if num_end == num_start {
            i += 1;
            continue;
        }
        // The digits must be followed by `:` for the message separator.
        if num_end >= len || bytes[num_end] != b':' {
            i += 1;
            continue;
        }
        // Verify the token before the colon looks like a filename.
        let path_end = i;
        let path_start = bytes[..path_end]
            .iter()
            .rposition(|&b| b == b' ' || b == b'\t')
            .map(|p| p + 1)
            .unwrap_or(0);
        let path_bytes = &bytes[path_start..path_end];
        let looks_like_file = path_bytes.contains(&b'.');
        if !looks_like_file {
            i += 1;
            continue;
        }
        // Everything after "filename:digits:" is the message.
        let msg_start = num_end + 1;
        if msg_start < len {
            return Some(line[msg_start..].trim().to_string());
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Detection -----------------------------------------------------------

    #[test]
    fn detect_json_fingerprint() {
        let sample = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"example.com/pkg","Test":"TestFoo"}"#;
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_text_fingerprint() {
        let sample = "=== RUN   TestFoo\n--- FAIL: TestFoo (0.00s)\n    foo_test.go:10: expected 1 got 2\nFAIL\n";
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "Building foo...\nDone.\nAll steps completed.\n";
        assert!(!PARSER.detect(sample));
    }

    // -- JSON parse ----------------------------------------------------------

    #[test]
    fn parse_json_all_pass() {
        let input = concat!(
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestA\"}\n",
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"output\",\"Package\":\"ex/pkg\",\"Test\":\"TestA\",\"Output\":\"    foo_test.go:5: ok\\n\"}\n",
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"pass\",\"Package\":\"ex/pkg\",\"Test\":\"TestA\",\"Elapsed\":0.01}\n",
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestB\"}\n",
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"pass\",\"Package\":\"ex/pkg\",\"Test\":\"TestB\",\"Elapsed\":0.02}\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.tool, "go-test");
        assert_eq!(out.counts.passed, 2);
        assert_eq!(out.counts.failed, 0);
        assert_eq!(out.counts.skipped, 0);
        assert!(out.diagnostics.is_empty());
        assert!(out.summary.contains("2 passed"));
        assert!(!out.summary.contains("failed"));
    }

    #[test]
    fn parse_json_with_failure() {
        let input = concat!(
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestBad\"}\n",
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"output\",\"Package\":\"ex/pkg\",\"Test\":\"TestBad\",\"Output\":\"    foo_test.go:42: got 1 want 2\\n\"}\n",
            "{\"Time\":\"2024-01-01T00:00:00Z\",\"Action\":\"fail\",\"Package\":\"ex/pkg\",\"Test\":\"TestBad\",\"Elapsed\":0.05}\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.counts.failed, 1);
        assert_eq!(out.counts.passed, 0);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert!(diag.name.contains("TestBad"));
        assert_eq!(diag.severity, Severity::Error);
        assert!(!diag.message.is_empty());
        let loc = diag.location.as_ref().expect("location should be present");
        assert_eq!(loc.file, "foo_test.go");
        assert_eq!(loc.line, 42);
    }

    // -- Text parse ----------------------------------------------------------

    #[test]
    fn parse_text_failure() {
        let input = concat!(
            "=== RUN   TestAdd\n",
            "--- FAIL: TestAdd (0.00s)\n",
            "    math_test.go:15: expected 4, got 3\n",
            "FAIL\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "TestAdd");
        assert_eq!(diag.severity, Severity::Error);
        let loc = diag.location.as_ref().expect("location should be present");
        assert_eq!(loc.file, "math_test.go");
        assert_eq!(loc.line, 15);
    }

    #[test]
    fn parse_text_pass_summary() {
        let input = concat!(
            "=== RUN   TestOne\n",
            "--- PASS: TestOne (0.01s)\n",
            "=== RUN   TestTwo\n",
            "--- PASS: TestTwo (0.00s)\n",
            "ok  \texample.com/mypkg\t0.015s\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.counts.passed, 2);
        assert_eq!(out.counts.failed, 0);
        assert!(out.diagnostics.is_empty());
        assert!(out.summary.contains("2 passed"));
    }
}
