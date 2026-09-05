use std::fmt::Write;

/// Unknown file types: first 50 lines + last 10 lines.
#[must_use]
pub fn head_tail(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if total <= 60 {
        return content.to_string();
    }

    let omitted = total - 60;
    let mut result = lines[..50].join("\n");
    let _ = write!(result, "\n\n... {total} lines total, {omitted} omitted\n\n");
    result.push_str(&lines[total - 10..].join("\n"));
    result
}

/// Log files: first 10 lines + last 5 lines + total line count.
#[must_use]
pub fn log_view(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if total <= 15 {
        return content.to_string();
    }

    let mut result = lines[..10].join("\n");
    let omitted = total - 15;
    let _ = write!(result, "\n\n... {total} lines total, {omitted} omitted\n\n");
    result.push_str(&lines[total - 5..].join("\n"));
    result
}
