use crate::types::Lang;

/// The kind of a test-structure entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// A grouping call: `describe(...)` or `context(...)`.
    Suite,
    /// A single test: `it(...)`, `test(...)`, or `specify(...)`.
    Case,
}

/// One suite or case found in a test file, in source order with its nesting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEntry {
    /// The test's title: the call's first string argument, quotes stripped.
    pub name: String,
    /// The function called: `describe`, `context`, `it`, `test`, or `specify`.
    pub callee: String,
    /// Suite or case.
    pub kind: TestKind,
    /// 1-based line of the call.
    pub start_line: u32,
    /// 1-based line where the call expression ends.
    pub end_line: u32,
    /// Nesting depth: 0 at the top level, one more per enclosing suite.
    pub depth: u8,
}

/// Walk `content` for `describe`/`context`/`it`/`test`/`specify` calls and
/// return them as data, in pre-order, each with its nesting depth. Languages
/// without a tree-sitter grammar, and files with no such calls, yield an
/// empty vector. Cases derived from naming conventions (`test_*`, `#[test]`,
/// `Test*`) are not part of this walk; read the outline for those.
#[must_use]
pub fn test_entries(content: &str, lang: Lang) -> Vec<TestEntry> {
    let Some(language) = crate::lang::outline::outline_language(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut entries = Vec::new();
    extract_test_calls(tree.root_node(), &lines, 0, &mut entries);
    entries
}

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

/// Recursively find describe/it/test call expressions.
fn extract_test_calls(
    node: tree_sitter::Node,
    lines: &[&str],
    depth: usize,
    entries: &mut Vec<TestEntry>,
) {
    let kind = node.kind();

    // Look for call expressions: describe(...), it(...), test(...)
    if kind == "call_expression" || kind == "expression_statement" {
        if let Some((callee, name)) = extract_test_name(node, lines) {
            let test_kind = if callee.starts_with("describe") || callee.starts_with("context") {
                TestKind::Suite
            } else {
                TestKind::Case
            };
            entries.push(TestEntry {
                name,
                callee,
                kind: test_kind,
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                depth: u8::try_from(depth).unwrap_or(u8::MAX),
            });

            // Recurse into the callback body for nested describes
            if test_kind == TestKind::Suite {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_test_calls(child, lines, depth + 1, entries);
                }
                return;
            }
        }
    }

    // Recurse
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_test_calls(child, lines, depth, entries);
    }
}

/// Extract the function name and first string argument from a call expression.
/// Returns `(callee, title)`, the title with its quotes stripped.
fn extract_test_name(node: tree_sitter::Node, lines: &[&str]) -> Option<(String, String)> {
    let mut cursor = node.walk();

    // Find the function name
    let func = node.children(&mut cursor).find(|c| {
        let k = c.kind();
        k == "identifier" || k == "member_expression" || k == "call_expression"
    })?;

    let func_text = get_node_text(func, lines);
    if !matches!(
        func_text.as_str(),
        "describe" | "it" | "test" | "context" | "specify"
    ) {
        return None;
    }

    // Find the first string argument
    let mut cursor2 = node.walk();
    let args = node
        .children(&mut cursor2)
        .find(|c| c.kind() == "arguments")?;

    let mut cursor3 = args.walk();
    let first_arg = args.children(&mut cursor3).find(|c| {
        let k = c.kind();
        k == "string" || k == "template_string" || k == "string_literal"
    })?;

    let arg_text = get_node_text(first_arg, lines);
    // Strip quotes
    let cleaned = arg_text
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`');

    Some((func_text, cleaned.to_string()))
}

fn get_node_text(node: tree_sitter::Node, lines: &[&str]) -> String {
    let row = node.start_position().row;
    let col_start = node.start_position().column;
    let end_row = node.end_position().row;

    if row < lines.len() && row == end_row {
        let col_end = node.end_position().column.min(lines[row].len());
        lines[row][col_start..col_end].to_string()
    } else if row < lines.len() {
        lines[row][col_start..].to_string()
    } else {
        String::new()
    }
}
