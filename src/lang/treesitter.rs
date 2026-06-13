//! Shared tree-sitter utilities used by symbol search and caller search.

/// Definition node kinds across tree-sitter grammars.
pub(crate) const DEFINITION_KINDS: &[&str] = &[
    // Functions
    "function_declaration",
    "function_definition",
    "function_item",
    "method_definition",
    "method_declaration",
    // Classes, structs & Kotlin objects
    "class_declaration",
    "class_definition",
    "struct_item",
    "object_declaration",
    // Interfaces & types (TS)
    "interface_declaration",
    "trait_declaration",
    "type_alias_declaration",
    "type_item",
    // Enums
    "enum_item",
    "enum_declaration",
    // Variables, constants & properties (Kotlin, C#, Swift)
    "lexical_declaration",
    "variable_declaration",
    "const_item",
    "const_declaration",
    "static_item",
    "property_declaration",
    // Rust-specific
    "trait_item",
    "impl_item",
    "mod_item",
    "namespace_definition",
    // Python
    "decorated_definition",
    // Go
    "type_declaration",
    // Exports
    "export_statement",
];

/// Extract the name defined by a tree-sitter definition node.
///
/// Walks standard field names (`name`, `identifier`, `declarator`) and handles
/// nested declarators and export statements.
pub(crate) fn extract_definition_name(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    // Try standard field names
    for field in &["name", "identifier", "declarator"] {
        if let Some(child) = node.child_by_field_name(field) {
            let text = node_text_simple(child, lines, NodeTextMode::Full);
            if !text.is_empty() {
                // For variable_declarator, get the identifier inside
                if child.kind().contains("declarator") {
                    if let Some(id) = child.child_by_field_name("name") {
                        return Some(node_text_simple(id, lines, NodeTextMode::Full));
                    }
                }
                return Some(text);
            }
        }
    }

    // For export_statement, check the declaration child
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if DEFINITION_KINDS.contains(&child.kind()) {
                return extract_definition_name(child, lines);
            }
        }
    }

    // JS/TS `lexical_declaration` and C# `variable_declaration` store the
    // identifier inside a `variable_declarator` child (field "declarations" /
    // unnamed children), not as a direct named field on the declaration node.
    // Walk children to find the first `variable_declarator` and pull its `name`.
    if node.kind() == "lexical_declaration" || node.kind() == "variable_declaration" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let text = node_text_simple(name_node, lines, NodeTextMode::Full);
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
    }

    None
}

/// Controls how [`node_text_simple`] renders a node that spans multiple lines.
#[derive(Clone, Copy)]
pub(crate) enum NodeTextMode {
    /// Return the node's start line untruncated.
    Full,
    /// Truncate the node's start line to roughly 80 characters.
    Truncated,
}

/// Returns a node's text from pre-split source lines.
///
/// For a single-line node, returns its exact slice. For a multi-line node,
/// returns only the start line (start column to end-of-line) — never the full
/// multi-line span; `mode` then decides whether that start line is returned in
/// full or truncated.
pub(crate) fn node_text_simple(
    node: tree_sitter::Node,
    lines: &[&str],
    mode: NodeTextMode,
) -> String {
    let row = node.start_position().row;
    let col_start = node.start_position().column;
    let end_row = node.end_position().row;
    if row < lines.len() && row == end_row {
        let col_end = node.end_position().column.min(lines[row].len());
        lines[row][col_start..col_end].to_string()
    } else if row < lines.len() {
        let text = &lines[row][col_start..];
        match mode {
            NodeTextMode::Full => text.to_string(),
            NodeTextMode::Truncated => {
                if text.len() > 80 {
                    format!("{}...", crate::types::truncate_str(text, 77))
                } else {
                    text.to_string()
                }
            }
        }
    } else {
        String::new()
    }
}

/// Extract trait name from Rust `impl Trait for Type` node.
/// Returns None for inherent impls (no trait).
pub(crate) fn extract_impl_trait(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let trait_node = node.child_by_field_name("trait")?;
    Some(node_text_simple(trait_node, lines, NodeTextMode::Full))
}

