use std::fmt::Write as FmtWrite;

use crate::run::types::{CompressResult, Counts, DiagnosticGroup, Severity};

const HEAD_TAIL_LINES: usize = 10;
const MAX_DETAIL_LINES: usize = 10;

// ---------------------------------------------------------------------------
// Markdown format
// ---------------------------------------------------------------------------

pub fn format_markdown(result: &CompressResult) -> String {
    let mut out = String::with_capacity(512);

    write_markdown_header(&mut out, result);

    for group in &result.groups {
        let _ = out.write_char('\n');
        write_markdown_group(&mut out, group);
    }

    out
}

pub fn format_markdown_with_raw(result: &CompressResult, cleaned: &str) -> String {
    let mut out = String::with_capacity(512);

    write_markdown_header(&mut out, result);

    if !result.groups.is_empty() {
        let _ = out.write_char('\n');
        for group in &result.groups {
            write_markdown_group(&mut out, group);
        }
    }

    // Skip head/tail when errors dominate the input — the groups are the content.
    let error_line_count: usize = result.groups.iter().map(|g| g.total).sum();
    #[allow(clippy::cast_precision_loss)] // ratio comparison — precision loss is acceptable
    let error_ratio = error_line_count as f64 / result.input_stats.lines.max(1) as f64;
    if error_ratio > 0.8 {
        return out;
    }

    write_head_tail_markdown(&mut out, cleaned);

    out
}

fn write_markdown_header(out: &mut String, result: &CompressResult) {
    let _ = out.write_str("# ");
    let _ = out.write_str(result.tool);
    let _ = out.write_str(" — ");
    let _ = out.write_str(&result.summary);
    if let Some(d) = result.duration_secs {
        let _ = write!(out, " ({d:.1}s)");
    }
    let _ = out.write_char('\n');
}

fn write_markdown_group(out: &mut String, group: &DiagnosticGroup) {
    let cascade_tag = if group.cascading { " [cascading]" } else { "" };

    if group.locations.len() > 1 || group.total > 1 {
        let _ = writeln!(
            out,
            "# {} {} — {} occurrence(s){cascade_tag}",
            group.severity, group.signature, group.total,
        );
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
        let loc_str = group
            .representative
            .location
            .as_ref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "# {}{loc_str} {}{cascade_tag}",
            group.severity, group.signature,
        );
    }

    let _ = writeln!(out, "#   {}", group.representative.message);

    if let Some(detail) = &group.representative.detail {
        write_detail_markdown(out, detail);
    }
}

fn write_detail_markdown(out: &mut String, detail: &str) {
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

fn write_head_tail_markdown(out: &mut String, cleaned: &str) {
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
}

// ---------------------------------------------------------------------------
// Plain format
// ---------------------------------------------------------------------------

pub fn format_plain(result: &CompressResult) -> String {
    let mut out = String::with_capacity(512);

    write_plain_header(&mut out, result);

    for group in &result.groups {
        let _ = out.write_char('\n');
        write_plain_group(&mut out, group);
    }

    out
}

pub fn format_plain_with_raw(result: &CompressResult, cleaned: &str) -> String {
    let mut out = String::with_capacity(512);

    write_plain_header(&mut out, result);

    if !result.groups.is_empty() {
        let _ = out.write_char('\n');
        for group in &result.groups {
            write_plain_group(&mut out, group);
        }
    }

    let error_line_count: usize = result.groups.iter().map(|g| g.total).sum();
    #[allow(clippy::cast_precision_loss)] // ratio comparison — precision loss is acceptable
    let error_ratio = error_line_count as f64 / result.input_stats.lines.max(1) as f64;
    if error_ratio > 0.8 {
        return out;
    }

    write_head_tail_plain(&mut out, cleaned);

    out
}

fn write_plain_header(out: &mut String, result: &CompressResult) {
    let _ = out.write_str(result.tool);
    let _ = out.write_str(" — ");
    let _ = out.write_str(&result.summary);
    if let Some(d) = result.duration_secs {
        let _ = write!(out, " ({d:.1}s)");
    }
    let _ = out.write_char('\n');
}

fn write_plain_group(out: &mut String, group: &DiagnosticGroup) {
    let cascade_tag = if group.cascading { " [cascading]" } else { "" };

    if group.locations.len() > 1 || group.total > 1 {
        let _ = writeln!(
            out,
            "{} {} — {} occurrence(s){cascade_tag}",
            group.severity, group.signature, group.total,
        );
        if !group.locations.is_empty() {
            let _ = out.write_str("  ");
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
        let loc_str = group
            .representative
            .location
            .as_ref()
            .map(|l| format!(" {l}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{}{loc_str} {}{cascade_tag}",
            group.severity, group.signature,
        );
    }

    let _ = writeln!(out, "  {}", group.representative.message);

    if let Some(detail) = &group.representative.detail {
        write_detail_plain(out, detail);
    }
}

fn write_detail_plain(out: &mut String, detail: &str) {
    let total: usize = detail.lines().count();

    for (count, line) in detail.lines().enumerate() {
        if count >= MAX_DETAIL_LINES {
            let remaining = total - count;
            let _ = writeln!(out, "  ... ({remaining} more lines)");
            break;
        }
        let _ = writeln!(out, "  {line}");
    }
}

fn write_head_tail_plain(out: &mut String, cleaned: &str) {
    let lines: Vec<&str> = cleaned.lines().collect();
    let n = lines.len();
    let head_count = HEAD_TAIL_LINES.min(n);

    let _ = writeln!(out, "\n--- first {head_count} lines ---");
    for line in &lines[..head_count] {
        let _ = writeln!(out, "{line}");
    }

    if n > head_count * 2 {
        let tail_start = n - HEAD_TAIL_LINES.min(n);
        let tail_count = n - tail_start;
        let _ = writeln!(out, "--- last {tail_count} lines ---");
        for line in &lines[tail_start..] {
            let _ = writeln!(out, "{line}");
        }
    } else if n > head_count {
        let _ = writeln!(out, "--- last {} lines ---", n - head_count);
        for line in &lines[head_count..] {
            let _ = writeln!(out, "{line}");
        }
    }
}

// ---------------------------------------------------------------------------
// JSON format
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct JsonOutput<'a> {
    tool: &'a str,
    summary: &'a str,
    duration_secs: Option<f64>,
    counts: &'a Counts,
    groups: Vec<JsonGroup<'a>>,
}

#[derive(serde::Serialize)]
struct JsonGroup<'a> {
    severity: &'a str,
    signature: &'a str,
    total: usize,
    locations: Vec<String>,
    message: &'a str,
    cascading: bool,
}

#[derive(serde::Serialize)]
struct JsonOutputWithRaw<'a> {
    tool: &'a str,
    summary: &'a str,
    duration_secs: Option<f64>,
    counts: &'a Counts,
    groups: Vec<JsonGroup<'a>>,
    head: Vec<&'a str>,
    tail: Vec<&'a str>,
}

