use std::fmt::Write as FmtWrite;

use crate::run::types::{DiagnosticGroup, ParsedOutput};

const HEAD_TAIL_LINES: usize = 10;
const MAX_DETAIL_LINES: usize = 10;

/// Format parsed output into a human-readable (and AI-readable) summary.
///
/// Returns the formatted string. The caller is responsible for the never-worse
/// check (falling back to original input when formatted >= original length).
pub fn format_output(parsed: &ParsedOutput, groups: &[DiagnosticGroup]) -> String {
    if parsed.tool == "unknown" {
        // Unknown tools are formatted via format_generic_with_raw; this path
        // only fires if someone calls format_output directly for unknown.
        return String::new();
    }

    format_summary(parsed, groups)
}


// ---------------------------------------------------------------------------
// Summary format (non-generic tools)
// ---------------------------------------------------------------------------

fn format_summary(parsed: &ParsedOutput, groups: &[DiagnosticGroup]) -> String {
    let mut out = String::with_capacity(512);

    write_header_line(&mut out, parsed);

    for group in groups {
        let _ = out.write_char('\n');
        write_group_block(&mut out, group);
    }

    out
}

// ---------------------------------------------------------------------------
// Shared formatting helpers
// ---------------------------------------------------------------------------

fn write_header_line(out: &mut String, parsed: &ParsedOutput) {
    let _ = out.write_str("# ");
    let _ = out.write_str(parsed.tool);
    let _ = out.write_str(" — ");
    let _ = out.write_str(&parsed.summary);
    if let Some(d) = parsed.duration_secs {
        let _ = write!(out, " ({d:.1}s)");
    }
    let _ = out.write_char('\n');
}


fn write_group_block(out: &mut String, group: &DiagnosticGroup) {
    // Header line
    let cascade_tag = if group.cascading { " [cascading]" } else { "" };

    if group.locations.len() > 1 || group.total > 1 {
        // Multiple locations or occurrences
        let _ = writeln!(
            out,
            "# {} {} — {} occurrence(s){cascade_tag}",
            group.severity,
            group.signature,
            group.total,
        );
        // List locations
        if !group.locations.is_empty() {
            let _ = out.write_str("#   ");
            let mut first = true;
            for loc in &group.locations {
                if !first {
                    let _ = out.write_str(", ");
                }
                let _ = out.write_str(&loc.to_string());
                first = false;
            }
            let extra = group.total.saturating_sub(group.locations.len());
            if extra > 0 {
                let _ = write!(out, " (+{extra} more)");
            }
            let _ = out.write_char('\n');
        }
    } else {
        // Single occurrence
        let loc_str = group
            .representative
            .location
            .as_ref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "# {}{loc_str} {}{cascade_tag}",
            group.severity,
            group.signature,
        );
    }

    // Message
    let _ = writeln!(out, "#   {}", group.representative.message);

    // Detail (truncated)
    if let Some(detail) = &group.representative.detail {
        write_detail(out, detail);
    }
}
/// Write detail lines, truncating to MAX_DETAIL_LINES.
fn write_detail(out: &mut String, detail: &str) {
    let total: usize = detail.lines().count();

    for (count, line) in detail.lines().enumerate() {
        if count >= MAX_DETAIL_LINES {
            let remaining = total - count;
            let _ = writeln!(out, "#   ... ({remaining} more lines)");
            break;
        }
        let _ = writeln!(out, "#   {line}");
    }
}

// ---------------------------------------------------------------------------
// Generic format with head/tail (called from process_inner with access to raw)
// ---------------------------------------------------------------------------

/// Format generic output with head/tail sections from the original cleaned lines.
///
/// Shows grouped error lines first, then head/tail of the raw output for context.
/// Skips head/tail when error lines dominate (>80% of input) — the groups already
/// contain all useful information.
pub fn format_generic_with_raw(
    parsed: &ParsedOutput,
    groups: &[DiagnosticGroup],
    cleaned: &str,
) -> String {
    let mut out = String::with_capacity(512);

    write_header_line(&mut out, parsed);

    if !groups.is_empty() {
        let _ = out.write_char('\n');
        for group in groups {
            write_group_block(&mut out, group);
        }
    }

    // Skip head/tail when errors dominate the input — the groups are the content.
    let error_line_count: usize = groups.iter().map(|g| g.total).sum();
    let error_ratio = error_line_count as f64 / parsed.raw_lines.max(1) as f64;
    if error_ratio > 0.8 {
        return out;
    }

    // Head + tail for context around the errors.
    let lines: Vec<&str> = cleaned.lines().collect();
    let n = lines.len();
    let head_count = HEAD_TAIL_LINES.min(n);

    let _ = writeln!(out, "#\n# --- first {head_count} lines ---");
    for line in &lines[..head_count] {
        let _ = writeln!(out, "# {line}");
    }

    if n > head_count * 2 {
        let tail_start = n - HEAD_TAIL_LINES.min(n);
        let tail_count = n - tail_start;
        let _ = writeln!(out, "# --- last {tail_count} lines ---");
        for line in &lines[tail_start..] {
            let _ = writeln!(out, "# {line}");
        }
    } else if n > head_count {
        let _ = writeln!(out, "# --- last {} lines ---", n - head_count);
        for line in &lines[head_count..] {
            let _ = writeln!(out, "# {line}");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::types::{Counts, Diagnostic, DiagnosticGroup, Location, ParsedOutput, Severity};

    fn make_parsed(tool: &'static str, summary: &str, raw_lines: usize) -> ParsedOutput {
        ParsedOutput {
            tool,
            summary: summary.to_string(),
            diagnostics: Vec::new(),
            counts: Counts::default(),
            duration_secs: None,
            raw_lines,
            raw_bytes: 0,
        }
    }

    fn make_group(severity: Severity, sig: &str, total: usize) -> DiagnosticGroup {
        DiagnosticGroup {
            severity,
            signature: sig.to_string(),
            locations: vec![Location {
                file: "src/main.rs".to_string(),
                line: 10,
                column: None,
            }],
            total,
            representative: Diagnostic {
                severity,
                location: None,
                name: sig.to_string(),
                message: "something went wrong".to_string(),
                detail: None,
            },
            cascading: false,
        }
    }

    #[test]
    fn summary_format_contains_tool() {
        let parsed = make_parsed("cargo test", "1 failed", 100);
        let group = make_group(Severity::Error, "test_foo", 1);
        let out = format_output(&parsed, &[group]);
        assert!(out.contains("cargo test"));
        assert!(out.contains("1 failed"));
    }

    #[test]
    fn detail_truncation() {
        let detail: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let mut out = String::new();
        write_detail(&mut out, &detail);
        assert!(out.contains("more lines"));
        let lines: Vec<_> = out.lines().collect();
        // Should be MAX_DETAIL_LINES content lines + 1 truncation line.
        assert_eq!(lines.len(), MAX_DETAIL_LINES + 1);
    }
}
