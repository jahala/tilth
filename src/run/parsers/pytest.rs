use memchr::memmem;

use crate::run::types::{Counts, Diagnostic, Location, ParsedOutput, Severity, extract_count, truncate_detail};

use super::Parser;

pub static PARSER: PytestParser = PytestParser;

pub struct PytestParser;

impl Parser for PytestParser {
    fn name(&self) -> &'static str {
        "pytest"
    }

    /// Detect pytest output via byte scanning — no regex.
    ///
    /// Accepts the short test summary section header, or the pytest summary
    /// banner line (`=====` with `passed`/`failed`).
    fn detect(&self, sample: &str) -> bool {
        let bytes = sample.as_bytes();

        // Unambiguous: pytest's short test summary info section header.
        let short_summary = memmem::Finder::new("short test summary info");
        if short_summary.find(bytes).is_some() {
            return true;
        }

        // Banner line fingerprint: `=====` AND (`passed` OR `failed`).
        let equals = memmem::Finder::new("=====");
        if equals.find(bytes).is_some() {
            let passed = memmem::Finder::new("passed");
            let failed = memmem::Finder::new("failed");
            return passed.find(bytes).is_some() || failed.find(bytes).is_some();
        }

        false
    }

    /// Append `--tb=short -q` when no `--tb=` flag is already present.
    fn rewrite(&self, command: &str) -> Option<String> {
        if command.contains("--tb=") {
            return None;
        }
        Some(format!("{command} --tb=short -q"))
    }

    fn parse(&self, input: &str) -> ParsedOutput {
        // JSON is only available with the pytest-json-report plugin — very rare.
        // Attempt it opportunistically; fall through to text parsing otherwise.
        let trimmed = input.trim_start();
        if trimmed.starts_with('{') {
            if let Some(parsed) = self.try_json(input) {
                return parsed;
            }
        }
        self.parse_text(input)
    }
}

