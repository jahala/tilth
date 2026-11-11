use crate::read::ends::{Ends, Shape};

/// The CSV/TSV view: the header line and 5 rows, then the last 3 rows.
pub(crate) const TABULAR: Shape = Shape { head: 6, tail: 3 };

/// CSV/TSV outline: column headers + row count + first 5 + last 3 rows.
pub fn outline(content: &str, _max_lines: usize) -> String {
    render(&Ends::of_text(content, TABULAR))
}

/// The CSV/TSV outline of a text's [`TABULAR`] ends. A text with more rows
/// than the outline shows marks the gap and counts the rows left out.
pub(crate) fn render(ends: &Ends) -> String {
    let head = ends.head_lines();
    let Some((columns, rows)) = head.split_first() else {
        return "(empty)".to_string();
    };
    let tail = ends.tail_lines();
    let total = ends.total();

    let mut out = Vec::new();
    out.push(format!("columns: {columns}"));
    out.push(format!("rows: {}", total - 1));
    out.push(String::new());
    out.extend(rows.iter().map(ToString::to_string));
    if !ends.shows_all() {
        out.push(format!(
            "... {} rows omitted",
            total - head.len() - tail.len()
        ));
        out.push(String::new());
    }
    out.extend(tail.iter().map(ToString::to_string));
    out.join("\n")
}
