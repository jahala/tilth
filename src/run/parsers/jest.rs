use memchr::memmem;
use serde_json::Value;

use crate::run::ansi;
use crate::run::types::{Counts, Diagnostic, Location, ParsedOutput, Severity, extract_count, truncate_detail};

use super::Parser;

pub static PARSER: JestParser = JestParser;

pub struct JestParser;

impl Parser for JestParser {
    fn name(&self) -> &'static str {
        "jest"
    }

    /// Detect Jest/Vitest output via byte scanning — no regex.
    ///
    /// Accepts JSON format (`"numPassedTests"` or `"numFailedTests"`) or text
    /// format (`Tests:` + `passed`/`failed`, or Vitest markers `✓`/`×` + `Tests`).
    fn detect(&self, sample: &str) -> bool {
        let bytes = sample.as_bytes();

        // JSON fingerprint: Jest and Vitest both emit these keys.
        let num_passed = memmem::Finder::new(r#""numPassedTests""#);
        let num_failed = memmem::Finder::new(r#""numFailedTests""#);
        if num_passed.find(bytes).is_some() || num_failed.find(bytes).is_some() {
            return true;
        }

        // Text fingerprint: summary line must contain "Tests:" AND a status word.
        let tests_label = memmem::Finder::new("Tests:");
        if tests_label.find(bytes).is_none() {
            return false;
        }

        let passed_finder = memmem::Finder::new("passed");
        let failed_finder = memmem::Finder::new("failed");
        // Vitest markers (UTF-8 encoded)
        let vitest_pass = memmem::Finder::new("✓");
        let vitest_fail = memmem::Finder::new("×");

        passed_finder.find(bytes).is_some()
            || failed_finder.find(bytes).is_some()
            || vitest_pass.find(bytes).is_some()
            || vitest_fail.find(bytes).is_some()
    }

    /// Append `--json` to the command unless it is already present.
    ///
    /// Handles bare `jest`, `vitest`, and package-manager–prefixed invocations
    /// (`npx jest`, `yarn jest`, `pnpm jest`, etc.).
    fn rewrite(&self, command: &str) -> Option<String> {
        let json_flag = memmem::Finder::new("--json");
        if json_flag.find(command.as_bytes()).is_some() {
            return None;
        }
        Some(format!("{command} --json"))
    }

    fn parse(&self, input: &str) -> ParsedOutput {
        let trimmed = input.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Some(parsed) = self.try_json(input) {
                return parsed;
            }
        }
        self.parse_text(input)
    }
}

