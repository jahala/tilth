/// Markdown outline via tree-sitter-md. The block grammar emits `section`
/// nodes that group each heading with its content (and any nested sections),
/// so heading hierarchy + section spans drop out of the AST instead of
/// requiring a hand-rolled fence-aware ATX scan. Fenced code blocks are
/// `fenced_code_block` nodes and never produce false-positive headings.
use crate::lang::outline::{heading_level, heading_text, parse_markdown};

pub fn outline(buf: &[u8], max_lines: usize) -> String {
    let Ok(content) = std::str::from_utf8(buf) else {
        return String::new();
    };
    let Some(tree) = parse_markdown(content) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();

    let mut entries = Vec::new();
    let mut code_block_count = 0u32;
    walk(
        tree.root_node(),
        &lines,
        max_lines,
        &mut entries,
        &mut code_block_count,
    );

    if code_block_count > 0 {
        entries.push(format!("\n({code_block_count} code blocks)"));
    }
    entries.join("\n")
}

fn walk(
    node: tree_sitter::Node,
    lines: &[&str],
    max_lines: usize,
    entries: &mut Vec<String>,
    code_block_count: &mut u32,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if entries.len() >= max_lines {
            return;
        }
        match child.kind() {
            "section" => {
                emit_section_heading(child, lines, entries);
                walk(child, lines, max_lines, entries, code_block_count);
            }
            "fenced_code_block" => {
                *code_block_count += 1;
            }
            _ => walk(child, lines, max_lines, entries, code_block_count),
        }
    }
}

/// If `section` opens with an `atx_heading` or `setext_heading`, append an
/// entry of the form `[start-end] {indent}{hashes} {text}` to `entries`.
fn emit_section_heading(section: tree_sitter::Node, lines: &[&str], entries: &mut Vec<String>) {
    let mut cursor = section.walk();
    for inner in section.children(&mut cursor) {
        // Only ATX headings nest as proper sections in the block grammar;
        // setext headings sit as siblings inside one big document section,
        // so section-span computation doesn't apply. Preserve the old
        // hand-rolled scanner's behavior of silently ignoring setext.
        if inner.kind() != "atx_heading" {
            continue;
        }
        let Some(level) = heading_level(inner) else {
            return;
        };
        let start_line = (inner.start_position().row + 1) as u32;
        let end_line = section_end_line(section);
        let text = heading_text(inner, lines);
        let indent = "  ".repeat((level as usize).saturating_sub(1));
        let hashes = "#".repeat(level as usize);
        let display = if text.len() > 80 {
            format!("{}...", crate::types::truncate_str(&text, 77))
        } else {
            text
        };
        entries.push(format!(
            "[{start_line}-{end_line}] {indent}{hashes} {display}"
        ));
        return;
    }
}

/// Convert a section node's exclusive end position to a 1-indexed inclusive
/// line. Tree-sitter end positions point one past the last character — when
/// the section ends with a newline, end.column is 0 on the row after the
/// content; otherwise end.row is the row containing the last character.
fn section_end_line(section: tree_sitter::Node) -> u32 {
    let end = section.end_position();
    if end.column == 0 {
        end.row as u32
    } else {
        (end.row + 1) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_headings() {
        let input = b"# H1\nSome text\n## H2\nMore text\n";
        let result = outline(input, 100);
        let lines: Vec<&str> = result.lines().collect();

        assert_eq!(lines.len(), 2);
        // H1 extends to end of file (line 4) since no other H1
        assert_eq!(lines[0], "[1-4] # H1");
        // H2 also extends to end of file (line 4)
        assert_eq!(lines[1], "[3-4]   ## H2");
    }

    #[test]
    fn code_blocks_skipped() {
        let input = b"# Real Heading\n\n```\ncode\n```\n";
        let result = outline(input, 100);

        // Should only find the real heading, not any inside code block
        assert!(result.starts_with("[1-5] # Real Heading"));
        assert!(result.contains("(1 code blocks)"));
        assert!(!result.contains("Fake Heading"));
    }

    #[test]
    fn code_block_count() {
        let input = b"# Heading\n```\ncode\n```\n```\nmore\n```\n";
        let result = outline(input, 100);

        assert!(result.contains("(2 code blocks)"));
    }

    #[test]
    fn nested_heading_ranges() {
        let input = b"# A\ntext\n## B\ntext\n## C\ntext\n# D\ntext\n";
        let result = outline(input, 100);
        let lines: Vec<&str> = result.lines().collect();

        assert_eq!(lines.len(), 4);
        // A extends until D (line 7), so ends at line 6
        assert_eq!(lines[0], "[1-6] # A");
        // B extends until C (line 5), so ends at line 4
        assert_eq!(lines[1], "[3-4]   ## B");
        // C extends until D (line 7), so ends at line 6
        assert_eq!(lines[2], "[5-6]   ## C");
        // D extends to end of file (line 8)
        assert_eq!(lines[3], "[7-8] # D");
    }

    #[test]
    fn last_heading_to_eof() {
        let input = b"# Heading\nline 2\nline 3\nline 4\n";
        let result = outline(input, 100);

        // Heading should extend to line 4 (total line count)
        assert_eq!(result, "[1-4] # Heading");
    }

    #[test]
    fn empty_file() {
        let input = b"";
        let result = outline(input, 100);

        assert_eq!(result, "");
    }

    /// AST handles fenced code blocks at the parser level — a `# foo` Python
    /// comment inside a fenced block is part of the `fenced_code_block` node,
    /// not an `atx_heading`. The hand-rolled scanner needed a manual fence
    /// pre-pass to avoid treating it as a heading; the AST gets this for free.
    #[test]
    fn hash_inside_fenced_code_does_not_become_heading() {
        let input = b"# Real\n\n```python\n# fake heading\nprint('x')\n```\n\n## Also Real\n";
        let result = outline(input, 100);
        let lines: Vec<&str> = result.lines().collect();
        let heading_lines: Vec<&&str> = lines.iter().filter(|l| l.starts_with('[')).collect();
        assert_eq!(heading_lines.len(), 2);
        assert!(heading_lines[0].contains("# Real"));
        assert!(heading_lines[1].contains("## Also Real"));
    }

    /// Setext headings (`Top\n====`) are not handled — the block grammar puts
    /// every setext heading as a sibling inside one document-spanning section
    /// rather than nesting them, so section-span computation doesn't apply.
    /// The old hand-rolled scanner only matched ATX too; we preserve that.
    #[test]
    fn setext_headings_silently_ignored() {
        let input = b"Top\n===\n\ncontent\n";
        let result = outline(input, 100);
        assert_eq!(result, "");
    }
}
