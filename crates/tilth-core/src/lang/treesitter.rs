//! Shared tree-sitter utilities used by symbol search and caller search.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Definition node kinds across tree-sitter grammars.
pub(crate) const DEFINITION_KINDS: &[&str] = &[
    // Functions
    "function_declaration",
    "function_definition",
    "function_item",
    "method_definition",
    "method_declaration",
    // Members declared without a body: trait and interface methods, abstract
    // methods, TS overloads and `declare function`. Each is the declaration
    // of its member, as a bodiless Java or C# `method_declaration` is.
    "function_signature_item",
    "method_signature",
    "abstract_method_signature",
    "function_signature",
    "protocol_function_declaration",
    // Classes, structs & Kotlin objects
    "class_declaration",
    "class_definition",
    "abstract_class_declaration",
    "struct_item",
    "union_item",
    "struct_declaration",
    "record_declaration",
    "annotation_type_declaration",
    "object_declaration",
    "object_definition",
    // Interfaces & types (TS)
    "interface_declaration",
    "trait_declaration",
    "trait_definition",
    "protocol_declaration",
    "type_alias_declaration",
    "type_item",
    // Enums
    "enum_item",
    "enum_declaration",
    // Variables, constants & properties (Kotlin, C#, Swift, Scala)
    "lexical_declaration",
    "variable_declaration",
    "variable_assignment", // Bash top-level assignments (bash-only today; a future grammar reusing this node kind would inherit definition_weight 60)
    "const_item",
    "const_declaration",
    "static_item",
    "property_declaration",
    "val_definition",
    "var_definition",
    // Rust-specific
    "trait_item",
    "impl_item",
    "mod_item",
    "macro_definition",
    // Namespaces and modules (C++, PHP, C#, TS, Ruby)
    "namespace_definition",
    "namespace_declaration",
    "file_scoped_namespace_declaration",
    "internal_module",
    "module",
    // Ruby. `class` is also a JS/TS class expression, which defines its name
    // the same way.
    "class",
    "method",
    "singleton_method",
    // Python
    "decorated_definition",
    // Go
    "type_declaration",
    // Exports
    "export_statement",
];

/// Nodes that wrap one declaration to add a modifier: `export`, Python
/// decorators, a TS `declare`. The declaration inside is the definition. The
/// wrapper is not a second one, and it adds no nesting.
pub const WRAPPER_KINDS: &[&str] = &[
    "export_statement",
    "decorated_definition",
    "ambient_declaration",
];

/// Type-like nodes whose body holds members: classes, structs, traits,
/// interfaces, impls, objects and modules. Their body adds no nesting of its
/// own, and a member's enclosing scope is qualified by the nearest one.
pub const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "class_definition",
    "abstract_class_declaration",
    "class",
    "class_specifier",
    "struct_specifier",
    "union_specifier",
    "struct_item",
    "struct_declaration",
    "record_declaration",
    "impl_item",
    "interface_declaration",
    "trait_item",
    "trait_declaration",
    "trait_definition",
    "protocol_declaration",
    "object_declaration",
    "object_definition",
    "companion_object",
    "type_declaration",
    "enum_item",
    "enum_declaration",
    "module",
    "mod_item",
    "internal_module",
    "namespace_definition",
    "namespace_declaration",
];

/// Nodes whose body opens a local scope: functions, methods, constructors,
/// closures and accessors, across every grammar. A variable declared inside
/// one is local, so it is not a definition.
pub const FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "function_definition",
    "function_item",
    "method_definition",
    "method_declaration",
    "method",
    "singleton_method",
    "constructor_declaration",
    "compact_constructor_declaration",
    "secondary_constructor",
    "init_declaration",
    "deinit_declaration",
    "arrow_function",
    "function_expression",
    "function",
    "generator_function",
    "generator_function_declaration",
    "func_literal",
    "closure_expression",
    "lambda",
    "lambda_expression",
    "lambda_literal",
    "anonymous_function",
    "anonymous_method_expression",
    "local_function_statement",
    "accessor_declaration",
    "getter",
    "setter",
    "computed_getter",
    "computed_setter",
    "class_static_block",
    "static_initializer",
    "anonymous_initializer",
];

/// Definition kinds that bind a variable, constant or property. Declared
/// inside a function body they are locals.
const BINDING_KINDS: &[&str] = &[
    "lexical_declaration",
    "variable_declaration",
    "variable_assignment",
    "const_item",
    "static_item",
    "property_declaration",
    "val_definition",
    "var_definition",
    "var_spec",
    "const_spec",
];