fn build_json_groups(result: &CompressResult) -> Vec<JsonGroup<'_>> {
    result
        .groups
        .iter()
        .map(|g| {
            let severity_str = match g.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
            };
            JsonGroup {
                severity: severity_str,
                signature: &g.signature,
                total: g.total,
                locations: g
                    .locations
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                message: &g.representative.message,
                cascading: g.cascading,
            }
        })
        .collect()
}

pub fn format_json(result: &CompressResult) -> String {
    let output = JsonOutput {
        tool: result.tool,
        summary: &result.summary,
        duration_secs: result.duration_secs,
        counts: &result.counts,
        groups: build_json_groups(result),
    };
    serde_json::to_string(&output).unwrap_or_default()
}

pub fn format_json_with_raw(result: &CompressResult, cleaned: &str) -> String {
    let lines: Vec<&str> = cleaned.lines().collect();
    let n = lines.len();
    let head_count = HEAD_TAIL_LINES.min(n);
    let head: Vec<&str> = lines[..head_count].to_vec();
    let tail: Vec<&str> = if n > head_count * 2 {
        let tail_start = n - HEAD_TAIL_LINES.min(n);
        lines[tail_start..].to_vec()
    } else if n > head_count {
        lines[head_count..].to_vec()
    } else {
        Vec::new()
    };

    let output = JsonOutputWithRaw {
        tool: result.tool,
        summary: &result.summary,
        duration_secs: result.duration_secs,
        counts: &result.counts,
        groups: build_json_groups(result),
        head,
        tail,
    };
    serde_json::to_string(&output).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::types::{Counts, Diagnostic, DiagnosticGroup, InputStats, Location, Severity};

    fn make_result(tool: &'static str, summary: &str, raw_lines: usize) -> CompressResult {
        CompressResult {
            tool,
            summary: summary.to_string(),
            groups: Vec::new(),
            counts: Counts::default(),
            duration_secs: None,
            input_stats: InputStats {
                lines: raw_lines,
                bytes: 0,
            },
            passthrough: false,
            cleaned: None,
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
        let mut result = make_result("cargo test", "1 failed", 100);
        result.groups = vec![make_group(Severity::Error, "test_foo", 1)];
        let out = format_markdown(&result);
        assert!(out.contains("cargo test"));
        assert!(out.contains("1 failed"));
    }

    #[test]
    fn detail_truncation() {
        let detail: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let mut out = String::new();
        write_detail_markdown(&mut out, &detail);
        assert!(out.contains("more lines"));
        let lines: Vec<_> = out.lines().collect();
        // Should be MAX_DETAIL_LINES content lines + 1 truncation line.
        assert_eq!(lines.len(), MAX_DETAIL_LINES + 1);
    }

    #[test]
    fn plain_format_no_hash_prefix() {
        let mut result = make_result("cargo test", "1 failed", 100);
        result.groups = vec![make_group(Severity::Error, "test_foo", 1)];
        let out = format_plain(&result);
        assert!(!out.contains("# "));
        assert!(out.contains("cargo test"));
    }

    #[test]
    fn json_format_parses() {
        let result = make_result("cargo test", "1 failed", 100);
        let out = format_json(&result);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(parsed["tool"], "cargo test");
    }
}
