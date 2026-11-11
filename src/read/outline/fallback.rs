use std::fmt::Write;

use crate::read::ends::{Ends, Shape};

/// The view of an unknown file type: its first 50 lines and its last 10.
pub(crate) const OTHER: Shape = Shape { head: 50, tail: 10 };

/// The view of a log file: its first 10 lines and its last 5.
pub(crate) const LOG: Shape = Shape { head: 10, tail: 5 };

/// Unknown file types: first 50 lines + last 10 lines.
pub fn head_tail(content: &str) -> String {
    excerpt(content, OTHER)
}

/// Log files: first 10 lines + last 5 lines + total line count.
pub fn log_view(content: &str) -> String {
    excerpt(content, LOG)
}

/// `content` itself when it has no more lines than `shape` shows, and
/// otherwise its two ends around a line that counts what was left out.
fn excerpt(content: &str, shape: Shape) -> String {
    let ends = Ends::of_text(content, shape);
    if ends.shows_all() {
        return content.to_string();
    }
    join(&ends)
}

/// The first lines, a line naming the total and how many are left out, then
/// the last lines. For ends that leave lines out; a text with no more lines
/// than its view shows is its own view.
pub(crate) fn join(ends: &Ends) -> String {
    let head = ends.head_lines();
    let tail = ends.tail_lines();
    let total = ends.total();
    let omitted = total - head.len() - tail.len();
    let mut result = head.join("\n");
    let _ = write!(result, "\n\n... {total} lines total, {omitted} omitted\n\n");
    result.push_str(&tail.join("\n"));
    result
}
