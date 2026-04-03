use std::collections::VecDeque;
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

    // Skip head/tail when error groups dominate the input — the groups are the content.
    // Each group contributes ~5 lines of formatted output; if that fills >80% of input, skip.
    let estimated_error_lines = result.groups.len() * 5;
    #[allow(clippy::cast_precision_loss)] // ratio comparison — precision loss is acceptable
    if estimated_error_lines as f64 / result.input_stats.lines.max(1) as f64 > 0.8 {
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

struct HeadTail<'a> {
    head: Vec<&'a str>,
    tail: VecDeque<&'a str>,
    total: usize,
}

fn collect_head_tail(text: &str) -> HeadTail<'_> {
    let mut head: Vec<&str> = Vec::with_capacity(HEAD_TAIL_LINES);
    let mut tail: VecDeque<&str> = VecDeque::with_capacity(HEAD_TAIL_LINES);
    let mut total = 0usize;

    for line in text.lines() {
        if total < HEAD_TAIL_LINES {
            head.push(line);
        } else {
            if tail.len() == HEAD_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        total += 1;
    }

    HeadTail { head, tail, total }
}

fn write_head_tail_markdown(out: &mut String, cleaned: &str) {
    let HeadTail { head, tail, total } = collect_head_tail(cleaned);
    let head_count = head.len();

    let _ = writeln!(out, "#\n# --- first {head_count} lines ---");
    for line in &head {
        let _ = writeln!(out, "# {line}");
    }

    if total > head_count * 2 {
        let tail_count = tail.len();
        let _ = writeln!(out, "# --- last {tail_count} lines ---");
        for line in &tail {
            let _ = writeln!(out, "# {line}");
        }
    } else if total > head_count {
        let _ = writeln!(out, "# --- last {} lines ---", total - head_count);
        for line in &tail {
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

    // Skip head/tail when error groups dominate the input — the groups are the content.
    // Each group contributes ~5 lines of formatted output; if that fills >80% of input, skip.
    let estimated_error_lines = result.groups.len() * 5;
    #[allow(clippy::cast_precision_loss)] // ratio comparison — precision loss is acceptable
    if estimated_error_lines as f64 / result.input_stats.lines.max(1) as f64 > 0.8 {
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
    let HeadTail { head, tail, total } = collect_head_tail(cleaned);
    let head_count = head.len();

    let _ = writeln!(out, "\n--- first {head_count} lines ---");
    for line in &head {
        let _ = writeln!(out, "{line}");
    }

    if total > head_count * 2 {
        let tail_count = tail.len();
        let _ = writeln!(out, "--- last {tail_count} lines ---");
        for line in &tail {
            let _ = writeln!(out, "{line}");
        }
    } else if total > head_count {
        let _ = writeln!(out, "--- last {} lines ---", total - head_count);
        for line in &tail {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
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
                detail: g.representative.detail.as_deref(),
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
    serde_json::to_string(&output).expect("CompressResult JSON is always serializable")
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
    serde_json::to_string(&output).expect("CompressResult JSON is always serializable")
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

    #[test]
    fn json_format_includes_detail_field() {
        let mut result = make_result("cargo test", "1 failed", 100);
        let mut group = make_group(Severity::Error, "test_foo", 1);
        group.representative.detail = Some("expected 1, got 2\nstack trace here".to_string());
        result.groups = vec![group];

        let out = format_json(&result);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let detail = parsed["groups"][0]["detail"]
            .as_str()
            .expect("detail field must be present");
        assert!(detail.contains("expected 1, got 2"));
    }

    #[test]
    fn json_format_omits_detail_when_none() {
        let mut result = make_result("cargo test", "1 failed", 100);
        result.groups = vec![make_group(Severity::Error, "test_foo", 1)];

        let out = format_json(&result);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(
            parsed["groups"][0].get("detail").is_none(),
            "detail field should be omitted when None"
        );
    }

    #[test]
    fn json_format_never_empty_string() {
        let result = make_result("cargo test", "ok", 10);
        let out = format_json(&result);
        assert!(!out.is_empty(), "JSON output must never be empty");
        // Must be valid JSON
        serde_json::from_str::<serde_json::Value>(&out).expect("must be valid JSON");
    }

    #[test]
    fn head_tail_not_suppressed_by_inflated_diagnostic_count() {
        // 100-line input, 5 error groups with total=20 each (100 diagnostics total).
        // The error_ratio should not suppress head/tail — only 5 distinct error patterns
        // exist, and many lines are non-error content (build progress, etc.).
        let mut result = make_result("cargo build", "5 errors", 100);
        result.groups = (0..5)
            .map(|i| {
                let mut g = make_group(Severity::Error, &format!("E{i:03}"), 20);
                g.total = 20;
                g
            })
            .collect();

        let cleaned: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let out = format_markdown_with_raw(&result, &cleaned);
        assert!(
            out.contains("first"),
            "head/tail should NOT be suppressed when diagnostic count inflates the ratio"
        );
    }
}