/// Whether definition `node` binds a variable local to a function: a binding
/// kind whose nearest enclosing function or type is a function.
#[must_use]
pub fn is_local_binding(node: tree_sitter::Node) -> bool {
    if !BINDING_KINDS.contains(&node.kind()) {
        return false;
    }
    let mut parent = node.parent();
    while let Some(p) = parent {
        if FUNCTION_KINDS.contains(&p.kind()) {
            return true;
        }
        if TYPE_KINDS.contains(&p.kind()) {
            return false;
        }
        parent = p.parent();
    }
    false
}

/// C and C++ type specifiers. With a body they define a type; without one
/// (`struct point *p`, `class Forward;`) they name a type defined elsewhere.
const C_TYPE_SPECIFIERS: &[&str] = &[
    "class_specifier",
    "struct_specifier",
    "union_specifier",
    "enum_specifier",
];

/// Whether `node` is a C/C++ type specifier that names an existing type rather
/// than defining one: `struct point` with no `{ ... }` body.
pub(crate) fn is_type_reference(node: tree_sitter::Node) -> bool {
    C_TYPE_SPECIFIERS.contains(&node.kind()) && node.child_by_field_name("body").is_none()
}

/// Nodes that group definitions without adding a scope of their own: C
/// preprocessor conditionals (an include guard wraps a whole header), C++
/// `extern "C" { ... }` blocks and `template <...>` prefixes. The definitions
/// inside count as if the wrapper were not there.
pub const PASS_THROUGH_KINDS: &[&str] = &[
    "preproc_if",
    "preproc_ifdef",
    "preproc_elif",
    "preproc_elifdef",
    "preproc_else",
    "linkage_specification",
    "template_declaration",
];

/// Namespace nodes (C++, C#, PHP, Rust `mod`, TS `namespace`, Ruby and TS
/// `module`). A namespace is a definition with a name, but it does not nest
/// its members the way a class does. Python's root node is also a `module`;
/// a root is never a namespace, so callers judge only nodes with a parent.
pub const NAMESPACE_KINDS: &[&str] = &[
    "namespace_declaration",
    "namespace_definition",
    "file_scoped_namespace_declaration",
    "mod_item",
    "internal_module",
    "module",
];

/// Whether `node` is one of `kinds`, or the body of one: its `body` field or
/// its `declaration_list` (`namespace geo { ... }`, `extern "C" { ... }`,
/// `module Shop ... end`). `parent` is `node`'s parent; a caller walking down
/// the tree passes the one it holds, since `Node::parent` walks from the root.
#[must_use]
pub fn is_or_body_of(
    node: tree_sitter::Node,
    parent: Option<tree_sitter::Node>,
    kinds: &[&str],
) -> bool {
    kinds.contains(&node.kind())
        || parent.is_some_and(|p| {
            kinds.contains(&p.kind())
                && (node.kind() == "declaration_list"
                    || p.child_by_field_name("body") == Some(node))
        })
}

/// Follow a declarator chain down to the node that holds the declared name.
///
/// A C/C++ declaration keeps its name at the bottom of a chain of declarators:
/// `int *make(void)` is a pointer declarator around a function declarator
/// around `make`. Reference and parenthesised declarators hold their inner
/// declarator as an unnamed child. In C++ the name can also sit behind a scope
/// (`Shape::sides`) or template arguments (`biggest<int>`); the declared name
/// is the last segment. JS, TS and Java `variable_declarator`s keep theirs in
/// a `name` field.
fn declared_name(mut node: tree_sitter::Node) -> tree_sitter::Node {
    loop {
        let kind = node.kind();
        let inner = if kind == "qualified_identifier" || kind == "template_function" {
            node.child_by_field_name("name")
        } else if kind.ends_with("declarator") {
            node.child_by_field_name("declarator")
                .or_else(|| node.child_by_field_name("name"))
                .or_else(|| node.named_child(0))
        } else {
            None
        };
        match inner {
            Some(next) => node = next,
            None => return node,
        }
    }
}

/// The text of a declared-name node. A C++ conversion operator
/// (`operator bool() const`) carries its parameter list as a declarator of its
/// own; its name is the text before that declarator.
fn name_text(node: tree_sitter::Node, lines: &[&str]) -> String {
    if node.kind() == "operator_cast" {
        if let Some(params) = node.child_by_field_name("declarator") {
            let start = node.start_position();
            let end = params.start_position();
            if start.row == end.row && start.row < lines.len() {
                if let Some(text) = lines[start.row].get(start.column..end.column) {
                    return text.trim_end().to_string();
                }
            }
        }
    }
    node_text_simple(node, lines, NodeTextMode::Full)
}

