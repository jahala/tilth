use memchr::memmem;

use crate::run::types::{
    build_test_summary, extract_count, truncate_detail, Counts, DetectResult, Diagnostic, Location,
    ParsedOutput, Severity,
};

use super::Parser;

pub static PARSER: CargoTestParser = CargoTestParser;

pub struct CargoTestParser;

impl Parser for CargoTestParser {
    fn name(&self) -> &'static str {
        "cargo-test"
    }

    /// Detect cargo test output via byte scanning — no regex.
    ///
    /// Accepts JSON format (`"type":"test"`) or text format (`test result:` + `passed`).
    fn detect(&self, sample: &str) -> DetectResult {
        let bytes = sample.as_bytes();

        // JSON fingerprint: any line with `"type":"test"` or `"type": "test"`
        let type_test_compact = memmem::Finder::new(r#""type":"test""#);
        let type_test_spaced = memmem::Finder::new(r#""type": "test""#);
        if type_test_compact.find(bytes).is_some() || type_test_spaced.find(bytes).is_some() {
            return DetectResult::NdJson;
        }

        // Text fingerprint: `test result:` AND `passed` both appear in the sample
        let test_result = memmem::Finder::new("test result:");
        let passed = memmem::Finder::new("passed");
        if test_result.find(bytes).is_some() && passed.find(bytes).is_some() {
            return DetectResult::Text;
        }

        DetectResult::NoMatch
    }

    fn parse(&self, input: &str, hint: DetectResult) -> ParsedOutput {
        if hint == DetectResult::NdJson {
            if let Some(parsed) = CargoTestParser::try_json(input) {
                return parsed;
            }
        }
        CargoTestParser::parse_text(input)
    }
}

impl CargoTestParser {
    fn try_json(input: &str) -> Option<ParsedOutput> {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let events = parse_ndjson(input);
        if events.is_empty() {
            return None;
        }

        let mut passed: u32 = 0;
        let mut failed: u32 = 0;
        let mut ignored: u32 = 0;
        let mut duration_secs: Option<f64> = None;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        for event in &events {
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let event_kind = event.get("event").and_then(|v| v.as_str()).unwrap_or("");

            match (event_type, event_kind) {
                ("test", "ok") => {
                    passed += 1;
                }
                ("test", "failed") => {
                    failed += 1;
                    let name = event
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unknown>")
                        .to_string();
                    let stdout = event.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
                    let message = extract_failure_message(stdout);
                    let location = Location::scan_text(stdout);
                    let detail = truncate_detail(stdout);
                    diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        location,
                        name,
                        message,
                        detail,
                    });
                }
                ("test", "ignored") => {
                    ignored += 1;
                }
                ("suite", "ok" | "failed") => {
                    if let Some(exec_time) =
                        event.get("exec_time").and_then(serde_json::Value::as_f64)
                    {
                        duration_secs = Some(exec_time);
                    }
                }
                _ => {}
            }
        }

        let summary = build_test_summary(passed, failed, ignored);
        let counts = Counts {
            passed,
            failed,
            skipped: ignored,
            ..Counts::default()
        };

        Some(ParsedOutput {
            tool: "cargo-test",
            summary,
            diagnostics,
            counts,
            duration_secs,
            raw_lines,
            raw_bytes,
        })
    }

    fn parse_text(input: &str) -> ParsedOutput {
        let raw_bytes = input.len();
        let raw_lines = input.lines().count();

        let mut passed: u32 = 0;
        let mut failed: u32 = 0;
        let mut ignored: u32 = 0;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Parse summary line: `test result: ok. 5 passed; 0 failed; 1 ignored`
        // or `test result: FAILED. 2 passed; 3 failed; 0 ignored`
        for line in input.lines() {
            if let Some(rest) = line.find("test result:").map(|i| &line[i + 12..]) {
                let rest = rest.trim();
                // Strip leading "ok." or "FAILED." status word
                let rest = rest.find('.').map_or(rest, |i| rest[i + 1..].trim());

                passed = extract_count(rest, "passed").unwrap_or(0);
                failed = extract_count(rest, "failed").unwrap_or(0);
                ignored = extract_count(rest, "ignored").unwrap_or(0);
                // NOTE: Only the first `test result:` line is captured. Multi-binary workspace
                // runs produce multiple summary lines; the JSON path handles those correctly.
                break;
            }
        }

        // Extract failure blocks: `---- test_name stdout ----`
        let lines: Vec<&str> = input.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            // A failure block header looks like: `---- some::test_name stdout ----`
            if let Some(name) = parse_failure_block_header(line) {
                // Collect the block content until the next `----` separator or end
                let block_start = i + 1;
                let mut block_end = block_start;
                while block_end < lines.len() {
                    let next = lines[block_end].trim();
                    if next.starts_with("----") {
                        break;
                    }
                    block_end += 1;
                }
                let block = lines[block_start..block_end].join("\n");
                let message = extract_failure_message(&block);
                let location = Location::scan_text(&block);
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

        let summary = build_test_summary(passed, failed, ignored);
        let counts = Counts {
            passed,
            failed,
            skipped: ignored,
            ..Counts::default()
        };

        ParsedOutput {
            tool: "cargo-test",
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

fn parse_ndjson(input: &str) -> Vec<serde_json::Value> {
    input
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Extract the most useful failure message from a test's stdout.
///
/// Priority order:
/// 1. `assertion` lines (`assert_eq!`, `assert!`, `assert_ne!` output)
/// 2. `thread '...' panicked at` lines
/// 3. First non-empty line as fallback
fn extract_failure_message(stdout: &str) -> String {
    // Look for assertion failures first: lines containing "left" / "right" or
    // the assertion macro name. Cargo prints them as:
    //   assertion `left == right` failed
    //   left: 1
    //   right: 2
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("assertion") && trimmed.contains("failed") {
            return trimmed.to_string();
        }
    }

    // Panic message: `thread 'test_name' panicked at 'message', file:line`
    // or the newer format (Rust 1.73+): `thread 'test_name' panicked at file:line:`
    // followed by the message on the next line.
    let lines: Vec<&str> = stdout.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.contains("panicked at") {
            // Old format: panicked at 'message', src/...
            if let Some(msg) = extract_panic_message_old(trimmed) {
                return msg;
            }
            // New format (Rust 1.73+): panicked at src/file.rs:line:col:
            // The message is on the next line.
            if is_new_panic_format(trimmed) {
                if let Some(next_line) = lines.get(i + 1) {
                    let msg = next_line.trim();
                    if !msg.is_empty() {
                        return msg.to_string();
                    }
                }
            }
            return trimmed.to_string();
        }
    }

    // Fallback: first non-empty line
    stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("test failed")
        .to_string()
}

/// Extract the human message from a panic line using the old rustc format.
///
/// Old format: `thread 'name' panicked at 'the message', file:line:col`
fn extract_panic_message_old(line: &str) -> Option<String> {
    let after_at = line.find("panicked at '")?;
    let after_quote = after_at + "panicked at '".len();
    let close = line[after_quote..].find('\'')?;
    Some(line[after_quote..after_quote + close].to_string())
}

/// Returns true when the line matches the new Rust 1.73+ panic format:
/// `thread 'name' panicked at file:line:col:`
/// In this format the location appears inline and ends with `:`, and the
/// message is on the following line (not quoted on this line).
fn is_new_panic_format(line: &str) -> bool {
    // New format does NOT have `panicked at '` (that's the old format).
    // It ends with `:` after the location (e.g. `panicked at src/lib.rs:10:5:`).
    line.contains("panicked at")
        && !line.contains("panicked at '")
        && line.trim_end().ends_with(':')
}

/// Parse the header of a stdout failure block.
///
/// Matches lines of the form: `---- module::test_name stdout ----`
/// Returns the test name portion (everything between `---- ` and ` stdout ----`).
fn parse_failure_block_header(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("----") || !line.ends_with("----") {
        return None;
    }
    // Strip leading and trailing `----`
    let inner = line.trim_start_matches('-').trim_end_matches('-').trim();
    // Strip trailing ` stdout` or ` stderr`
    let name = if let Some(stripped) = inner.strip_suffix("stdout") {
        stripped.trim()
    } else if let Some(stripped) = inner.strip_suffix("stderr") {
        stripped.trim()
    } else {
        return None;
    };
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
        let sample = r#"{"type":"test","event":"ok","name":"foo::bar"}"#;
        assert!(PARSER.detect(sample).matched());
        assert_eq!(PARSER.detect(sample), DetectResult::NdJson);
    }

    #[test]
    fn detect_json_fingerprint_spaced() {
        let sample = r#"{"type": "test", "event": "ok", "name": "foo::bar"}"#;
        assert!(PARSER.detect(sample).matched());
    }

    #[test]
    fn detect_text_fingerprint() {
        let sample = "running 5 tests\ntest result: ok. 5 passed; 0 failed; 0 ignored";
        assert_eq!(PARSER.detect(sample), DetectResult::Text);
    }

    #[test]
    fn detect_rejects_generic() {
        let sample = "Building project...\nCompiling foo v0.1.0\nFinished in 1.2s";
        assert_eq!(PARSER.detect(sample), DetectResult::NoMatch);
    }

    // -- JSON parse ----------------------------------------------------------

    #[test]
    fn parse_json_all_pass() {
        let input = concat!(
            "{\"type\":\"suite\",\"event\":\"started\",\"test_count\":3}\n",
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"a::test_one\"}\n",
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"a::test_two\"}\n",
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"a::test_three\"}\n",
            "{\"type\":\"suite\",\"event\":\"ok\",\"passed\":3,\"failed\":0,\"ignored\":0,\"exec_time\":0.05}\n",
        );
        let out = PARSER.parse(input, DetectResult::NdJson);
        assert_eq!(out.tool, "cargo-test");
        assert_eq!(out.counts.passed, 3);
        assert_eq!(out.counts.failed, 0);
        assert_eq!(out.counts.skipped, 0);
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.duration_secs, Some(0.05));
        assert!(out.summary.contains("3 passed"));
        assert!(!out.summary.contains("failed"));
    }

    #[test]
    fn parse_json_with_failure() {
        let stdout =
            "thread 'a::test_bad' panicked at 'assertion failed: 1 == 2', src/lib.rs:10:5\n";
        let event = format!(
            "{{\"type\":\"test\",\"event\":\"failed\",\"name\":\"a::test_bad\",\"stdout\":{}}}\n",
            serde_json::to_string(stdout).unwrap()
        );
        let input = format!(
            "{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}}\n{event}{{\"type\":\"suite\",\"event\":\"failed\",\"passed\":0,\"failed\":1,\"ignored\":0,\"exec_time\":0.01}}\n",
        );
        let out = PARSER.parse(&input, DetectResult::NdJson);
        assert_eq!(out.counts.failed, 1);
        assert_eq!(out.counts.passed, 0);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "a::test_bad");
        assert_eq!(diag.severity, Severity::Error);
        // Message should contain the panic content
        assert!(!diag.message.is_empty());
        // Location should be extracted
        let loc = diag.location.as_ref().expect("location should be present");
        assert_eq!(loc.file, "src/lib.rs");
        assert_eq!(loc.line, 10);
        assert_eq!(loc.column, Some(5));
    }

    // -- Text parse ----------------------------------------------------------

    #[test]
    fn parse_text_summary() {
        let input = "running 3 tests\ntest foo ... ok\ntest bar ... ok\ntest baz ... FAILED\n\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out";
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.counts.passed, 2);
        assert_eq!(out.counts.failed, 1);
        assert_eq!(out.counts.skipped, 0);
        assert!(out.summary.contains("2 passed"));
        assert!(out.summary.contains("1 failed"));
    }

    // -- Panic format (Bug #7) -----------------------------------------------

    #[test]
    fn extract_panic_message_new_format() {
        // Rust 1.73+ format: location inline after "panicked at", message on next line.
        let stdout = "thread 'my_mod::test_add' panicked at src/lib.rs:10:5:\nexpected true, got false\nnote: run with RUST_BACKTRACE=1\n";
        let msg = extract_failure_message(stdout);
        assert_eq!(msg, "expected true, got false");
    }

    #[test]
    fn extract_panic_message_old_format_still_works() {
        // Old format: message quoted inline.
        let stdout =
            "thread 'my_mod::test_add' panicked at 'assertion failed: 1 == 2', src/lib.rs:10:5\n";
        let msg = extract_failure_message(stdout);
        assert_eq!(msg, "assertion failed: 1 == 2");
    }

    #[test]
    fn parse_json_failure_new_panic_format() {
        // Simulates a failed test whose stdout uses the Rust 1.73+ panic format.
        let stdout =
            "thread 'a::test_bad' panicked at src/lib.rs:10:5:\nassertion failed: 2 + 2 == 5\n";
        let event = format!(
            "{{\"type\":\"test\",\"event\":\"failed\",\"name\":\"a::test_bad\",\"stdout\":{}}}\n",
            serde_json::to_string(stdout).unwrap()
        );
        let input = format!(
            "{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}}\n{event}{{\"type\":\"suite\",\"event\":\"failed\",\"passed\":0,\"failed\":1,\"ignored\":0,\"exec_time\":0.01}}\n",
        );
        let out = PARSER.parse(&input, DetectResult::NdJson);
        assert_eq!(out.counts.failed, 1);
        let diag = &out.diagnostics[0];
        // Should extract the next-line message, not the location string.
        assert_eq!(diag.message, "assertion failed: 2 + 2 == 5");
    }

    #[test]
    fn parse_text_failure_block() {
        let input = concat!(
            "running 1 test\n",
            "test my_mod::test_add ... FAILED\n",
            "\n",
            "failures:\n",
            "\n",
            "---- my_mod::test_add stdout ----\n",
            "thread 'my_mod::test_add' panicked at 'assertion failed: `(left == right)`\n",
            "  left: `1`,\n",
            " right: `2`', src/lib.rs:15:5\n",
            "\n",
            "test result: FAILED. 0 passed; 1 failed; 0 ignored\n",
        );
        let out = PARSER.parse(input, DetectResult::Text);
        assert_eq!(out.diagnostics.len(), 1);
        let diag = &out.diagnostics[0];
        assert_eq!(diag.name, "my_mod::test_add");
        assert_eq!(diag.severity, Severity::Error);
        // Detail should contain the raw stdout block
        assert!(diag.detail.is_some());
        let detail = diag.detail.as_ref().unwrap();
        assert!(detail.contains("left"));
    }
}