impl PytestParser {
    /// Opportunistic JSON parse for pytest-json-report output.
    fn try_json(&self, input: &str) -> Option<ParsedOutput> {
        let value: serde_json::Value = serde_json::from_str(input).ok()?;
        let summary = value.get("summary")?;

        let passed = summary
            .get("passed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let failed = summary
            .get("failed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let skipped = summary
            .get("skipped")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let errors = summary
            .get("error")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let duration_secs = value
            .get("duration")
            .and_then(|v| v.as_f64());

        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        if let Some(tests) = value.get("tests").and_then(|v| v.as_array()) {
            for test in tests {
                let outcome = test
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if outcome != "failed" && outcome != "error" {
                    continue;
                }
                let name = test
                    .get("nodeid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>")
                    .to_string();
                let call = test.get("call");
                let message = call
                    .and_then(|c| c.get("longrepr"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.lines().next())
                    .unwrap_or("test failed")
                    .to_string();
                let location = test
                    .get("nodeid")
                    .and_then(|v| v.as_str())
                    .and_then(parse_nodeid_location);
                let raw_detail = call
                    .and_then(|c| c.get("longrepr"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let detail = truncate_detail(raw_detail);
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    location,
                    name,
                    message,
                    detail,
                });
            }
        }

        let summary_str = build_summary(passed, failed, skipped, errors);
        let counts = Counts {
            passed,
            failed,
            skipped,
            errors,
            ..Counts::default()
        };

        Some(ParsedOutput {
            tool: "pytest",
            summary: summary_str,
            diagnostics,
            counts,
            duration_secs,
            raw_lines: input.lines().count(),
            raw_bytes: input.len(),
        })
    }

    fn parse_text(&self, input: &str) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();
        let lines: Vec<&str> = input.lines().collect();

        let mut passed: u32 = 0;
        let mut failed: u32 = 0;
        let mut skipped: u32 = 0;
        let mut duration_secs: Option<f64> = None;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Single pass: classify each line by state.
        let mut in_short_summary = false;
        let mut failure_block_name: Option<String> = None;
        let mut failure_block_lines: Vec<&str> = Vec::new();

        let finish_block =
            |name: &str, block_lines: &[&str], diagnostics: &mut Vec<Diagnostic>| {
                let block = block_lines.join("\n");
                let message = extract_error_message(&block);
                let location = Location::scan_text(&block);
                let detail = truncate_detail(&block);
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    location,
                    name: name.to_string(),
                    message,
                    detail,
                });
            };

        for line in &lines {
            // Detect `___ test_name ___` failure block headers.
            if let Some(block_name) = parse_pytest_block_header(line) {
                // Finish any open block first.
                if let Some(ref name) = failure_block_name {
                    finish_block(name, &failure_block_lines, &mut diagnostics);
                }
                failure_block_name = Some(block_name);
                failure_block_lines = Vec::new();
                in_short_summary = false;
                continue;
            }

            // Detect the short test summary section header.
            // e.g. `========= short test summary info =========`
            if line.contains("short test summary info") {
                // Close any open block.
                if let Some(ref name) = failure_block_name {
                    finish_block(name, &failure_block_lines, &mut diagnostics);
                    failure_block_name = None;
                    failure_block_lines = Vec::new();
                }
                in_short_summary = true;
                continue;
            }

            // A banner line (`====...====`) that isn't the summary section header
            // ends the short summary section (or ends a block).
            if is_banner_line(line) {
                if let Some(ref name) = failure_block_name {
                    finish_block(name, &failure_block_lines, &mut diagnostics);
                    failure_block_name = None;
                    failure_block_lines = Vec::new();
                }
                in_short_summary = false;

                // Try to parse the overall summary from this banner line.
                // e.g. `======= 2 failed, 5 passed, 1 skipped in 0.42s =======`
                if let Some(result) = parse_summary_banner(line) {
                    passed = result.0;
                    failed = result.1;
                    skipped = result.2;
                    duration_secs = result.3;
                }
                continue;
            }

            // Accumulate failure block content.
            if failure_block_name.is_some() {
                failure_block_lines.push(line);
                continue;
            }

            // Parse short test summary lines.
            // e.g. `FAILED tests/test_foo.py::test_bar - AssertionError: x != y`
            // e.g. `ERROR tests/test_foo.py::test_bar`
            if in_short_summary {
                if let Some(diag) = parse_summary_line(line) {
                    // Only add if we haven't already captured this test via a block.
                    // Prefer block-based diagnostics (richer detail); the summary
                    // line is used as fallback if there was no block.
                    //
                    // Block names come from `___ test_name ___` headers (just the
                    // function name), while summary names are full node ids like
                    // `tests/foo.py::test_name`.  Match on the trailing component.
                    let fn_name = diag
                        .name
                        .rsplit("::")
                        .next()
                        .unwrap_or(&diag.name);
                    let already_captured = diagnostics
                        .iter()
                        .any(|d| d.name == diag.name || d.name == fn_name || diag.name.ends_with(&format!("::{}", d.name)));
                    if !already_captured {
                        diagnostics.push(diag);
                    }
                }
            }
        }

        // Close any trailing open block.
        if let Some(ref name) = failure_block_name {
            finish_block(name, &failure_block_lines, &mut diagnostics);
        }

        let summary_str = build_summary(passed, failed, skipped, 0);
        let counts = Counts {
            passed,
            failed,
            skipped,
            errors: diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count() as u32,
            ..Counts::default()
        };

        ParsedOutput {
            tool: "pytest",
            summary: summary_str,
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

/// Returns true if a line is a pytest banner (`=====...=====`).
fn is_banner_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("====") && trimmed.ends_with("====") && trimmed.len() >= 8
}

/// Parse a pytest failure/section block header of the form `___ name ___`.
///
/// Returns the name between the underscores, or `None` if the line doesn't
/// match.
fn parse_pytest_block_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("___") || !trimmed.ends_with("___") || trimmed.len() < 7 {
        return None;
    }
    let inner = trimmed
        .trim_start_matches('_')
        .trim_end_matches('_')
        .trim();
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// Parse a short test summary line.
///
/// Handled formats:
/// - `FAILED tests/foo.py::test_bar - AssertionError: something`
/// - `ERROR tests/foo.py::test_bar`
fn parse_summary_line(line: &str) -> Option<Diagnostic> {
    let trimmed = line.trim();

    let (severity, rest) = if let Some(r) = trimmed.strip_prefix("FAILED ") {
        (Severity::Error, r)
    } else if let Some(r) = trimmed.strip_prefix("ERROR ") {
        (Severity::Error, r)
    } else {
        return None;
    };

    // Split on ` - ` to separate node id from message.
    let (nodeid, message) = if let Some(dash_pos) = rest.find(" - ") {
        (&rest[..dash_pos], rest[dash_pos + 3..].trim())
    } else {
        (rest, "test failed")
    };

    let location = parse_nodeid_location(nodeid);

    Some(Diagnostic {
        severity,
        location,
        name: nodeid.trim().to_string(),
        message: message.to_string(),
        detail: None,
    })
}

/// Parse the pytest final summary banner, returning (passed, failed, skipped, duration).
///
/// Example: `====== 2 failed, 5 passed, 1 skipped in 0.42s ======`
fn parse_summary_banner(line: &str) -> Option<(u32, u32, u32, Option<f64>)> {
    // Only process lines that look like summary banners — they must contain
    // at least one of these result keywords.
    if !line.contains("passed") && !line.contains("failed") && !line.contains("error") {
        return None;
    }

    let passed = extract_count(line, "passed").unwrap_or(0);
    let failed = extract_count(line, "failed").unwrap_or(0);
    let skipped = extract_count(line, "skipped")
        .or_else(|| extract_count(line, "deselected"))
        .unwrap_or(0);
    let duration = extract_duration(line);

    Some((passed, failed, skipped, duration))
}

/// Extract the duration value from a string like `in 0.42s`.
fn extract_duration(s: &str) -> Option<f64> {
    let in_pos = s.find(" in ")?;
    let after = s[in_pos + 4..].trim();
    // Collect digits and the decimal point, stop at non-numeric characters.
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse().ok()
}

/// Extract the most useful error message from a failure block.
///
/// Priority:
/// 1. Lines starting with `E ` (pytest error detail lines)
/// 2. Lines matching `path:N: ErrorType`
/// 3. First non-empty, non-`>` line as fallback
fn extract_error_message(block: &str) -> String {
    // Collect all `E ` lines — they carry the assertion/error detail.
    let e_lines: Vec<&str> = block
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("E ") || t.starts_with("E\t")
        })
        .map(|l| l.trim_start().trim_start_matches('E').trim())
        .collect();