/// Extract the name defined by a tree-sitter definition node.
///
/// Walks standard field names (`name`, `identifier`, `declarator`), follows
/// declarator chains to the declared identifier, and handles export
/// statements and JS/TS/C# variable declarations. A C/C++ type specifier
/// without a body defines nothing, so it has no name.
#[must_use]
pub fn extract_definition_name(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    if is_type_reference(node) {
        return None;
    }

    // Try standard field names
    for field in &["name", "identifier", "declarator"] {
        if let Some(child) = node.child_by_field_name(field) {
            let text = name_text(declared_name(child), lines);
            if !text.is_empty() {
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

    // A Scala `val x = 1` binds a pattern; only a plain identifier names one
    // symbol (`val (a, b) = ...` binds a tuple).
    if let Some(pattern) = node.child_by_field_name("pattern") {
        if pattern.kind() == "identifier" {
            return Some(node_text_simple(pattern, lines, NodeTextMode::Full));
        }
    }

    // A Kotlin `property_declaration` and a PHP `const_declaration` name their
    // symbol in a child: a `variable_declaration` or a `const_element`, whose
    // first named child is the name.
    if node.kind() == "property_declaration" || node.kind() == "const_declaration" {
        let mut cursor = node.walk();
        let holder = node
            .children(&mut cursor)
            .find(|c| c.kind() == "variable_declaration" || c.kind() == "const_element");
        if let Some(name_node) = holder.and_then(|h| h.named_child(0)) {
            let text = node_text_simple(name_node, lines, NodeTextMode::Full);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    None
}

/// Controls how [`node_text_simple`] renders a node that spans multiple lines.
#[derive(Clone, Copy)]
pub enum NodeTextMode {
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
#[must_use]
pub fn node_text_simple(node: tree_sitter::Node, lines: &[&str], mode: NodeTextMode) -> String {
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
#[must_use]
pub fn extract_impl_trait(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let trait_node = node.child_by_field_name("trait")?;
    Some(node_text_simple(trait_node, lines, NodeTextMode::Full))
}

/// Extract implementing type from Rust `impl ... for Type` node.
#[must_use]
pub fn extract_impl_type(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let type_node = node.child_by_field_name("type")?;
    Some(node_text_simple(type_node, lines, NodeTextMode::Full))
}

/// Extract implemented interface names from TS/Java class declaration.
/// Walks `implements_clause` (TS) and `super_interfaces` (Java) children.
#[must_use]
pub fn extract_implemented_interfaces(node: tree_sitter::Node, lines: &[&str]) -> Vec<String> {
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
        | "method"
        | "singleton_method"
        | "class_declaration"
        | "class_definition"
        | "abstract_class_declaration"
        | "class"
        | "struct_item"
        | "union_item"
        | "struct_declaration"
        | "record_declaration"
        | "annotation_type_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "trait_definition"
        | "protocol_declaration"
        | "trait_item"
        | "enum_item"
        | "enum_declaration"
        | "type_item"
        | "type_declaration"
        | "type_spec"
        | "type_alias"
        | "class_specifier"
        | "struct_specifier"
        | "union_specifier"
        | "enum_specifier"
        | "type_definition"
        | "alias_declaration" => 100,
        "impl_item" | "object_declaration" | "object_definition" => 90,
        "const_item"
        | "const_declaration"
        | "const_spec"
        | "static_item"
        | "macro_definition"
        | "preproc_def"
        | "preproc_function_def" => 80,
        "mod_item"
        | "namespace_definition"
        | "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "internal_module"
        | "module"
        | "property_declaration"
        | "val_definition"
        | "var_definition" => 70,
        "variable_assignment" => 60,
        "lexical_declaration" | "variable_declaration" | "var_spec" => 40,
        // Bodiless member declarations (trait, interface and abstract
        // methods) rank below the bodies that implement them.
        _ => 50,
    }
}

/// Global cache of compiled tree-sitter queries, shared by symbol/caller
/// search (`search::siblings`) and per-language extractors (e.g. Go's
/// receiver-name lookup). Keyed by `(node_kind_count, field_count,
/// query_str_ptr)` so that distinct query strings for the same language are
/// stored under separate keys. We avoid `Language::name()` because ABI < 15
/// grammars (e.g. tree-sitter-kotlin-ng) return `None`.
#[allow(clippy::type_complexity)]
static QUERY_CACHE: LazyLock<Mutex<HashMap<(usize, usize, usize), tree_sitter::Query>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Look up or compile `query_str` for `ts_lang`, then invoke `f` with a
/// reference to the cached `Query`. Returns `None` if compilation fails.
///
/// `query_str` must be `'static` so its pointer address is stable across
/// calls and can serve as part of the cache key.
pub fn with_query<R>(
    ts_lang: &tree_sitter::Language,
    query_str: &'static str,
    f: impl FnOnce(&tree_sitter::Query) -> R,
) -> Option<R> {
    use std::collections::hash_map::Entry;
    // Pointer address distinguishes different queries for the same language.
    let key = (
        ts_lang.node_kind_count(),
        ts_lang.field_count(),
        query_str.as_ptr() as usize,
    );
    let mut cache = QUERY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let query = match cache.entry(key) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let q = tree_sitter::Query::new(ts_lang, query_str).ok()?;
            e.insert(q)
        }
    };
    Some(f(query))
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
    use crate::lang::outline::outline_language;
    use crate::types::Lang;

    #[test]
    fn definition_weight_covers_every_tier() {
        // 100 — primary declarations, one per source language shape
        // (Rust function_item/enum_item, TS class_declaration/interface_declaration,
        // TS abstract classes, Ruby methods and classes).
        assert_eq!(definition_weight("function_item"), 100);
        assert_eq!(definition_weight("class_declaration"), 100);
        assert_eq!(definition_weight("interface_declaration"), 100);
        assert_eq!(definition_weight("enum_item"), 100);
        assert_eq!(definition_weight("abstract_class_declaration"), 100);
        assert_eq!(definition_weight("method"), 100);
        assert_eq!(definition_weight("singleton_method"), 100);
        assert_eq!(definition_weight("class"), 100);
        // C/C++ types rank with the other languages' types.
        for kind in [
            "class_specifier",
            "struct_specifier",
            "union_specifier",
            "enum_specifier",
            "type_definition",
            "alias_declaration",
        ] {
            assert_eq!(definition_weight(kind), 100, "{kind}");
        }
        // Go specs rank with their counterparts: types, constants, variables.
        assert_eq!(definition_weight("type_spec"), 100);
        assert_eq!(definition_weight("type_alias"), 100);
        assert_eq!(definition_weight("const_spec"), 80);
        assert_eq!(definition_weight("var_spec"), 40);
        // 90 — impls / object-like declarations (Rust impl_item, Kotlin object_declaration).
        assert_eq!(definition_weight("impl_item"), 90);
        assert_eq!(definition_weight("object_declaration"), 90);
        // 80 — const/static.
        assert_eq!(definition_weight("const_item"), 80);
        assert_eq!(definition_weight("static_item"), 80);
        // 70 — module/namespace/property.
        assert_eq!(definition_weight("mod_item"), 70);
        assert_eq!(definition_weight("property_declaration"), 70);
        // 60 — Bash top-level assignment (special-cased above the 40 tier).
        assert_eq!(definition_weight("variable_assignment"), 60);
        // 40 — plain variable declarations (JS/TS lexical_declaration, C#/Kotlin variable_declaration).
        assert_eq!(definition_weight("lexical_declaration"), 40);
        assert_eq!(definition_weight("variable_declaration"), 40);
        // 80 — macros rank with constants.
        assert_eq!(definition_weight("macro_definition"), 80);
        assert_eq!(definition_weight("preproc_def"), 80);
        // 50 — unrecognized kind falls to the default tier, not 0.
        assert_eq!(definition_weight("comment"), 50);
    }

    /// Parse `src` with `lang`'s grammar and return the owned tree.
    fn parse(src: &str, lang: Lang) -> tree_sitter::Tree {
        let language = outline_language(lang).expect("grammar available for test language");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).expect("grammar loads");
        parser.parse(src, None).expect("parse succeeds")
    }

    /// Depth-first search for the first descendant node of the given kind.
    fn find_by_kind<'a>(root: tree_sitter::Node<'a>, kind: &str) -> tree_sitter::Node<'a> {
        let mut cursor = root.walk();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == kind {
                return node;
            }
            stack.extend(node.children(&mut cursor));
        }
        panic!("no {kind} node found in parsed tree");
    }

    #[test]
    fn extract_definition_name_rust_function_item() {
        let src = "fn greet(name: &str) -> String { name.to_string() }\n";
        let tree = parse(src, Lang::Rust);
        let lines: Vec<&str> = src.lines().collect();
        let node = find_by_kind(tree.root_node(), "function_item");
        assert_eq!(
            extract_definition_name(node, &lines),
            Some("greet".to_string())
        );
    }

    #[test]
    fn extract_definition_name_python_class_definition() {
        let src = "class Widget:\n    pass\n";
        let tree = parse(src, Lang::Python);
        let lines: Vec<&str> = src.lines().collect();
        let node = find_by_kind(tree.root_node(), "class_definition");
        assert_eq!(
            extract_definition_name(node, &lines),
            Some("Widget".to_string())
        );
    }

    #[test]
    fn extract_definition_name_unwraps_export_statement() {
        // export_statement has no "name"/"identifier"/"declarator" field of its
        // own — extract_definition_name must recurse into the wrapped
        // function_declaration to find the name (the node.kind() == "export_statement"
        // branch).
        let src = "export function handler() {}\n";
        let tree = parse(src, Lang::TypeScript);
        let lines: Vec<&str> = src.lines().collect();
        let node = find_by_kind(tree.root_node(), "export_statement");
        assert_eq!(
            extract_definition_name(node, &lines),
            Some("handler".to_string())
        );
    }

    #[test]
    fn extract_definition_name_walks_lexical_declaration_declarator() {
        // lexical_declaration stores its identifier inside a child
        // variable_declarator, not as a direct field on the declaration node —
        // exercises the dedicated child-walk branch.
        let src = "const total = 42;\n";
        let tree = parse(src, Lang::TypeScript);
        let lines: Vec<&str> = src.lines().collect();
        let node = find_by_kind(tree.root_node(), "lexical_declaration");
        assert_eq!(
            extract_definition_name(node, &lines),
            Some("total".to_string())
        );
    }

    #[test]
    fn extract_definition_name_returns_none_when_no_name_field_present() {
        // impl_item has no "name"/"identifier"/"declarator" field and isn't
        // handled by any of the special-cased branches — must fall through to
        // None rather than panic or return an empty string.
        let src = "impl Widget {}\n";
        let tree = parse(src, Lang::Rust);
        let lines: Vec<&str> = src.lines().collect();
        let node = find_by_kind(tree.root_node(), "impl_item");
        assert_eq!(extract_definition_name(node, &lines), None);
    }

    #[test]
    fn extract_definition_name_follows_c_family_declarators_to_the_name() {
        // A C/C++ function's name sits at the bottom of its declarator chain:
        // inside a function declarator, wrapped by pointer, reference or
        // parenthesised declarators, and in C++ behind a scope or template
        // arguments. The defined name is that bottom identifier, never the
        // declarator text with its parameter list.
        let cases = [
            (Lang::C, "int add(int a, int b) { return a + b; }\n", "add"),
            (Lang::C, "int *make(void) { return 0; }\n", "make"),
            (Lang::C, "int (*pick(void))(int) { return 0; }\n", "pick"),
            (Lang::Cpp, "int &ref(int &x) { return x; }\n", "ref"),
            (Lang::Cpp, "int Shape::sides() { return 4; }\n", "sides"),
            (Lang::Cpp, "void a::b::deep() {}\n", "deep"),
            (Lang::Cpp, "Shape::~Shape() {}\n", "~Shape"),
            (
                Lang::Cpp,
                "bool operator==(int a, int b) { return true; }\n",
                "operator==",
            ),
            (
                Lang::Cpp,
                "Shape::operator bool() const { return true; }\n",
                "operator bool",
            ),
            (
                Lang::Cpp,
                "template <> int biggest<int>(int a) { return a; }\n",
                "biggest",
            ),
        ];
        for (lang, src, expected) in cases {
            let tree = parse(src, lang);
            let lines: Vec<&str> = src.lines().collect();
            let node = find_by_kind(tree.root_node(), "function_definition");
            assert_eq!(
                extract_definition_name(node, &lines).as_deref(),
                Some(expected),
                "{src}"
            );
        }
    }

    #[test]
    fn extract_definition_name_skips_c_type_references() {
        // `struct point { ... }` defines `point`; a bare `struct point` names
        // an existing type and defines nothing. A typedef defines the name in
        // its declarator, even behind a function-pointer declarator.
        let src = "struct point { int x; };\nstruct point *p;\ntypedef int (*cmp_fn)(int);\n";
        let tree = parse(src, Lang::C);
        let lines: Vec<&str> = src.lines().collect();
        let root = tree.root_node();
        let definition = root.named_child(0).expect("struct definition");
        let reference = root
            .named_child(1)
            .and_then(|d| d.child_by_field_name("type"))
            .expect("struct reference");
        let typedef = root.named_child(2).expect("typedef");
        assert_eq!(
            extract_definition_name(definition, &lines).as_deref(),
            Some("point")
        );
        assert_eq!(extract_definition_name(reference, &lines), None);
        assert_eq!(
            extract_definition_name(typedef, &lines).as_deref(),
            Some("cmp_fn")
        );
    }
}
