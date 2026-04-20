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
            let text = node_text_simple(child, lines);
            if !text.is_empty() {
                // For variable_declarator, get the identifier inside
                if child.kind().contains("declarator") {
                    if let Some(id) = child.child_by_field_name("name") {
                        return Some(node_text_simple(id, lines));
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

    None
}

/// Get the text of a single-line node from pre-split source lines.
///
/// Returns the text slice for single-line nodes, or the text from the start
/// column to end-of-line for multi-line nodes.
pub(crate) fn node_text_simple(node: tree_sitter::Node, lines: &[&str]) -> String {
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

/// Extract trait name from Rust `impl Trait for Type` node.
/// Returns None for inherent impls (no trait).
pub(crate) fn extract_impl_trait(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let trait_node = node.child_by_field_name("trait")?;
    Some(node_text_simple(trait_node, lines))
}

/// Extract implementing type from Rust `impl ... for Type` node.
pub(crate) fn extract_impl_type(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let type_node = node.child_by_field_name("type")?;
    Some(node_text_simple(type_node, lines))
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
                    let text = node_text_simple(ident, lines);
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

/// Elixir call-node target identifiers that define named symbols.
/// This is the complete set used for definition detection in symbol search/index.
/// See also `ELIXIR_DEF_KEYWORDS` in `outline.rs` which is the subset of
/// function-like keywords (excludes container keywords like `defmodule`,
/// `defprotocol`, `defimpl`, `defstruct`, `defexception` that have their own
/// outline handling).
const ELIXIR_DEFINITION_TARGETS: &[&str] = &[
    "defmodule",
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defguard",
    "defguardp",
    "defdelegate",
    "defstruct",
    "defexception",
    "defprotocol",
    "defimpl",
];

/// Find the `arguments` child of an Elixir `call` node.
/// In tree-sitter-elixir, `arguments` is a node kind, not a named field,
/// so `child_by_field_name("arguments")` doesn't work.
pub(crate) fn elixir_arguments(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cursor = node.walk();
    // Node is Copy (arena index) — the returned node survives cursor drop.
    let result = node.children(&mut cursor).find(|c| c.kind() == "arguments");
    result
}

/// Check if a tree-sitter node is an Elixir definition.
/// In Elixir all definitions are `call` nodes whose `target` identifier
/// is one of `defmodule`, `def`, `defp`, etc.
pub(crate) fn is_elixir_definition(node: tree_sitter::Node, lines: &[&str]) -> bool {
    if node.kind() != "call" {
        return false;
    }
    let Some(target) = node.child_by_field_name("target") else {
        return false;
    };
    let kw = node_text_simple(target, lines);
    ELIXIR_DEFINITION_TARGETS.contains(&kw.as_str())
}

/// Extract the defined name from an Elixir definition `call` node.
///
/// - `defmodule Foo.Bar do...end` → `"Foo.Bar"`
/// - `def greet(name) do...end`  → `"greet"`
/// - `defstruct [:a, :b]`       → `"defstruct"`
pub(crate) fn extract_elixir_definition_name(
    node: tree_sitter::Node,
    lines: &[&str],
) -> Option<String> {
    let target = node.child_by_field_name("target")?;
    let kw = node_text_simple(target, lines);
    let args = elixir_arguments(node)?;

    match kw.as_str() {
        "defmodule" | "defprotocol" | "defimpl" => {
            // First named child of arguments is the module/protocol alias.
            // For `defimpl Printable, for: User`, this returns "Printable" (the
            // protocol name), not "User" (the implementing type). Searching for
            // the protocol name will find both the protocol and all its impls.
            let mut cursor = args.walk();
            for child in args.children(&mut cursor) {
                if child.is_named() {
                    return Some(node_text_simple(child, lines));
                }
            }
            None
        }
        "def" | "defp" | "defmacro" | "defmacrop" | "defguard" | "defguardp" | "defdelegate" => {
            // First named child is:
            //   `call`              — normal: `def greet(name)`
            //   `identifier`        — no-arg: `def bar, do: :ok`
            //   `binary_operator`   — guard:  `def foo(x) when x > 0`
            let mut cursor = args.walk();
            for child in args.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                return elixir_extract_func_head_name(child, lines);
            }
            None
        }
        // In Elixir, a struct IS its enclosing module (`%MyModule{}`), and only
        // one struct per module is allowed. There's no standalone struct name to
        // extract, so we index the keyword itself. Search for the struct by its
        // module name instead.
        "defstruct" | "defexception" => Some(kw.clone()),
        _ => None,
    }
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
            .map(|t| node_text_simple(t, lines)),
        "identifier" => Some(node_text_simple(node, lines)),
        "binary_operator" => {
            // Guard clause: `foo(x) when x > 0` → left is the function head
            let left = node.child_by_field_name("left")?;
            elixir_extract_func_head_name(left, lines)
        }
        _ => None,
    }
}

/// Semantic weight for Elixir definition keywords.
pub(crate) fn elixir_definition_weight(node: tree_sitter::Node, lines: &[&str]) -> u16 {
    let Some(target) = node.child_by_field_name("target") else {
        return 50;
    };
    let kw = node_text_simple(target, lines);
    match kw.as_str() {
        "defmodule" | "defprotocol" | "def" | "defp" | "defmacro" | "defmacrop" | "defguard"
        | "defguardp" | "defdelegate" => 100,
        "defimpl" => 90,
        "defstruct" | "defexception" => 80,
        _ => 50,
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
