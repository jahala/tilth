use tilth_core::types::Lang;
use tilth_core::{test_entries, TestKind};

/// Extract test structure (describe/it/test) via tree-sitter queries.
/// Returns a structured test outline with suite nesting, or None if
/// no test structure was found. At most `max_lines` entries are rendered.
#[must_use]
pub fn outline(content: &str, lang: Lang, max_lines: usize) -> Option<String> {
    let entries = test_entries(content, lang);
    if entries.is_empty() || max_lines == 0 {
        return None;
    }

    let rendered: Vec<String> = entries
        .iter()
        .take(max_lines)
        .map(|e| {
            let indent = "  ".repeat(usize::from(e.depth));
            let label = match e.kind {
                TestKind::Suite => "suite",
                TestKind::Case => "test",
            };
            format!(
                "{indent}[{}] {label}: {}(\"{}\")",
                e.start_line, e.callee, e.name
            )
        })
        .collect();

    Some(rendered.join("\n"))
}
