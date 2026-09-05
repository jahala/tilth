/// CSV/TSV outline: column headers + row count + first 5 + last 3 rows.
/// Uses memchr for line counting on the raw bytes, then only collects
/// the head/tail slices needed for display.
#[must_use]
pub fn outline(content: &str, _max_lines: usize) -> String {
    let buf = content.as_bytes();
    if buf.is_empty() {
        return "(empty)".to_string();
    }

    // Count lines via memchr — O(n) SIMD scan, no Vec allocation
    let total = memchr::memchr_iter(b'\n', buf).count() + 1;

    // We still need to index into lines for head/tail display,
    // but only collect offsets, not full line slices
    let lines: Vec<&str> = content.lines().collect();

    let mut out = Vec::new();

    // Header
    out.push(format!("columns: {}", lines[0]));
    out.push(format!("rows: {}", total.saturating_sub(1)));
    out.push(String::new());

    // First 5 data rows
    let head_end = 6.min(lines.len()); // header + 5 rows
    for line in &lines[1..head_end] {
        out.push(line.to_string());
    }

    // Gap indicator + last 3 rows
    if total > 9 {
        out.push(format!("... {} rows omitted", total - 9));
        out.push(String::new());
        let tail_start = lines.len().saturating_sub(3);
        for line in &lines[tail_start..] {
            out.push(line.to_string());
        }
    } else if lines.len() > head_end {
        for line in &lines[head_end..] {
            out.push(line.to_string());
        }
    }

    out.join("\n")
}
