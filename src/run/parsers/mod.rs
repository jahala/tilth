pub mod cargo_build;
pub mod cargo_test;
pub mod eslint;
pub mod generic;
pub mod go_test;
pub mod golangci_lint;
pub mod jest;
pub mod mypy;
pub mod npm;
pub mod pip;
pub mod pytest;
pub mod ruff;
pub mod tsc;

use crate::run::types::{DetectResult, ParsedOutput};

/// A tool-specific parser that can detect its own input and produce structured output.
pub trait Parser: Send + Sync {
    fn name(&self) -> &'static str;

    /// Returns a `DetectResult` indicating whether `sample` (first ~200 lines)
    /// looks like output from this tool, and if so, what format it's in.
    fn detect(&self, sample: &str) -> DetectResult;

    fn parse(&self, input: &str, hint: DetectResult) -> ParsedOutput;
}

/// Static registry — ordered by detection priority.
/// JSON formats first (most distinctive fingerprints), then text-only.
/// The generic fallback is implicit and never registered here.
static PARSERS: &[&dyn Parser] = &[
    &cargo_test::PARSER,
    &cargo_build::PARSER,
    &go_test::PARSER,
    &golangci_lint::PARSER,
    &jest::PARSER,
    // JSON-capable linter parsers.
    &eslint::PARSER,
    &ruff::PARSER,
    &mypy::PARSER,
    // Text-only parsers — no distinctive JSON fingerprint.
    &pytest::PARSER,
    &tsc::PARSER,
    &npm::PARSER,
    &pip::PARSER,
];

/// Truncate input to the first `n` lines without allocation.
fn truncate_to_n_lines(input: &str, n: usize) -> &str {
    let mut count = 0;
    for (i, b) in input.bytes().enumerate() {
        if b == b'\n' {
            count += 1;
            if count >= n {
                return &input[..=i];
            }
        }
    }
    input
}

/// Detect the appropriate parser by scanning the first ~200 lines of content.
pub fn detect_from_content(input: &str) -> (&'static dyn Parser, DetectResult) {
    let sample = truncate_to_n_lines(input, 200);
    for p in PARSERS {
        let result = p.detect(sample);
        if result.matched() {
            return (*p, result);
        }
    }
    (&generic::PARSER, DetectResult::Text)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Bug 5 regression: ruff's weak fingerprint was before mypy in PARSERS.
    // Verify that mypy JSON is correctly detected as mypy (not ruff) when
    // detect_from_content walks the PARSERS array in order.
    #[test]
    fn detect_mypy_json_not_claimed_by_ruff() {
        let mypy_json = concat!(
            r#"{"file": "src/main.py", "line": 42, "column": 5, "severity": "error", "message": "Incompatible types in assignment", "code": "assignment"}"#,
            "\n",
        );
        let (parser, _result) = detect_from_content(mypy_json);
        assert_eq!(
            parser.name(),
            "mypy",
            "mypy NDJSON should be detected as mypy, not ruff (ruff is before mypy in PARSERS)"
        );
    }
}
