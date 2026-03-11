pub mod cargo_build;
pub mod cargo_test;
pub mod eslint;
pub mod generic;
pub mod go_test;
pub mod golangci_lint;
pub mod jest;
pub mod mypy;
pub mod pytest;
pub mod ruff;
pub mod tsc;

use crate::run::types::ParsedOutput;

/// A tool-specific parser that can detect its own input and produce structured output.
pub trait Parser: Send + Sync {
    fn name(&self) -> &'static str;

    /// Returns true if `sample` (first ~200 lines) looks like output from this tool.
    fn detect(&self, sample: &str) -> bool;

    fn parse(&self, input: &str) -> ParsedOutput;
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
pub fn detect_from_content(input: &str) -> &'static dyn Parser {
    let sample = truncate_to_n_lines(input, 200);
    for p in PARSERS {
        if p.detect(sample) {
            return *p;
        }
    }
    &generic::PARSER
}