impl JestParser {
    fn try_json(&self, input: &str) -> Option<ParsedOutput> {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        // Jest JSON output is a single object, not NDJSON.
        let root: Value = serde_json::from_str(input.trim()).ok()?;

        let passed = root
            .get("numPassedTests")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let failed = root
            .get("numFailedTests")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let pending = root
            .get("numPendingTests")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut duration_secs: Option<f64> = None;

        if let Some(test_results) = root.get("testResults").and_then(|v| v.as_array()) {
            for suite in test_results {
                // Duration from perfStats or endTime - startTime.
                if duration_secs.is_none() {
                    if let Some(runtime) = suite
                        .get("perfStats")
                        .and_then(|ps| ps.get("runtime"))
                        .and_then(|v| v.as_f64())
                    {
                        duration_secs = Some(runtime / 1000.0);
                    } else {
                        let start = suite
                            .get("perfStats")
                            .and_then(|ps| ps.get("start"))
                            .or_else(|| suite.get("startTime"))
                            .and_then(|v| v.as_f64());
                        let end = suite
                            .get("perfStats")
                            .and_then(|ps| ps.get("end"))
                            .or_else(|| suite.get("endTime"))
                            .and_then(|v| v.as_f64());
                        if let (Some(s), Some(e)) = (start, end) {
                            duration_secs = Some((e - s) / 1000.0);
                        }
                    }
                }

                let assertions = suite
                    .get("assertionResults")
                    .and_then(|v| v.as_array());
                let Some(assertions) = assertions else {
                    continue;
                };

                for assertion in assertions {
                    let status = assertion
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if status != "failed" {
                        continue;
                    }

                    let name = build_test_name(assertion);

                    let failure_messages: Vec<&str> = assertion
                        .get("failureMessages")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str())
                                .collect()
                        })
                        .unwrap_or_default();

                    let first_message = failure_messages.first().copied().unwrap_or("");
                    let message = extract_jest_message(first_message);
                    let location = extract_location_from_failure(first_message);

                    let detail = if first_message.is_empty() {
                        None
                    } else {
                        truncate_detail(&ansi::strip(first_message))
                    };

                    diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        location,
                        name,
                        message,
                        detail,
                    });
                }
            }
        }

        let summary = build_summary(passed, failed, pending);
        let counts = Counts {
            passed,
            failed,
            errors: failed,
            skipped: pending,
            ..Counts::default()
        };

        Some(ParsedOutput {
            tool: "jest",
            summary,
            diagnostics,
            counts,
            duration_secs,
            raw_lines,
            raw_bytes,
        })
    }

    fn parse_text(&self, input: &str) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut passed: u32 = 0;
        let mut failed: u32 = 0;
        let mut pending: u32 = 0;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Summary line: `Tests:  3 passed, 1 failed, 4 total`
        for line in input.lines() {
            if let Some(rest) = line.find("Tests:").map(|i| &line[i + 6..]) {
                let rest = rest.trim();
                passed = extract_count(rest, "passed").unwrap_or(0);
                failed = extract_count(rest, "failed").unwrap_or(0);
                pending = extract_count(rest, "skipped")
                    .or_else(|| extract_count(rest, "pending"))
                    .or_else(|| extract_count(rest, "todo"))
                    .unwrap_or(0);
                break;
            }
        }

        // Failure blocks: `● describe > test name` followed by message lines until the
        // next `●` or end of output.
        let lines: Vec<&str> = input.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if let Some(name) = parse_bullet_header(line) {
                let block_start = i + 1;
                let mut block_end = block_start;
                while block_end < lines.len() {
                    let next = lines[block_end];
                    // Next failure block or section separator ends this block.
                    if parse_bullet_header(next).is_some() {
                        break;
                    }
                    // Blank line followed by a non-indented line typically ends a block.
                    if next.trim().is_empty()
                        && block_end + 1 < lines.len()
                        && !lines[block_end + 1].starts_with(' ')
                        && !lines[block_end + 1].starts_with('\t')
                        && !lines[block_end + 1].trim().is_empty()
                    {
                        break;
                    }
                    block_end += 1;
                }

                let block = lines[block_start..block_end].join("\n");
                let message = extract_jest_message(&block);
                let location = extract_location_from_failure(&block);
                let detail = truncate_detail(&block);
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    location,
                    name,
                    message,
                    detail,
                });
                i = block_end;
                continue;
            }
            i += 1;
        }

        let summary = build_summary(passed, failed, pending);
        let counts = Counts {
            passed,
            failed,
            errors: failed,
            skipped: pending,
            ..Counts::default()
        };

        ParsedOutput {
            tool: "jest",
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

/// Build a human-readable summary, omitting zero categories except `passed`.
fn build_summary(passed: u32, failed: u32, pending: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{passed} passed"));
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if pending > 0 {
        parts.push(format!("{pending} skipped"));
    }
    parts.join(", ")
}

/// Build a test name from a Jest assertion result object.
///
/// Prefers `fullName`; falls back to `ancestorTitles` joined with ` > ` + `title`.
fn build_test_name(assertion: &Value) -> String {
    if let Some(full) = assertion.get("fullName").and_then(|v| v.as_str()) {
        if !full.is_empty() {
            return full.to_string();
        }
    }

    let ancestors: Vec<&str> = assertion
        .get("ancestorTitles")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let title = assertion
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");

    if ancestors.is_empty() {
        title.to_string()
    } else {
        format!("{} > {title}", ancestors.join(" > "))
    }
}

/// Extract the most useful human-readable message from a Jest failure string.
///
/// Priority:
/// 1. `expect(...)` lines — the matcher description
/// 2. `Error:` lines
/// 3. First non-empty, non-ANSI, non-`at ` line
fn extract_jest_message(text: &str) -> String {
    let clean = ansi::strip(text);

    // Look for expect() description lines or Error: lines.
    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("expect(") || trimmed.starts_with("Expected") {
            return trimmed.to_string();
        }
        if trimmed.starts_with("Error:") || trimmed.starts_with("AssertionError:") {
            return trimmed.to_string();
        }
    }

    // Fallback: first non-empty line that isn't a stack frame.
    clean
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("at ") && !l.starts_with("●"))
        .unwrap_or("test failed")
        .to_string()
}

