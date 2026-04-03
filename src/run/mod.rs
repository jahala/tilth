mod ansi;
pub mod exec;
mod format;
mod group;
pub(crate) mod parsers;
pub mod types;

pub use types::{CompressResult, Counts, InputStats, OutputFormat};

use std::panic::AssertUnwindSafe;

/// Process command output into a structured result: strip ANSI, detect tool, parse, group.
///
/// Returns a `CompressResult` that can be formatted lazily via `format()` or `format_checked()`.
#[must_use]
pub fn process_structured(input: &str) -> CompressResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| process_structured_inner(input)))
        .unwrap_or_else(|_| passthrough_result(input))
}

/// Process command output: strip ANSI, detect tool, parse, group, format.
///
/// Returns the original input unchanged if:
/// - Input is empty
/// - Input contains binary (null bytes)
/// - Input is <=20 lines and unrecognised
/// - Formatted output would be longer than the input (never-worse guarantee)
/// - Any internal panic occurs
#[allow(dead_code)]
#[must_use]
pub fn process(input: &str) -> String {
    let result = process_structured(input);
    if result.passthrough {
        return input.to_string();
    }
    result
        .format_checked(OutputFormat::Markdown, input.len())
        .unwrap_or_else(|| input.to_string())
}

fn process_structured_inner(input: &str) -> CompressResult {
    if input.is_empty() {
        return passthrough_result(input);
    }

    // Binary detection: null bytes in first 512 bytes -> passthrough.
    let check_len = input.len().min(512);
    if input.as_bytes()[..check_len].contains(&0) {
        return passthrough_result(input);
    }

    // Strip ANSI.
    let cleaned = ansi::strip(input);

    // Detect tool.
    let (parser, hint) = parsers::detect_from_content(&cleaned);

    // Short unknown input: invisible passthrough.
    if parser.name() == "unknown" && cleaned.lines().nth(20).is_none() {
        return passthrough_result(input);
    }

    // Parse.
    let parsed = parser.parse(&cleaned, hint);

    // Group.
    let groups = group::group(&parsed.diagnostics);

    // Build CompressResult.
    let cleaned_for_generic = if parsed.tool == "unknown" {
        Some(cleaned)
    } else {
        None
    };

    CompressResult {
        tool: parsed.tool,
        summary: parsed.summary,
        groups,
        counts: parsed.counts,
        duration_secs: parsed.duration_secs,
        input_stats: InputStats {
            lines: parsed.raw_lines,
            bytes: parsed.raw_bytes,
        },
        passthrough: false,
        cleaned: cleaned_for_generic,
    }
}

fn passthrough_result(input: &str) -> CompressResult {
    CompressResult {
        tool: "passthrough",
        summary: String::new(),
        groups: Vec::new(),
        counts: Counts::default(),
        duration_secs: None,
        input_stats: InputStats {
            lines: input.lines().count(),
            bytes: input.len(),
        },
        passthrough: true,
        cleaned: None,
    }
}
