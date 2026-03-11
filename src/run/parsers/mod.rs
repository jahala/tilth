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

    /// Optional: rewrite the command string before execution (wrapper mode).
    fn rewrite(&self, _command: &str) -> Option<String> {
        None
    }

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

/// Detect the appropriate parser by scanning the first ~200 lines of content.
pub fn detect_from_content(input: &str) -> &'static dyn Parser {
    let sample: String = input.lines().take(200).collect::<Vec<_>>().join("\n");
    for p in PARSERS {
        if p.detect(&sample) {
            return *p;
        }
    }
    &generic::PARSER
}

/// Detect the appropriate parser from a command string (wrapper mode).
pub fn detect_from_command(command: &str) -> &'static dyn Parser {
    // Match command prefixes to parsers.
    let cmd = command.trim();
    if cmd.starts_with("cargo test") || cmd.starts_with("cargo nextest") {
        return &cargo_test::PARSER;
    }
    if cmd.starts_with("cargo build")
        || cmd.starts_with("cargo check")
        || cmd.starts_with("cargo clippy")
    {
        return &cargo_build::PARSER;
    }
    if cmd.starts_with("go test") {
        return &go_test::PARSER;
    }
    if cmd.starts_with("golangci-lint") {
        return &golangci_lint::PARSER;
    }
    // Jest / Vitest — handle bare, npx/yarn/pnpm prefixed invocations.
    let jest_keywords = ["jest", "vitest"];
    for kw in jest_keywords {
        if cmd.contains(kw) {
            return &jest::PARSER;
        }
    }
    if cmd.starts_with("pytest") || cmd.starts_with("python -m pytest") {
        return &pytest::PARSER;
    }
    // ESLint — bare, npx/yarn/pnpm prefixed.
    if cmd.contains("eslint") {
        return &eslint::PARSER;
    }
    // Ruff — `ruff check` or `ruff` bare.
    if cmd.starts_with("ruff") {
        return &ruff::PARSER;
    }
    // mypy — bare or `python -m mypy`.
    if cmd.starts_with("mypy") || cmd.starts_with("python -m mypy") {
        return &mypy::PARSER;
    }
    // tsc — bare, `npx tsc`, `yarn tsc`, `pnpm tsc`.
    // Match when any whitespace-delimited word is exactly `tsc`.
    if cmd.split_whitespace().any(|w| w == "tsc") {
        return &tsc::PARSER;
    }
    &generic::PARSER
}
