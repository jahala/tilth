use std::collections::HashMap;

use memchr::memmem;
use serde_json::Value;

use crate::run::types::{
    build_test_summary, truncate_detail, Counts, DetectResult, Diagnostic, Location, ParsedOutput,
    Severity,
};

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
    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // JSON fingerprint: both "Action":" and "Package":" must appear.
        let action = memmem::Finder::new(r#""Action":""#);
        let package = memmem::Finder::new(r#""Package":""#);
        if action.find(bytes).is_some() && package.find(bytes).is_some() {
            return DetectResult::NdJson;
        }

        // Text fingerprint: at least one of the canonical test result markers.
        let fail_marker = memmem::Finder::new("--- FAIL:");
        let pass_marker = memmem::Finder::new("--- PASS:");
        if fail_marker.find(bytes).is_some() || pass_marker.find(bytes).is_some() {
            return DetectResult::Text;
        }

        DetectResult::NoMatch
    }

    fn parse(&self, input: &str, hint: DetectResult) -> ParsedOutput {
        if hint.is_json() {
            if let Some(parsed) = GoTestParser::try_json(input) {
                return parsed;
            }
        }
        GoTestParser::parse_text(input)
    }
}

impl GoTestParser {
    /// Parse `go test -json` NDJSON output.
    ///
    /// Each line is an independent JSON object (event). We drive a per-test
    /// state machine keyed by `(Package, Test)` to accumulate output lines,
    /// then produce a `Diagnostic` on failure.
    fn try_json(input: &str) -> Option<ParsedOutput> {
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
                    if let Some(elapsed) = event.get("Elapsed").and_then(serde_json::Value::as_f64)
                    {
                        // Only count top-level tests (no '/' in name). Subtests have the form
                        // "TestFoo/subtest" — their time is already included in the parent's
                        // elapsed, so adding both would double-count.
                        if let Some(name) = test_name {
                            if !name.contains('/') {
                                duration_secs += elapsed;
                            }
                        }
                    }
                    if let Some(name) = test_name {
                        passed += 1;
                        output_buf.remove(&(pkg, name.to_string()));
                    }
                }
                "fail" => {
                    if let Some(elapsed) = event.get("Elapsed").and_then(serde_json::Value::as_f64)
                    {
                        // Same rule: only top-level test durations to avoid double-counting.
                        if let Some(name) = test_name {
                            if !name.contains('/') {
                                duration_secs += elapsed;
                            }
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
                            format!("{pkg}::{name}")
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
        let summary = build_test_summary(passed, failed, skipped);
        let counts = Counts {
            passed,
            failed,
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
    ///
    /// Go test output structure:
    /// ```text
    /// === RUN   TestFoo
    ///     foo_test.go:15: expected 4, got 3   -- detail comes BEFORE the result line
    /// --- FAIL: TestFoo (0.00s)
    /// ```
    ///
    /// Lines are accumulated per-test (keyed by `=== RUN` name). On `--- FAIL:` we
    /// flush the accumulated lines as a diagnostic. On `--- PASS:` / `--- SKIP:`
    /// we discard them.
    fn parse_text(input: &str) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut passed: u32 = 0;
        let mut skipped: u32 = 0;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Lines accumulated since the last `=== RUN`, keyed by test name.
        let mut current_name: Option<String> = None;
        let mut current_lines: Vec<String> = Vec::new();

        // Duration: prefer the package-level `ok`/`FAIL` line time.
        // Accumulate top-level (non-subtest) durations as fallback.
        let mut pkg_duration: Option<f64> = None;
        let mut toplevel_duration: f64 = 0.0;

        for line in input.lines() {
            // === RUN   TestName — start accumulating for a new test.
            if let Some(rest) = line.strip_prefix("=== RUN") {
                let name = rest.trim().to_string();
                current_name = if name.is_empty() { None } else { Some(name) };
                current_lines.clear();
                continue;
            }

            // --- FAIL: TestName (0.00s) — emit diagnostic from accumulated lines.
            if let Some(rest) = line.strip_prefix("--- FAIL: ") {
                let name = rest.split_whitespace().next().unwrap_or(rest).to_string();
                // Only count top-level tests for fallback duration (no '/' means not a subtest).
                if !name.contains('/') {
                    if let Some(secs) = parse_test_duration(rest) {
                        toplevel_duration += secs;
                    }
                }
                // Use lines accumulated since `=== RUN` (the detail comes BEFORE this line).
                let lines = std::mem::take(&mut current_lines);
                diagnostics.push(build_text_diagnostic(name, &lines));
                current_name = None;
                continue;
            }

            // --- PASS: TestName (0.00s) — discard accumulated lines, count pass.
            if let Some(rest) = line.strip_prefix("--- PASS: ") {
                let name = rest.split_whitespace().next().unwrap_or(rest).to_string();
                // Only count top-level tests for fallback duration.
                if !name.contains('/') {
                    if let Some(secs) = parse_test_duration(rest) {
                        toplevel_duration += secs;
                    }
                }
                current_lines.clear();
                current_name = None;
                passed += 1;
                continue;
            }

            // --- SKIP: TestName (0.00s) — discard accumulated lines, count skip.
            if line.starts_with("--- SKIP: ") {
                current_lines.clear();
                current_name = None;
                skipped += 1;
                continue;
            }

            // ok  \tpackage\t0.65s  or  FAIL\tpackage\t0.65s — package-level summary.
            // This is the authoritative total duration; prefer it over per-test accumulation.
            if line.starts_with("ok  \t") || line.starts_with("FAIL\t") {
                if let Some(secs) = parse_package_duration(line) {
                    pkg_duration = Some(secs);
                }
                current_lines.clear();
                current_name = None;
                continue;
            }

            // Any other line while a test is running: accumulate as detail.
            if current_name.is_some() {
                current_lines.push(line.to_string());
            }
        }

        // Failure count comes directly from the collected diagnostics.
        let failed = diagnostics.len() as u32;

        // Prefer the package-level duration; fall back to sum of top-level tests.
        let duration_secs = pkg_duration.or(if toplevel_duration > 0.0 {
            Some(toplevel_duration)
        } else {
            None
        });

        let summary = build_test_summary(passed, failed, skipped);
        let counts = Counts {
            passed,
            failed,
            skipped,
            ..Counts::default()
        };

        ParsedOutput {
            tool: "go-test",
            summary,
            diagnostics,
            counts,
            duration_secs,
            raw_lines,
            raw_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the duration from a `--- PASS:` / `--- FAIL:` rest string like `TestFoo (0.50s)`.
///
/// Returns seconds as `f64`, or `None` if no valid duration is found.
fn parse_test_duration(rest: &str) -> Option<f64> {
    // rest looks like: `TestFoo (0.50s)` or `TestFoo/sub (0.20s)`
    let open = rest.rfind('(')?;
    let close = rest[open..].find(')')? + open;
    let inner = rest[open + 1..close].trim();
    // Strip trailing 's'
    let num_str = inner.strip_suffix('s')?;
    num_str.parse::<f64>().ok()
}

/// Parse the duration from a package-level `ok` or `FAIL` line.
///
/// `ok  \tmypackage\t0.65s` → `Some(0.65)`
/// `FAIL\tmypackage\t0.65s` → `Some(0.65)`
fn parse_package_duration(line: &str) -> Option<f64> {
    // The duration is the last whitespace-separated token ending in 's'.
    let last = line.split_whitespace().last()?;
    let num_str = last.strip_suffix('s')?;
    num_str.parse::<f64>().ok()
}

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
        .map(str::trim)
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
            .map_or(0, |p| p + 1);
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
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_text_fingerprint() {
        let sample = "=== RUN   TestFoo\n    foo_test.go:10: expected 1 got 2\n--- FAIL: TestFoo (0.00s)\nFAIL\n";
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "Building foo...\nDone.\nAll steps completed.\n";
        assert!(!PARSER.detect(sample).matched());
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
        let out = PARSER.parse(input, DetectResult::NdJson);
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
        let out = PARSER.parse(input, DetectResult::NdJson);
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

    // Bug 2 regression: detail lines come BEFORE `--- FAIL:` in real Go output.
    // The old parser accumulated lines AFTER `--- FAIL:`, so it always got empty detail.
    #[test]
    fn parse_text_failure_detail_before_fail_line() {
        let input = concat!(
            "=== RUN   TestAdd\n",
            "    math_test.go:15: expected 4, got 3\n",
            "--- FAIL: TestAdd (0.00s)\n",
            "FAIL\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "TestAdd");
        assert_eq!(diag.severity, Severity::Error);
        // Location must be extracted from the detail line (before --- FAIL).
        let loc = diag.location.as_ref().expect("location should be present");
        assert_eq!(loc.file, "math_test.go");
        assert_eq!(loc.line, 15);
        assert!(
            diag.message.contains("expected 4, got 3"),
            "message should come from the detail line"
        );
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
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.passed, 2);
        assert_eq!(out.counts.failed, 0);
        assert!(out.diagnostics.is_empty());
        assert!(out.summary.contains("2 passed"));
    }

    // -- Bug #8: duration double-counting -----------------------------------------

    /// Package-level `ok` line must be used as the authoritative duration.
    /// Subtest durations must NOT be summed in (that would double-count).
    #[test]
    fn parse_text_duration_uses_ok_line() {
        // TestFoo (0.50s) includes subtest time; ok line gives 0.65s (total).
        // Without the fix, summing all --- PASS lines gives 0.50+0.20+0.30+0.10 = 1.10s.
        let input = concat!(
            "=== RUN   TestFoo\n",
            "=== RUN   TestFoo/subtest1\n",
            "--- PASS: TestFoo/subtest1 (0.20s)\n",
            "=== RUN   TestFoo/subtest2\n",
            "--- PASS: TestFoo/subtest2 (0.30s)\n",
            "--- PASS: TestFoo (0.50s)\n",
            "=== RUN   TestBar\n",
            "--- PASS: TestBar (0.10s)\n",
            "ok  \tmypackage\t0.65s\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        // Should use the ok line's 0.65s, not the double-counted sum.
        assert_eq!(out.duration_secs, Some(0.65));
    }

    /// When no `ok`/`FAIL` package line is present, only top-level (non-subtest)
    /// durations should be summed — subtests must be excluded.
    #[test]
    fn parse_text_duration_toplevel_only_fallback() {
        let input = concat!(
            "=== RUN   TestFoo\n",
            "=== RUN   TestFoo/sub\n",
            "--- PASS: TestFoo/sub (0.30s)\n",
            "--- PASS: TestFoo (0.50s)\n",
            "=== RUN   TestBar\n",
            "--- PASS: TestBar (0.10s)\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        // 0.50 + 0.10 = 0.60 (TestFoo/sub must not be included).
        let dur = out.duration_secs.expect("duration should be present");
        assert!((dur - 0.60).abs() < 1e-9, "expected 0.60, got {dur}");
    }

    /// JSON parse: subtest elapsed times must not be added to the total.
    #[test]
    fn parse_json_duration_no_subtest_double_count() {
        let input = concat!(
            // Top-level test: elapsed 0.50s
            "{\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestFoo\"}\n",
            // Subtest 1: elapsed 0.20s (already included in TestFoo's 0.50s)
            "{\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestFoo/sub1\"}\n",
            "{\"Action\":\"pass\",\"Package\":\"ex/pkg\",\"Test\":\"TestFoo/sub1\",\"Elapsed\":0.20}\n",
            // Subtest 2: elapsed 0.30s
            "{\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestFoo/sub2\"}\n",
            "{\"Action\":\"pass\",\"Package\":\"ex/pkg\",\"Test\":\"TestFoo/sub2\",\"Elapsed\":0.30}\n",
            // Parent test
            "{\"Action\":\"pass\",\"Package\":\"ex/pkg\",\"Test\":\"TestFoo\",\"Elapsed\":0.50}\n",
            // Another top-level test: elapsed 0.10s
            "{\"Action\":\"run\",\"Package\":\"ex/pkg\",\"Test\":\"TestBar\"}\n",
            "{\"Action\":\"pass\",\"Package\":\"ex/pkg\",\"Test\":\"TestBar\",\"Elapsed\":0.10}\n",
        );
        let out = PARSER.parse(input, DetectResult::NdJson);
        // Should be 0.50 + 0.10 = 0.60, not 0.50 + 0.20 + 0.30 + 0.10 = 1.10.
        let dur = out.duration_secs.expect("duration should be present");
        assert!((dur - 0.60).abs() < 1e-9, "expected 0.60, got {dur}");
    }
}