    if !e_lines.is_empty() {
        return e_lines.join(" ").trim().to_string();
    }

    // Look for a `path:N: ErrorType` line.
    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.contains(':') && !trimmed.starts_with('>') {
            // Heuristic: contains a colon after a `.py` path segment.
            if trimmed.contains(".py:") {
                return trimmed.to_string();
            }
        }
    }

    // Fallback: first non-empty, non-separator line.
    block
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('>') && !l.starts_with('-'))
        .unwrap_or("test failed")
        .to_string()
}

/// Parse a pytest node id like `tests/test_foo.py::test_bar` into a Location.
fn parse_nodeid_location(nodeid: &str) -> Option<Location> {
    // Strip the `::test_name` suffix, leaving just the file path.
    let file = if let Some(pos) = nodeid.find("::") {
        &nodeid[..pos]
    } else {
        nodeid
    };
    if file.is_empty() || !file.ends_with(".py") {
        return None;
    }
    Some(Location {
        file: file.to_string(),
        line: 0,
        column: None,
    })
}

/// Build a human-readable summary, omitting zero categories except `passed`.
fn build_summary(passed: u32, failed: u32, skipped: u32, errors: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{passed} passed"));
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    if errors > 0 {
        parts.push(format!("{errors} errors"));
    }
    parts.join(", ")
}



// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Detection -----------------------------------------------------------

    #[test]
    fn detect_summary_with_equals() {
        let sample = "collected 3 items\n\n============================== 2 passed, 1 failed in 0.12s ==============================";
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_short_test_summary() {
        let sample = "=========================== short test summary info ============================\nFAILED tests/test_foo.py::test_bar - AssertionError: 1 != 2";
        assert!(PARSER.detect(sample));
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "Building project...\nCompiling foo\nDone in 1.2s";
        assert!(!PARSER.detect(sample));
    }

    // -- Rewrite -------------------------------------------------------------

    #[test]
    fn rewrite_appends_tb_short() {
        let result = PARSER.rewrite("pytest tests/");
        assert_eq!(result, Some("pytest tests/ --tb=short -q".to_string()));
    }

    #[test]
    fn rewrite_skips_if_present() {
        let result = PARSER.rewrite("pytest tests/ --tb=long");
        assert!(result.is_none());
    }

    // -- Text parse ----------------------------------------------------------

    #[test]
    fn parse_text_all_pass() {
        let input = concat!(
            "collected 5 items\n",
            "\n",
            "tests/test_foo.py .....                                                  [100%]\n",
            "\n",
            "============================== 5 passed in 0.23s ==============================\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.tool, "pytest");
        assert_eq!(out.counts.passed, 5);
        assert_eq!(out.counts.failed, 0);
        assert_eq!(out.counts.skipped, 0);
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.duration_secs, Some(0.23));
        assert!(out.summary.contains("5 passed"));
        assert!(!out.summary.contains("failed"));
    }

    #[test]
    fn parse_text_with_failures() {
        let input = concat!(
            "collected 3 items\n",
            "\n",
            "=========================== short test summary info ============================\n",
            "FAILED tests/test_foo.py::test_add - AssertionError: 1 != 2\n",
            "FAILED tests/test_foo.py::test_mul - AssertionError: 3 != 4\n",
            "========================= 2 failed, 1 passed in 0.05s =========================\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.counts.passed, 1);
        assert_eq!(out.counts.failed, 2);
        assert_eq!(out.diagnostics.len(), 2);
        assert_eq!(out.diagnostics[0].name, "tests/test_foo.py::test_add");
        assert_eq!(out.diagnostics[0].message, "AssertionError: 1 != 2");
        assert_eq!(out.diagnostics[0].severity, Severity::Error);
        assert!(out.summary.contains("2 failed"));
        assert!(out.summary.contains("1 passed"));
    }

    #[test]
    fn parse_text_failure_detail() {
        let input = concat!(
            "collected 1 item\n",
            "\n",
            "________________________________ test_add _________________________________\n",
            "\n",
            "    def test_add():\n",
            ">       assert add(1, 2) == 4\n",
            "E       AssertionError: assert 3 == 4\n",
            "\n",
            "tests/test_foo.py:10: AssertionError\n",
            "=========================== short test summary info ============================\n",
            "FAILED tests/test_foo.py::test_add - AssertionError: assert 3 == 4\n",
            "============================== 1 failed in 0.07s ==============================\n",
        );
        let out = PARSER.parse(input);
        assert_eq!(out.counts.failed, 1);
        // The block-based diagnostic is preferred over the summary line.
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "test_add");
        assert_eq!(diag.severity, Severity::Error);
        assert!(
            diag.detail.is_some(),
            "detail should be populated from block"
        );
        let detail = diag.detail.as_ref().unwrap();
        assert!(detail.contains("assert add(1, 2) == 4"));
        let loc = diag.location.as_ref().expect("location should be extracted");
        assert_eq!(loc.file, "tests/test_foo.py");
        assert_eq!(loc.line, 10);
    }
}
