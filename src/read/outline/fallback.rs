use std::fmt::Write;

use crate::read::ends::{Ends, Shape};

/// The view of an unknown file type: its first 50 lines and its last 10.
pub(crate) const OTHER: Shape = Shape { head: 50, tail: 10 };

/// The view of a log file: its first 10 lines and its last 5.
pub(crate) const LOG: Shape = Shape { head: 10, tail: 5 };

/// The whole text when the ends are all of it, and otherwise the first
/// lines, a line naming the total and how many are left out, then the last
/// lines.
pub(crate) fn excerpt(ends: &Ends) -> String {
    let (head, tail) = (ends.head(), ends.tail());
    if ends.shows_all() {
        return format!("{head}{tail}");
    }
    let total = ends.total();
    let head: Vec<&str> = head.lines().collect();
    let tail: Vec<&str> = tail.lines().collect();
    let omitted = total - head.len() - tail.len();
    let mut result = head.join("\n");
    let _ = write!(result, "\n\n... {total} lines total, {omitted} omitted\n\n");
    result.push_str(&tail.join("\n"));
    result
}
