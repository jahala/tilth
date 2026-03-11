mod ansi;
mod format;
mod group;
mod parsers;
mod types;

use std::panic::AssertUnwindSafe;

/// Process command output: strip ANSI, detect tool, parse, group, format.
///
/// Returns the original input unchanged if:
/// - Input is empty
/// - Input contains binary (null bytes)
/// - Input is ≤20 lines and unrecognised
/// - Formatted output would be longer than the input (never-worse guarantee)
/// - Any internal panic occurs
pub fn process(input: &str) -> String {
    std::panic::catch_unwind(AssertUnwindSafe(|| process_inner(input)))
        .unwrap_or_else(|_| input.to_string())
}

fn process_inner(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    // Binary detection: null bytes in first 512 bytes → passthrough.
    let check_len = input.len().min(512);
    if input.as_bytes()[..check_len].contains(&0) {
        return input.to_string();
    }

    // Strip ANSI.
    let cleaned = ansi::strip(input);

    // Detect tool.
    let parser = parsers::detect_from_content(&cleaned);

    // Short unknown input: invisible passthrough.
    if parser.name() == "unknown" && cleaned.lines().nth(20).is_none() {
        return input.to_string();
    }

    // Parse.
    let parsed = parser.parse(&cleaned);

    // Group.
    let groups = group::group(&parsed.diagnostics);

    // Format — generic tool gets head/tail treatment with raw lines.
    let formatted = if parsed.tool == "unknown" {
        format::format_generic_with_raw(&parsed, &groups, &cleaned)
    } else {
        format::format_output(&parsed, &groups)
    };

    // Never-worse: formatted must be strictly shorter than original.
    if formatted.len() >= input.len() {
        return input.to_string();
    }

    formatted
}