/// Scan a failure message for a `file:line:col` stack frame pattern.
///
/// Jest stack frames look like: `at Object.<anonymous> (src/foo.test.js:42:5)`.
fn extract_location_from_failure(text: &str) -> Option<Location> {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("at ") {
            continue;
        }
        // Look for a `(path:line:col)` or `(path:line)` group.
        if let Some(loc) = extract_paren_location(trimmed) {
            return Some(loc);
        }
    }
    None
}

/// Extract a `Location` from a parenthesised path, e.g. `(src/foo.test.js:42:5)`.
fn extract_paren_location(line: &str) -> Option<Location> {
    let open = line.rfind('(')?;
    let close = line[open..].find(')')? + open;
    let inner = &line[open + 1..close];
    parse_path_with_coords(inner)
}

/// Parse `path:line` or `path:line:col` from a plain string.
fn parse_path_with_coords(s: &str) -> Option<Location> {
    // Walk backwards from end, collecting col then line numbers.
    let bytes = s.as_bytes();
    let mut end = bytes.len();

    // Optional column.
    let col = if end > 0 {
        let col_end = end;
        let mut col_start = col_end;
        while col_start > 0 && bytes[col_start - 1].is_ascii_digit() {
            col_start -= 1;
        }
        if col_start < col_end && col_start > 0 && bytes[col_start - 1] == b':' {
            let val: u32 = std::str::from_utf8(&bytes[col_start..col_end])
                .ok()?
                .parse()
                .ok()?;
            end = col_start - 1; // strip `:col`
            Some(val)
        } else {
            None
        }
    } else {
        None
    };

    // Line number (required).
    let line_end = end;
    let mut line_start = line_end;
    while line_start > 0 && bytes[line_start - 1].is_ascii_digit() {
        line_start -= 1;
    }
    if line_start == line_end || line_start == 0 || bytes[line_start - 1] != b':' {
        return None;
    }
    let line_num: u32 = std::str::from_utf8(&bytes[line_start..line_end])
        .ok()?
        .parse()
        .ok()?;
    let path_end = line_start - 1; // strip `:line`

    if path_end == 0 {
        return None;
    }

    let file = std::str::from_utf8(&bytes[..path_end]).ok()?;

    // Must look like a path (contains `.` or `/`) and not be empty.
    let looks_like_path = file.contains('.') || file.contains('/');
    if !looks_like_path || file.is_empty() {
        return None;
    }

    Some(Location {
        file: file.to_string(),
        line: line_num,
        column: col,
    })
}