/// Extract implementing type from Rust `impl ... for Type` node.
pub(crate) fn extract_impl_type(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let type_node = node.child_by_field_name("type")?;
    Some(node_text_simple(type_node, lines, NodeTextMode::Full))
}

/// Extract implemented interface names from TS/Java class declaration.
/// Walks `implements_clause` (TS) and `super_interfaces` (Java) children.
pub(crate) fn extract_implemented_interfaces(
    node: tree_sitter::Node,
    lines: &[&str],
) -> Vec<String> {
    let mut interfaces = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "implements_clause" || child.kind() == "super_interfaces" {
            let mut inner = child.walk();
            for ident in child.children(&mut inner) {
                if ident.kind().contains("identifier") {
                    let text = node_text_simple(ident, lines, NodeTextMode::Full);
                    if !text.is_empty() {
                        interfaces.push(text);
                    }
                }
            }
        }
    }
    interfaces
}

// ---------------------------------------------------------------------------
// Elixir-specific definition helpers
// ---------------------------------------------------------------------------

/// Find the `arguments` child of an Elixir `call` node.
/// In tree-sitter-elixir, `arguments` is a node kind, not a named field,
/// so `child_by_field_name("arguments")` doesn't work.
pub(crate) fn elixir_arguments(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cursor = node.walk();
    // Node is Copy (arena index) — the returned node survives cursor drop.
    let result = node.children(&mut cursor).find(|c| c.kind() == "arguments");
    result
}

/// Extract function name from the first argument of a `def`/`defp`/`defmacro` call.
///
/// The first argument can be:
/// - `call` node: `def greet(name)` → target is `greet`
/// - `identifier` node: `def bar, do: :ok` → text is `bar`
/// - `binary_operator` with `when`: `def foo(x) when x > 0` → unwrap left, then recurse
pub(crate) fn elixir_extract_func_head_name(
    node: tree_sitter::Node,
    lines: &[&str],
) -> Option<String> {
    match node.kind() {
        "call" => node
            .child_by_field_name("target")
            .map(|t| node_text_simple(t, lines, NodeTextMode::Full)),
        "identifier" => Some(node_text_simple(node, lines, NodeTextMode::Full)),
        "binary_operator" => {
            // Guard clause: `foo(x) when x > 0` → left is the function head
            let left = node.child_by_field_name("left")?;
            elixir_extract_func_head_name(left, lines)
        }
        _ => None,
    }
}

/// Semantic weight for definition kinds. Primary declarations rank highest.
pub(crate) fn definition_weight(kind: &str) -> u16 {
    match kind {
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "method_definition"
        | "method_declaration"
        | "class_declaration"
        | "class_definition"
        | "struct_item"
        | "interface_declaration"
        | "trait_declaration"
        | "trait_item"
        | "enum_item"
        | "enum_declaration"
        | "type_item"
        | "type_declaration"
        | "decorated_definition" => 100,
        "impl_item" | "object_declaration" => 90,
        "const_item" | "const_declaration" | "static_item" => 80,
        "mod_item" | "namespace_definition" | "property_declaration" => 70,
        "lexical_declaration" | "variable_declaration" => 40,
        "export_statement" => 30,
        _ => 50,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_text_mode_controls_multiline_truncation() {
        let first_line =
            "pub fn very_long_function_name_with_enough_characters_to_force_truncation_for_outline_test() {";
        let source = format!("{first_line}\n}}\n");
        let lines: Vec<&str> = source.lines().collect();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("rust parser should load");
        let tree = parser
            .parse(&source, None)
            .expect("rust source should parse");
        let node = tree
            .root_node()
            .named_child(0)
            .expect("source should contain a function node");

        assert_eq!(
            node_text_simple(node, &lines, NodeTextMode::Full),
            first_line
        );
        assert_eq!(
            node_text_simple(node, &lines, NodeTextMode::Truncated),
            format!("{}...", &first_line[..77])
        );
    }
}
