//! Test structure as data: the `describe`/`context`/`it`/`test`/`specify`
//! calls and Bats cases, in source order with their nesting.

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
    /// The function called (`describe`, `context`, `it`, `test`, or `specify`), or Bats `@test`.
    pub callee: String,
    /// Suite or case.
    pub kind: TestKind,
    /// 1-based line of the call.
    pub start_line: u32,
    /// 1-based line where the call expression or Bats body ends.
    pub end_line: u32,
    /// Nesting depth: 0 at the top level, one more per enclosing suite.
    pub depth: u8,
}

/// Walk `content` for `describe`/`context`/`it`/`test`/`specify` calls and
/// return them as data, in pre-order, each with its nesting depth. Languages
/// without a tree-sitter grammar, and files with no such calls or Bats cases, yield an
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

    if lang == Lang::Bash {
        return extract_bats_cases(tree.root_node(), content, &mut parser);
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut entries = Vec::new();
    extract_test_calls(tree.root_node(), &lines, 0, &mut entries);
    entries
}

/// Bash parses `@test "title" {` as a command, without pairing the braces.
/// Rewrite only those headers as functions in a scratch copy so Bash can
/// find each body's closing brace. Preserve line breaks for the result ranges.
fn extract_bats_cases(
    root: tree_sitter::Node,
    content: &str,
    parser: &mut tree_sitter::Parser,
) -> Vec<TestEntry> {
    let mut titles = std::collections::BTreeMap::new();
    let mut normalized = String::new();
    let mut copied_until = 0;
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let Some(node) = leading_bash_command(statement) else {
            continue;
        };
        let (Some(name), Some(title), Some(brace)) = (
            node.named_child(0),
            node.named_child(1),
            node.named_child(2),
        ) else {
            continue;
        };
        let bytes = content.as_bytes();
        let Ok(open) = brace.utf8_text(bytes) else {
            continue;
        };
        // Bash may combine an empty "{ }" block into one word.
        let empty_block = open
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .is_some_and(|inner| inner.trim().is_empty());
        if name.utf8_text(bytes) != Ok("@test") || (open != "{" && !empty_block) {
            continue;
        }
        let quote = match title.kind() {
            "string" => '"',
            "raw_string" => '\'',
            _ => continue,
        };
        let Ok(raw) = title.utf8_text(bytes) else {
            continue;
        };
        let Some(title) = raw.strip_prefix(quote).and_then(|s| s.strip_suffix(quote)) else {
            continue;
        };
        let start = node.start_byte();
        let end = brace.start_byte();
        if !content[..start]
            .rsplit('\n')
            .next()
            .is_some_and(|line| line.trim().is_empty())
        {
            continue;
        }
        normalized.push_str(&content[copied_until..start]);
        normalized.push_str("t()");
        normalized.extend(content[start..end].chars().filter(|&ch| ch == '\n'));
        // Bats allows an empty block; a Bash function needs a command.
        normalized.push_str(" { :; ");
        copied_until = end + 1;
        let position = node.start_position();
        titles.insert((position.row, position.column), title);
    }

    if titles.is_empty() {
        return Vec::new();
    }
    normalized.push_str(&content[copied_until..]);
    let Some(tree) = parser.parse(normalized, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter_map(|node| {
            let position = node.start_position();
            let title = titles.get(&(position.row, position.column))?;
            let body = node.child_by_field_name("body")?;
            let close = body.child(body.child_count().checked_sub(1)?)?;
            if node.kind() != "function_definition" || close.kind() != "}" || close.is_missing() {
                return None;
            }
            Some(TestEntry {
                name: (*title).to_owned(),
                callee: "@test".to_owned(),
                kind: TestKind::Case,
                start_line: node.start_position().row as u32 + 1,
                end_line: close.end_position().row as u32 + 1,
                depth: 0,
            })
        })
        .collect()
}

/// A trailing redirect, pipe, or conditional can wrap the header's command.
/// Follow only the leading statement; commands inside bodies are not headers.
fn leading_bash_command(mut node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    while matches!(node.kind(), "redirected_statement" | "pipeline" | "list") {
        let first = node.named_child(0)?;
        if first.start_byte() != node.start_byte() {
            return None;
        }
        node = first;
    }
    (node.kind() == "command").then_some(node)
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

#[cfg(test)]
mod bats_tests {
    use super::*;

    #[test]
    fn bats_names_cases_and_their_brace_ranges() {
        let source = r#"setup() { echo ready; }
@test "rejects empty input" {
  run ./check.sh ""
  [ "$status" -eq 1 ]
}
@test 'unicode π and a literal }' {
  helper() { echo "}"; }
  cat <<'END'
}
@test "inside a heredoc" {
END
  run helper
}
@test "inline" { run true; }
teardown() { echo done; }
"#;
        let actual: Vec<_> = test_entries(source, Lang::Bash)
            .into_iter()
            .map(|entry| {
                (
                    entry.name,
                    entry.kind,
                    entry.start_line,
                    entry.end_line,
                    entry.depth,
                )
            })
            .collect();
        assert_eq!(
            actual,
            [
                ("rejects empty input".into(), TestKind::Case, 2, 5, 0),
                ("unicode π and a literal }".into(), TestKind::Case, 6, 13, 0),
                ("inline".into(), TestKind::Case, 14, 14, 0),
            ]
        );
    }

    #[test]
    fn bats_keeps_inline_redirects_pipelines_and_empty_bodies() {
        let source = r#"@test "redirect" { echo ok > "$file"; }
@test "pipe" { echo ok | grep ok; }
@test "and" { true && echo ok; }
@test "empty" { }
"#;
        let entries: Vec<_> = test_entries(source, Lang::Bash)
            .into_iter()
            .map(|entry| (entry.name, entry.start_line, entry.end_line))
            .collect();
        assert_eq!(
            entries,
            [
                ("redirect".into(), 1, 1),
                ("pipe".into(), 2, 2),
                ("and".into(), 3, 3),
                ("empty".into(), 4, 4),
            ]
        );
    }

    #[test]
    fn bats_ignores_incomplete_cases_and_ordinary_shell_commands() {
        let source = r#"echo '@test "not a case" {'
@test "no brace"
@test "unterminated" {
  run true
"#;
        assert_eq!(test_entries(source, Lang::Bash), []);
    }
}
