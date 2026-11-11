use std::fmt::Write;

/// How many leading lines [`head_tail`] shows.
pub(crate) const HEAD_LINES: usize = 50;
/// How many trailing lines [`head_tail`] shows.
pub(crate) const TAIL_LINES: usize = 10;

/// Unknown file types: the first [`HEAD_LINES`] lines and the last
/// [`TAIL_LINES`].
pub fn head_tail(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if total <= HEAD_LINES + TAIL_LINES {
        return content.to_string();
    }
    join_head_tail(&lines[..HEAD_LINES], &lines[total - TAIL_LINES..], total)
}

/// The head-and-tail shape: `head`, a line naming the file's `total` lines
/// and how many are left out, then `tail`. One home for the shape, whether
/// the lines came from the whole text or from a file's two ends.
pub(crate) fn join_head_tail(head: &[&str], tail: &[&str], total: usize) -> String {
    let omitted = total.saturating_sub(head.len() + tail.len());
    let mut result = head.join("\n");
    let _ = write!(result, "\n\n... {total} lines total, {omitted} omitted\n\n");
    result.push_str(&tail.join("\n"));
    result
}

/// Log files: first 10 lines + last 5 lines + total line count.
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
