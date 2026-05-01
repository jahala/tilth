mod ansi;
pub mod exec;
mod format;
mod group;
pub(crate) mod parsers;
pub mod types;

pub use types::{CompressResult, Counts, InputStats, OutputFormat};

/// Process command output into a structured result: strip ANSI, detect tool, parse, group.
///
/// Returns a `CompressResult` that can be formatted lazily via `format()` or `format_checked()`.
#[must_use]
pub fn process_structured(input: &str) -> CompressResult {
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

/// Process command output: strip ANSI, detect tool, parse, group, format.
///
/// Returns the original input unchanged if:
/// - Input is empty
/// - Input contains binary (null bytes)
/// - Input is <=20 lines and unrecognised
/// - Formatted output would be longer than the input (never-worse guarantee)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_passthrough() {
        let result = process_structured("");
        assert!(result.passthrough);
    }

    #[test]
    fn short_unknown_input_is_passthrough() {
        let result = process_structured("hello world\n");
        assert!(result.passthrough);
    }

    #[test]
    fn binary_input_is_passthrough() {
        let input = "hello\x00world";
        let result = process_structured(input);
        assert!(result.passthrough);
    }

    #[test]
    fn process_returns_original_for_short_input() {
        let input = "some short output\n";
        let output = process(input);
        assert_eq!(output, input);
    }

    #[test]
    fn passthrough_result_format_checked_returns_none() {
        // Demonstrates that passthrough correctness in tool_run (mcp.rs) is accidental:
        // format_checked on a passthrough result returns None because the formatted output
        // is longer than the original, so the fallback kicks in. This works by accident.
        let input = "short output\n";
        let result = process_structured(input);
        assert!(
            result.passthrough,
            "short unknown input should be passthrough"
        );
        // format_checked should return None for passthrough results
        let formatted = result.format_checked(OutputFormat::Markdown, input.len());
        assert!(
            formatted.is_none(),
            "passthrough format_checked should return None (accidental correctness)"
        );
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