/// Parse the leading `●` failure block header: `  ● describe > test name`.
///
/// Returns the test name (the text after `●`), or `None` if the line is not a header.
fn parse_bullet_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('●')?;
    let name = rest.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
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
        let sample = r#"{"numPassedTests":3,"numFailedTests":0,"testResults":[]}"#;
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_text_fingerprint() {
        let sample = "PASS src/add.test.js\nTests:  3 passed, 3 total\nTime: 0.5s";
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "Building project...\nCompiling foo v0.1.0\nFinished in 1.2s";
        assert!(!PARSER.detect(sample));
    }

    // -- Rewrite -------------------------------------------------------------

    #[test]
    fn rewrite_appends_json() {
        assert_eq!(
            PARSER.rewrite("npx jest"),
            Some("npx jest --json".to_string())
        );
        assert_eq!(
            PARSER.rewrite("yarn vitest run"),
            Some("yarn vitest run --json".to_string())
        );
    }

    #[test]
    fn rewrite_skips_if_present() {
        assert!(PARSER.rewrite("jest --json").is_none());
        assert!(PARSER.rewrite("npx jest --json --coverage").is_none());
    }

    // -- JSON parse ----------------------------------------------------------

    #[test]
    fn parse_json_all_pass() {
        let input = r#"{
            "numPassedTests": 3,
            "numFailedTests": 0,
            "numPendingTests": 0,
            "testResults": [{
                "perfStats": {"runtime": 450},
                "assertionResults": [
                    {"status": "passed", "fullName": "add returns 2", "failureMessages": []},
                    {"status": "passed", "fullName": "add returns 4", "failureMessages": []},
                    {"status": "passed", "fullName": "add returns 6", "failureMessages": []}
                ]
            }]
        }"#;
        let out = PARSER.parse(input);
        assert_eq!(out.tool, "jest");
        assert_eq!(out.counts.passed, 3);
        assert_eq!(out.counts.failed, 0);
        assert_eq!(out.counts.skipped, 0);
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.duration_secs, Some(0.45));
        assert!(out.summary.contains("3 passed"));
        assert!(!out.summary.contains("failed"));
    }

    #[test]
    fn parse_json_with_failure() {
        let input = r#"{
            "numPassedTests": 1,
            "numFailedTests": 1,
            "numPendingTests": 0,
            "testResults": [{
                "perfStats": {"runtime": 200},
                "assertionResults": [
                    {"status": "passed", "fullName": "add returns 2", "failureMessages": []},
                    {
                        "status": "failed",
                        "fullName": "add returns wrong value",
                        "title": "returns wrong value",
                        "ancestorTitles": ["add"],
                        "failureMessages": [
                            "Error: expect(received).toBe(expected)\n\nExpected: 5\nReceived: 4\n\n    at Object.<anonymous> (src/add.test.js:10:5)"
                        ]
                    }
                ]
            }]
        }"#;
        let out = PARSER.parse(input);
        assert_eq!(out.counts.failed, 1);
        assert_eq!(out.counts.passed, 1);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "add returns wrong value");
        assert_eq!(diag.severity, Severity::Error);
        assert!(!diag.message.is_empty());
        let loc = diag.location.as_ref().expect("location should be present");
        assert_eq!(loc.file, "src/add.test.js");
        assert_eq!(loc.line, 10);
        assert_eq!(loc.column, Some(5));
    }

    // -- Text parse ----------------------------------------------------------

    #[test]
    fn parse_text_summary() {
        let input = concat!(
            "PASS src/add.test.js\n",
            "FAIL src/sub.test.js\n",
            "\n",
            "Tests:  2 passed, 1 failed, 3 total\n",
            "Snapshots: 0 total\n",
            "Time: 1.2s\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.counts.passed, 2);
        assert_eq!(out.counts.failed, 1);
        assert_eq!(out.counts.skipped, 0);
        assert!(out.summary.contains("2 passed"));
        assert!(out.summary.contains("1 failed"));
    }

    #[test]
    fn parse_text_failure_block() {
        let input = concat!(
            "FAIL src/sub.test.js\n",
            "\n",
            "  ● subtract › returns correct value\n",
            "\n",
            "    expect(received).toBe(expected)\n",
            "\n",
            "    Expected: 1\n",
            "    Received: 2\n",
            "\n",
            "      at Object.<anonymous> (src/sub.test.js:8:5)\n",
            "\n",
            "Tests:  0 passed, 1 failed, 1 total\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "subtract › returns correct value");
        assert_eq!(diag.severity, Severity::Error);
        assert!(diag.detail.is_some());
        let detail = diag.detail.as_ref().unwrap();
        assert!(detail.contains("Expected"));
    }
}
